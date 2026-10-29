//! Lado que controla: se conecta a un host, recibe el video, lo decodifica y envía la entrada.

use eframe::egui;
use lcv_codec::H264Decoder;
use crate::plugins::{self, Channel};
use lcv_proto::{
    AUDIO_PROTOCOL_VERSION, CURSOR_PROTOCOL_VERSION, CursorShape, DEFAULT_PORT, DisplayInfo, MIN_PROTOCOL_VERSION, Message, PLUGIN_ACCESS_DENIED, PLUGIN_NOT_ALLOWED,
    PLUGIN_NOT_INSTALLED, PLUGIN_NOT_RESPONDING, PLUGIN_PROTOCOL_VERSION, PROTOCOL_VERSION, PluginInfo, REJECT_VERSION,
    read_message, write_message,
};
use std::collections::HashMap;
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub enum Status {
    Connecting,
    /// El host pide contraseña y no hay una guardada: la interfaz debe solicitarla.
    NeedPassword,
    WaitingAcceptance,
    Streaming,
    Closed(String),
}

/// Estado de un complemento en esta sesión (sin entrada = cerrado).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// Esperando a que el equipo remoto abra su lado.
    Opening,
    /// El remoto está listo; abriendo el panel en este equipo.
    Launching,
    Open,
    Error(String),
}

pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
    pub received: Instant,
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub fps: f32,
    pub mbps: f32,
    pub rtt_ms: f32,
    pub host_ms: f32,
    pub decode_ms: f32,
    pub width: u32,
    pub height: u32,
}

pub struct Shared {
    pub status: Status,
    pub frame: Option<RgbaFrame>,
    pub stats: Stats,
    pub displays: Vec<DisplayInfo>,
    pub current_display: u32,
    /// Identidad del host (llega en `Accepted`); se usa para guardarlo en "Recientes".
    pub host_id: String,
    pub host_name: String,
    /// El host habla una versión con audio (se sabe al aceptar la conexión).
    pub audio_supported: bool,
    /// Miniaturas de monitores recién llegadas; la interfaz las recoge y las convierte en texturas.
    pub previews: Vec<lcv_proto::DisplayPreview>,
    /// El host habla una versión con complementos (v4).
    pub plugin_supported: bool,
    /// Complementos instalados en el equipo remoto (llega tras aceptar la conexión).
    pub remote_plugins: Option<Vec<PluginInfo>>,
    /// Estado de cada complemento por id.
    pub plugin_state: HashMap<String, PluginState>,
    /// El host envía el puntero aparte de la imagen (v5): la interfaz lo dibuja encima.
    pub cursor_supported: bool,
    /// Último puntero del host.
    pub cursor: Option<RemoteCursor>,
    /// Formas de puntero recién llegadas; la interfaz las convierte en texturas.
    pub cursor_shapes: Vec<CursorShape>,
}

/// Puntero del equipo remoto: su punto activo en píxeles del monitor que se ve y su forma (`id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteCursor {
    pub id: u64,
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}

type Channels = Arc<Mutex<HashMap<u32, Arc<Channel>>>>;

pub struct Viewer {
    pub target: String,
    /// Nombre visible del equipo (para mostrarlo al pedir la contraseña).
    pub display_name: String,
    shared: Arc<Mutex<Shared>>,
    stop: Arc<AtomicBool>,
    /// Único punto de escritura hacia el host: lo comparten el visor, el "ping" y la interfaz.
    writer: Arc<Mutex<Option<TcpStream>>>,
    /// La interfaz entrega aquí la contraseña escrita (o `None` al cancelar).
    pw_tx: std::sync::mpsc::Sender<Option<(String, bool)>>,
    /// Id de esta sesión como dueña del canal de teclas del hook.
    key_owner: u64,
    /// Reproducir el sonido del equipo remoto (botón 🔊/🔇).
    audio_on: Arc<AtomicBool>,
    /// Última petición de miniatura por monitor (para no pedirlas en cada fotograma).
    preview_asked: Mutex<std::collections::HashMap<u32, Instant>>,
    /// Complementos abiertos, por canal.
    channels: Channels,
    next_channel: AtomicU32,
    ctx: egui::Context,
}

impl Viewer {
    pub fn connect(target: String, display_name: String, initial_password: Option<String>, ctx: egui::Context) -> Self {
        let shared = Arc::new(Mutex::new(Shared {
            status: Status::Connecting,
            frame: None,
            stats: Stats::default(),
            displays: Vec::new(),
            current_display: 0,
            host_id: String::new(),
            host_name: String::new(),
            audio_supported: false,
            previews: Vec::new(),
            plugin_supported: false,
            remote_plugins: None,
            plugin_state: HashMap::new(),
            cursor_supported: false,
            cursor: None,
            cursor_shapes: Vec::new(),
        }));
        let channels: Channels = Default::default();
        let stop = Arc::new(AtomicBool::new(false));
        let audio_on = Arc::new(AtomicBool::new(crate::settings::load_audio()));
        let writer = Arc::new(Mutex::new(None));

        // Canal del hook de teclado → socket. Un hilo dedicado escribe las teclas capturadas para que
        // la función del hook (que debe ser muy rápida) nunca bloquee escribiendo en la red.
        let (key_tx, key_rx) = std::sync::mpsc::channel::<Message>();
        // Esta sesión pasa a ser la dueña del canal; al cerrarse solo puede retirar el suyo.
        let key_owner = crate::keyhook::new_owner_id();
        crate::keyhook::set_sink(key_owner, key_tx);
        {
            let (wr, st) = (writer.clone(), stop.clone());
            thread::Builder::new()
                .name("viewer-keys".into())
                .spawn(move || {
                    for msg in key_rx {
                        if st.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Message::Key { scancode, down, .. } = &msg {
                            if *scancode == 0x5B || *scancode == 0x5C {
                                info!("reenviada tecla Windows (down={down})");
                            }
                        }
                        match lock(&wr).as_mut() {
                            Some(s) => {
                                if let Err(e) = write_message(s, &msg) {
                                    warn!("no se pudo enviar una tecla al host: {e}");
                                }
                            }
                            None => warn!("tecla capturada antes de tener conexión; descartada"),
                        }
                    }
                })
                .ok();
        }

        let (pw_tx, pw_rx) = std::sync::mpsc::channel::<Option<(String, bool)>>();

        let (sh, st, wr, t, au, chs) =
            (shared.clone(), stop.clone(), writer.clone(), target.clone(), audio_on.clone(), channels.clone());
        let ui_ctx = ctx.clone();
        thread::Builder::new()
            .name("viewer".into())
            .spawn(move || {
                let result = run(&t, initial_password, pw_rx, &sh, &st, &wr, &au, &chs, &ctx);
                // Sin sesión no hay túnel: se cierran las tuberías (el panel del complemento lo nota).
                for (_, ch) in lock(&chs).drain() {
                    ch.close();
                }
                lock(&sh).plugin_state.clear();
                let reason = match result {
                    _ if st.load(Ordering::Relaxed) => "Desconectado".to_string(),
                    Ok(reason) => reason,
                    Err(e) => format!("Conexión perdida: {e}"),
                };
                info!("visor: {reason}");
                lock(&sh).status = Status::Closed(reason);
                ctx.request_repaint();
            })
            .ok();
        Self {
            target,
            display_name,
            shared,
            stop,
            writer,
            pw_tx,
            key_owner,
            audio_on,
            preview_asked: Mutex::new(Default::default()),
            channels,
            next_channel: AtomicU32::new(1),
            ctx: ui_ctx,
        }
    }

    /// Abre un complemento: primero su lado en el equipo remoto (`PluginOpen`); cuando confirma, el
    /// panel en este equipo (ver `launch_local_panel`).
    pub fn open_plugin(&self, id: &str) {
        {
            let mut sh = self.shared();
            if matches!(sh.plugin_state.get(id), Some(PluginState::Opening | PluginState::Launching | PluginState::Open)) {
                return;
            }
            sh.plugin_state.insert(id.into(), PluginState::Opening);
        }
        let channel = self.next_channel.fetch_add(1, Ordering::Relaxed);
        lock(&self.channels).insert(channel, Channel::new(channel, id));
        info!("complemento {id}: pidiendo al equipo remoto que lo abra (canal {channel})");
        self.send(&Message::PluginOpen { id: id.into(), channel });
    }

    /// Cierra el complemento en los dos equipos.
    pub fn close_plugin(&self, id: &str) {
        let ids: Vec<u32> = lock(&self.channels).iter().filter(|(_, c)| c.plugin == id).map(|(k, _)| *k).collect();
        for channel in ids {
            if let Some(ch) = lock(&self.channels).remove(&channel) {
                ch.close();
                self.send(&Message::PluginClose { channel });
            }
        }
        self.shared().plugin_state.remove(id);
        self.ctx.request_repaint();
    }

    /// Pide al host una miniatura del monitor `index`, como mucho una vez cada 1,5 s por monitor
    /// (se llama en cada fotograma mientras el cursor está sobre su botón). Solo con hosts v3.
    pub fn request_preview(&self, index: u32) {
        if !self.shared().audio_supported {
            return;
        }
        let mut asked = lock(&self.preview_asked);
        if asked.get(&index).is_some_and(|t| t.elapsed() < Duration::from_millis(1500)) {
            return;
        }
        asked.insert(index, Instant::now());
        drop(asked);
        self.send(&Message::PreviewRequest { index });
    }

    /// ¿Se está reproduciendo el sonido remoto?
    pub fn audio_on(&self) -> bool {
        self.audio_on.load(Ordering::Relaxed)
    }

    /// Activa o silencia el sonido remoto. Con el host compatible, también le pide que deje de
    /// capturar (no gasta red ni CPU mientras está silenciado). Se recuerda para próximas sesiones.
    pub fn set_audio(&self, on: bool) {
        self.audio_on.store(on, Ordering::Relaxed);
        crate::settings::save_audio(on);
        if self.shared().audio_supported {
            self.send(&Message::AudioEnable { on });
        }
    }

    /// La interfaz entrega la contraseña escrita por el usuario (y si debe recordarse).
    pub fn provide_password(&self, password: String, remember: bool) {
        let _ = self.pw_tx.send(Some((password, remember)));
    }

    /// El usuario canceló el cuadro de contraseña.
    pub fn cancel_password(&self) {
        let _ = self.pw_tx.send(None);
        self.disconnect();
    }

    pub fn shared(&self) -> MutexGuard<'_, Shared> {
        lock(&self.shared)
    }

    /// Envía un mensaje al host (entrada, cambio de monitor). Silencioso si la conexión ya cayó.
    pub fn send(&self, msg: &Message) {
        if let Some(s) = lock(&self.writer).as_mut() {
            let _ = write_message(s, msg);
        }
    }

    pub fn select_display(&self, index: u32) {
        self.send(&Message::SelectDisplay { index });
    }

    pub fn disconnect(&self) {
        self.stop.store(true, Ordering::Relaxed);
        for (_, ch) in lock(&self.channels).drain() {
            ch.close();
        }
        crate::keyhook::clear_sink(self.key_owner);
        if let Some(s) = lock(&self.writer).as_ref() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[allow(clippy::too_many_arguments)]
fn run(
    target: &str,
    initial_password: Option<String>,
    pw_rx: std::sync::mpsc::Receiver<Option<(String, bool)>>,
    shared: &Arc<Mutex<Shared>>,
    stop: &Arc<AtomicBool>,
    writer: &Arc<Mutex<Option<TcpStream>>>,
    audio_on: &AtomicBool,
    channels: &Channels,
    ctx: &egui::Context,
) -> std::io::Result<String> {
    let target = if target.contains(':') { target.to_string() } else { format!("{target}:{DEFAULT_PORT}") };
    let addr = target
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "dirección no válida"))?;
    // Se empieza con la versión más nueva; un host antiguo la rechaza y se reintenta con la mínima.
    let mut version = PROTOCOL_VERSION;
    let mut stream = open(&addr, version, writer)?;
    if stop.load(Ordering::Relaxed) {
        return Ok("Conexión cancelada".into());
    }
    lock(shared).status = Status::WaitingAcceptance;
    ctx.request_repaint();

    let clock = Instant::now();
    let mut decoder = H264Decoder::new().map_err(std::io::Error::other)?;
    let mut last_keyframe_request: Option<Instant> = None;
    let mut window = Window::new();
    // Contraseña a usar al llegar el reto (auto-login si venía guardada) y, si procede, a recordar.
    let mut pending_password = initial_password;
    let mut remember_after: Option<String> = None;
    // Reproductor del sonido remoto: se crea al llegar el primer trozo y se suelta al silenciar.
    let mut player: Option<lcv_audio::Player> = None;

    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok("Conexión cancelada".into());
        }
        let msg = match read_message(&mut stream) {
            Ok(m) => m,
            // Sin datos aún (esperando que acepten): volver a comprobar si se canceló.
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(e) => return Err(e),
        };
        match msg {
            Message::AuthChallenge { nonce } => {
                // Si no hay contraseña (ni guardada ni entregada), la interfaz la solicita y esperamos.
                let password = match pending_password.take() {
                    Some(p) => p,
                    None => {
                        lock(shared).status = Status::NeedPassword;
                        ctx.request_repaint();
                        let (pw, remember) = loop {
                            if stop.load(Ordering::Relaxed) {
                                return Ok("Desconectado".into());
                            }
                            match pw_rx.recv_timeout(Duration::from_millis(250)) {
                                Ok(Some(v)) => break v,
                                Ok(None) => return Ok("Conexión cancelada".into()),
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                    return Ok("Conexión cancelada".into());
                                }
                            }
                        };
                        lock(shared).status = Status::WaitingAcceptance;
                        ctx.request_repaint();
                        if remember {
                            remember_after = Some(pw.clone());
                        }
                        pw
                    }
                };
                let mac = crate::auth::hmac_sha256(password.as_bytes(), &nonce).to_vec();
                if let Some(s) = lock(writer).as_mut() {
                    write_message(s, &Message::AuthResponse { mac })?;
                }
            }
            Message::Accepted { host_id, host_name, displays, current } => {
                info!("aceptado por «{host_name}»; {} monitor(es), activo {current}", displays.len());
                // Si el usuario pidió recordar la contraseña, la guardamos para este equipo.
                if let Some(pw) = remember_after.take() {
                    crate::auth::remember_password(&host_id, &pw);
                }
                // Ya en sesión: lecturas bloqueantes (desconectar cierra el socket y las despierta).
                stream.set_read_timeout(None)?;
                let mut sh = lock(shared);
                sh.status = Status::Streaming;
                sh.displays = displays;
                sh.current_display = current;
                sh.host_id = host_id;
                sh.host_name = host_name;
                sh.audio_supported = version >= AUDIO_PROTOCOL_VERSION;
                sh.plugin_supported = version >= PLUGIN_PROTOCOL_VERSION;
                sh.cursor_supported = version >= CURSOR_PROTOCOL_VERSION;
                drop(sh);
                if version >= PLUGIN_PROTOCOL_VERSION
                    && let Some(s) = lock(writer).as_mut()
                {
                    write_message(s, &Message::PluginQuery)?;
                }
                if version >= AUDIO_PROTOCOL_VERSION {
                    let on = audio_on.load(Ordering::Relaxed);
                    info!("el host admite audio ({})", if on { "activado" } else { "silenciado" });
                    if let Some(s) = lock(writer).as_mut() {
                        write_message(s, &Message::AudioEnable { on })?;
                    }
                } else {
                    info!("el host usa una versión sin audio");
                }
                spawn_pinger(writer.clone(), stop.clone(), clock);
                ctx.request_repaint();
            }
            Message::Displays { displays, current } => {
                let mut sh = lock(shared);
                sh.displays = displays;
                sh.current_display = current;
            }
            // Host antiguo que no conoce esta versión: se reintenta con la anterior (5 → 4 → 3 → 2). El
            // rechazo por versión llega antes de pedir contraseña o aceptación, así que no se repiten.
            Message::Rejected { reason } if reason == REJECT_VERSION && version > MIN_PROTOCOL_VERSION => {
                info!("el host no admite la versión {version} del protocolo; reintentando con la {}", version - 1);
                version -= 1;
                stream = open(&addr, version, writer)?;
            }
            Message::Rejected { reason } => return Ok(reason),
            Message::Preview(p) => {
                let mut sh = lock(shared);
                sh.previews.retain(|q| q.index != p.index);
                sh.previews.push(p);
                drop(sh);
                ctx.request_repaint();
            }
            Message::Audio(a) => {
                if audio_on.load(Ordering::Relaxed) {
                    let chunk = lcv_audio::Chunk::from_le_bytes(a.sample_rate, a.channels, &a.pcm);
                    player.get_or_insert_with(lcv_audio::Player::start).push(chunk);
                } else {
                    player = None;
                }
            }
            Message::PluginList { items } => {
                info!("complementos del equipo remoto: {items:?}");
                lock(shared).remote_plugins = Some(items);
                ctx.request_repaint();
            }
            Message::PluginOpened { channel } => {
                let ch = lock(channels).get(&channel).cloned();
                match ch {
                    Some(ch) => {
                        info!("complemento {}: abierto en el equipo remoto (canal {channel})", ch.plugin);
                        launch_local_panel(ch, shared.clone(), channels.clone(), writer.clone(), ctx.clone());
                    }
                    // Se cerró aquí mientras el remoto lo abría: que lo cierre también.
                    None => {
                        if let Some(s) = lock(writer).as_mut() {
                            write_message(s, &Message::PluginClose { channel })?;
                        }
                    }
                }
            }
            Message::PluginError { channel, reason } => {
                if let Some(ch) = lock(channels).remove(&channel) {
                    warn!("complemento {}: el equipo remoto no lo pudo abrir ({reason})", ch.plugin);
                    ch.close();
                    lock(shared).plugin_state.insert(ch.plugin.clone(), PluginState::Error(remote_error_text(&reason)));
                    ctx.request_repaint();
                }
            }
            Message::PluginData { channel, data } => {
                if let Some(ch) = lock(channels).get(&channel) {
                    ch.push(data);
                }
            }
            Message::PluginClose { channel } => {
                if let Some(ch) = lock(channels).remove(&channel) {
                    info!("complemento {}: el equipo remoto lo cerró (canal {channel})", ch.plugin);
                    ch.close();
                    lock(shared).plugin_state.remove(&ch.plugin);
                    ctx.request_repaint();
                }
            }
            Message::CursorShape(shape) => {
                lock(shared).cursor_shapes.push(shape);
                ctx.request_repaint();
            }
            Message::CursorPos { id, x, y, visible } => {
                lock(shared).cursor = Some(RemoteCursor { id, x, y, visible });
                ctx.request_repaint();
            }
            Message::Pong { t_us } => {
                let rtt = (clock.elapsed().as_micros() as u64).saturating_sub(t_us) as f32 / 1000.0;
                lock(shared).stats.rtt_ms = rtt;
            }
            Message::VideoFrame(f) => {
                let received = Instant::now();
                window.add(f.data.len(), f.host_us);
                match decoder.decode(&f.data) {
                    Ok(Some(d)) => {
                        let frame = RgbaFrame { width: d.width, height: d.height, pixels: d.rgba.to_vec(), received };
                        let decode_ms = received.elapsed().as_secs_f32() * 1000.0;
                        let mut sh = lock(shared);
                        sh.frame = Some(frame);
                        sh.stats.decode_ms = ema(sh.stats.decode_ms, decode_ms);
                        sh.stats.width = f.width;
                        sh.stats.height = f.height;
                        if let Some((fps, mbps, host_ms)) = window.take_if_elapsed() {
                            (sh.stats.fps, sh.stats.mbps, sh.stats.host_ms) = (fps, mbps, host_ms);
                        }
                        drop(sh);
                        ctx.request_repaint();
                    }
                    Ok(None) => {}
                    Err(e) => {
                        if last_keyframe_request.is_none_or(|t| t.elapsed() > Duration::from_millis(500)) {
                            warn!("error de decodificación ({e}); pidiendo fotograma clave");
                            if let Some(s) = lock(writer).as_mut() {
                                write_message(s, &Message::RequestKeyframe)?;
                            }
                            last_keyframe_request = Some(Instant::now());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Texto para el usuario de un `PluginError` del equipo remoto.
fn remote_error_text(reason: &str) -> String {
    match reason {
        PLUGIN_NOT_INSTALLED => "IGP_Soft no está instalado en el equipo remoto.".into(),
        PLUGIN_NOT_RESPONDING => "IGP_Soft no respondió en el equipo remoto (10 s).".into(),
        PLUGIN_ACCESS_DENIED => {
            "El equipo remoto rechazó la conexión con IGP_Soft. ¿Se está ejecutando como administrador? \
             Ábrelo sin elevar, igual que LCVDesk."
                .into()
        }
        PLUGIN_NOT_ALLOWED => "El equipo remoto tiene los complementos desactivados en Ajustes.".into(),
        other => format!("El equipo remoto no pudo abrir el complemento ({other})."),
    }
}

/// El equipo remoto ya abrió su lado: crea la tubería local, abre el panel de IGP_Soft en modo
/// remoto apuntando a ella y, cuando se conecta, empieza a copiar en los dos sentidos.
fn launch_local_panel(
    ch: Arc<Channel>,
    shared: Arc<Mutex<Shared>>,
    channels: Channels,
    writer: Arc<Mutex<Option<TcpStream>>>,
    ctx: egui::Context,
) {
    thread::Builder::new()
        .name("viewer-plugin-open".into())
        .spawn(move || {
            let plugin = ch.plugin.clone();
            let set = |state: Option<PluginState>| {
                let mut sh = lock(&shared);
                match state {
                    Some(s) => sh.plugin_state.insert(plugin.clone(), s),
                    None => sh.plugin_state.remove(&plugin),
                };
                drop(sh);
                ctx.request_repaint();
            };
            let send = {
                let writer = writer.clone();
                move |m: Message| lock(&writer).as_mut().is_some_and(|s| write_message(s, &m).is_ok())
            };
            let abort = |text: String| {
                warn!("complemento {plugin}: {text}");
                if lock(&channels).remove(&ch.id).is_some() {
                    ch.close();
                    send(Message::PluginClose { channel: ch.id });
                }
                set(Some(PluginState::Error(text)));
            };
            set(Some(PluginState::Launching));
            let Some(local) = plugins::detect(&plugin) else {
                return abort("IGP_Soft no está instalado en este equipo.".into());
            };
            let name = plugins::random_pipe_name();
            let pipe = match plugins::Pipe::create_server(&name) {
                Ok(p) => p,
                Err(e) => return abort(format!("No se pudo preparar la conexión local ({e}).")),
            };
            if let Err(e) = plugins::launch(&local.exe, &["--remoto".into(), format!("--tubo={name}")]) {
                return abort(format!("No se pudo abrir IGP_Soft ({e})."));
            }
            if let Err(e) = pipe.accept(plugins::CONNECT_TIMEOUT, ch.cancel_flag()) {
                if ch.is_closed() {
                    return; // Se cerró a mano o terminó la sesión mientras tanto.
                }
                return abort(format!("IGP_Soft no se conectó en 10 s ({e})."));
            }
            info!("complemento {plugin} (canal {}): panel remoto conectado a {name}", ch.id);
            set(Some(PluginState::Open));
            let (chs, sh, ctx2, id, plugin2) = (channels.clone(), shared.clone(), ctx.clone(), ch.id, plugin.clone());
            ch.start(pipe, send, move || {
                // El panel se cerró (o se cortó el túnel): vuelve a quedar disponible.
                lock(&chs).remove(&id);
                let mut sh = lock(&sh);
                if sh.plugin_state.get(&plugin2) == Some(&PluginState::Open) {
                    sh.plugin_state.remove(&plugin2);
                }
                drop(sh);
                ctx2.request_repaint();
            });
        })
        .ok();
}

/// Abre la conexión TCP, la deja como punto de escritura y saluda con `version`.
fn open(addr: &SocketAddr, version: u32, writer: &Arc<Mutex<Option<TcpStream>>>) -> std::io::Result<TcpStream> {
    let stream = TcpStream::connect_timeout(addr, Duration::from_secs(3))?;
    stream.set_nodelay(true)?;
    // Hasta que el host acepte, las lecturas despiertan cada poco para atender "Cancelar" (si se
    // pulsó mientras aún se conectaba, todavía no había socket que cerrar).
    stream.set_read_timeout(Some(Duration::from_millis(300)))?;
    *lock(writer) = Some(stream.try_clone()?);
    let name = hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_else(|_| "Equipo".into());
    if let Some(s) = lock(writer).as_mut() {
        write_message(s, &Message::Hello { version, name })?;
    }
    Ok(stream)
}

fn spawn_pinger(writer: Arc<Mutex<Option<TcpStream>>>, stop: Arc<AtomicBool>, clock: Instant) {
    thread::Builder::new()
        .name("viewer-ping".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let t_us = clock.elapsed().as_micros() as u64;
                let ok = match writer.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                    Some(s) => write_message(s, &Message::Ping { t_us }).is_ok(),
                    None => false,
                };
                if !ok {
                    break;
                }
                thread::sleep(Duration::from_secs(1));
            }
        })
        .ok();
}

fn ema(prev: f32, sample: f32) -> f32 {
    if prev == 0.0 { sample } else { prev * 0.9 + sample * 0.1 }
}

struct Window {
    start: Instant,
    frames: u32,
    bytes: usize,
    host_us: u64,
}

impl Window {
    fn new() -> Self {
        Self { start: Instant::now(), frames: 0, bytes: 0, host_us: 0 }
    }

    fn add(&mut self, bytes: usize, host_us: u32) {
        self.frames += 1;
        self.bytes += bytes;
        self.host_us += host_us as u64;
    }

    fn take_if_elapsed(&mut self) -> Option<(f32, f32, f32)> {
        let secs = self.start.elapsed().as_secs_f32();
        if secs < 1.0 || self.frames == 0 {
            return None;
        }
        let r = (
            self.frames as f32 / secs,
            self.bytes as f32 * 8.0 / secs / 1_000_000.0,
            self.host_us as f32 / self.frames as f32 / 1000.0,
        );
        *self = Self::new();
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sesión real host ↔ visor en este equipo (127.0.0.1): con la versión 5, el puntero llega
    /// aparte del vídeo (su forma y su posición). Mueve el ratón un instante y lo devuelve a su sitio.
    /// `cargo test -p lcvdesk cursor_through_session -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn cursor_through_session() {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

        let _ = tracing_subscriber::fmt().with_test_writer().with_thread_names(true).try_init();
        let port = 47_172;
        let host = crate::host::Host::start(port);
        thread::sleep(Duration::from_millis(500));
        let h = host.clone();
        thread::spawn(move || {
            for _ in 0..100 {
                if h.state().pending.is_some() {
                    h.answer(true);
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
        let viewer = Viewer::connect(format!("127.0.0.1:{port}"), "prueba".into(), None, egui::Context::default());
        let wait = |what: &str, ok: &dyn Fn() -> bool| {
            let t = Instant::now();
            while !ok() {
                assert!(t.elapsed() < Duration::from_secs(20), "tiempo agotado esperando: {what}");
                thread::sleep(Duration::from_millis(50));
            }
            println!("ok: {what} ({:.2} s)", t.elapsed().as_secs_f32());
        };
        wait("sesión aceptada", &|| matches!(viewer.shared().status, Status::Streaming));
        assert!(viewer.shared().cursor_supported, "la sesión debería hablar la versión 5");

        let mut original = POINT::default();
        unsafe { GetCursorPos(&mut original) }.unwrap();
        let (x, y) = (200, 150);
        unsafe { SetCursorPos(x, y) }.unwrap();
        wait("posición del puntero", &|| viewer.shared().cursor.is_some_and(|c| c.visible && (c.x, c.y) == (x, y)));
        unsafe { SetCursorPos(original.x, original.y) }.unwrap();

        let sh = viewer.shared();
        let id = sh.cursor.unwrap().id;
        let shape = sh.cursor_shapes.iter().find(|s| s.id == id).expect("la forma del puntero actual");
        println!("puntero {id:#x}: {}x{}, punto activo {},{}", shape.width, shape.height, shape.hot_x, shape.hot_y);
        assert_eq!(shape.rgba.len(), (shape.width * shape.height * 4) as usize);
        drop(sh);
        viewer.disconnect();
        thread::sleep(Duration::from_millis(500));
    }
}

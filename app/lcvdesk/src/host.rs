//! Lado controlado: escucha conexiones, pide permiso al usuario y transmite la pantalla.

use anyhow::{Context, bail};
use lcv_capture::Capturer;
use lcv_codec::H264Encoder;
use lcv_input::{Injector, TargetRect};
use crate::plugins;
use lcv_proto::{
    AUDIO_PROTOCOL_VERSION, AudioChunk, CURSOR_PROTOCOL_VERSION, CursorShape, DisplayInfo, DisplayPreview, MIN_PROTOCOL_VERSION, Message, PLUGIN_ACCESS_DENIED,
    PLUGIN_NOT_ALLOWED, PLUGIN_NOT_INSTALLED, PLUGIN_NOT_RESPONDING, PLUGIN_UNKNOWN, PROTOCOL_VERSION, REJECT_VERSION, VideoFrame,
    read_message, write_message,
};
use std::collections::HashMap;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const NO_DISPLAY: u32 = u32::MAX;

pub struct SessionInfo {
    pub peer: SocketAddr,
    pub name: String,
    stop: Arc<AtomicBool>,
    stream: TcpStream,
}

#[derive(Default)]
pub struct HostState {
    pub port: u16,
    pub listen_error: Option<String>,
    pub session: Option<SessionInfo>,
    /// Solicitud de conexión esperando que el usuario la acepte o rechace en la interfaz.
    pub pending: Option<ConsentRequest>,
}

/// Una solicitud de conexión pendiente. La interfaz la muestra y responde con `Host::answer`.
pub struct ConsentRequest {
    pub name: String,
    pub ip: std::net::IpAddr,
    pub since: Instant,
    answer: mpsc::Sender<bool>,
}

#[derive(Clone)]
pub struct Host {
    state: Arc<Mutex<HostState>>,
    /// `true` mientras hay una sesión activa (lo lee el descubrimiento para anunciarse ocupado).
    busy: Arc<AtomicBool>,
}

impl Host {
    pub fn start(port: u16) -> Self {
        let state = Arc::new(Mutex::new(HostState { port, ..Default::default() }));
        let busy = Arc::new(AtomicBool::new(false));
        let (s, b) = (state.clone(), busy.clone());
        thread::Builder::new().name("host-listen".into()).spawn(move || listen(port, s, b)).ok();
        Self { state, busy }
    }

    pub fn state(&self) -> MutexGuard<'_, HostState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Indicador compartido de sesión activa, para el anuncio de descubrimiento.
    pub fn busy_flag(&self) -> Arc<AtomicBool> {
        self.busy.clone()
    }

    /// Respuesta del usuario a la solicitud de conexión pendiente.
    pub fn answer(&self, accept: bool) {
        if let Some(req) = self.state().pending.take() {
            let _ = req.answer.send(accept);
        }
    }

    /// Corta la sesión activa (botón "Desconectar" del equipo controlado).
    pub fn disconnect(&self) {
        if let Some(s) = self.state().session.as_ref() {
            s.stop.store(true, Ordering::Relaxed);
            let _ = s.stream.shutdown(Shutdown::Both);
        }
    }
}

fn listen(port: u16, state: Arc<Mutex<HostState>>, busy: Arc<AtomicBool>) {
    let listener = match bind_listener(port) {
        Ok(l) => {
            state.lock().unwrap().listen_error = None;
            l
        }
        Err(e) => {
            warn!("no se pudo escuchar en el puerto {port}: {e}");
            state.lock().unwrap().listen_error = Some(e.to_string());
            return;
        }
    };
    info!("escuchando en TCP {port}");
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let state = state.clone();
        let busy = busy.clone();
        thread::Builder::new()
            .name("host-session".into())
            .spawn(move || {
                if let Err(e) = handle(stream, &state, &busy) {
                    warn!("sesión terminada: {e:#}");
                }
            })
            .ok();
    }
}

/// Enlaza el socket de escucha con `SO_REUSEADDR` y reintenta unos segundos. Así, cuando una
/// instancia se relanza tras actualizar, no falla con el error 10048 porque el puerto de la
/// instancia anterior siga en `TIME_WAIT` o tarde un instante en liberarse.
fn bind_listener(port: u16) -> std::io::Result<TcpListener> {
    use socket2::{Domain, Socket, Type};
    use std::net::Ipv4Addr;

    let addr: SocketAddr = (Ipv4Addr::UNSPECIFIED, port).into();
    let mut last_err = None;
    // ~6 s de margen (15 × 400 ms): de sobra para un relanzado por actualización.
    for intento in 0..15 {
        let sock = Socket::new(Domain::IPV4, Type::STREAM, None)?;
        sock.set_reuse_address(true)?;
        match sock.bind(&addr.into()).and_then(|_| sock.listen(128)) {
            Ok(_) => return Ok(sock.into()),
            Err(e) => {
                if intento == 0 {
                    warn!("puerto {port} ocupado, reintentando: {e}");
                }
                last_err = Some(e);
                thread::sleep(Duration::from_millis(400));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("no se pudo enlazar el puerto")))
}

fn handle(mut stream: TcpStream, state: &Arc<Mutex<HostState>>, busy: &Arc<AtomicBool>) -> anyhow::Result<()> {
    let peer = stream.peer_addr()?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let Message::Hello { version, name } = read_message(&mut stream)? else { bail!("se esperaba Hello") };
    info!("conexión de {name} ({peer})");

    let reject = |stream: &mut TcpStream, reason: &str| {
        let _ = write_message(stream, &Message::Rejected { reason: reason.into() });
    };
    // Se admiten visores de versiones anteriores compatibles (sin audio); el motivo del rechazo es
    // el mismo que usaban los hosts antiguos, para que un visor nuevo sepa reintentar.
    if !(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version) {
        reject(&mut stream, REJECT_VERSION);
        bail!("versión {version} incompatible");
    }
    if state.lock().unwrap().session.is_some() {
        reject(&mut stream, "El equipo ya tiene una sesión activa");
        bail!("ocupado");
    }

    // Autenticación opcional por contraseña (reto-respuesta HMAC). Si coincide, se entra sin pedir
    // aceptación manual. Si no hay contraseña, o es incorrecta, se cae al diálogo de aceptación.
    let authed = verify_password(&mut stream)?;
    if authed {
        info!("{name} ({peer}) autenticado por contraseña");
    }
    if !authed
        && let Err(reason) = ask_consent(&name, peer, &stream, state)
    {
        reject(&mut stream, reason);
        bail!("no aceptada: {reason}");
    }
    stream.set_read_timeout(None)?;

    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut st = state.lock().unwrap();
        if st.session.is_some() {
            reject(&mut stream, "El equipo ya tiene una sesión activa");
            bail!("ocupado");
        }
        st.session = Some(SessionInfo { peer, name, stop: stop.clone(), stream: stream.try_clone()? });
    }
    busy.store(true, Ordering::Relaxed);
    let result = run_session(stream, stop, version);
    busy.store(false, Ordering::Relaxed);
    state.lock().unwrap().session = None;
    info!("sesión con {peer} cerrada");
    result
}

/// Reto-respuesta de contraseña. Devuelve `true` solo si el host tiene contraseña configurada y el
/// visor respondió con el HMAC correcto. Si no hay contraseña, devuelve `false` (se pedirá aceptar).
fn verify_password(stream: &mut TcpStream) -> anyhow::Result<bool> {
    let Some(password) = crate::auth::load_password() else { return Ok(false) };
    let nonce = crate::auth::random_nonce();
    write_message(stream, &Message::AuthChallenge { nonce: nonce.clone() })?;
    // El visor puede estar pidiendo la contraseña al usuario: damos margen para que la escriba.
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    let mac = match read_message(stream)? {
        Message::AuthResponse { mac } => mac,
        _ => return Ok(false),
    };
    let expected = crate::auth::hmac_sha256(password.as_bytes(), &nonce);
    Ok(crate::auth::constant_eq(&expected, &mac))
}

/// Plazo para responder a una solicitud de conexión antes de rechazarla sola.
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(60);

/// Pide permiso al usuario dentro de LCVDesk: la ventana sale al frente (aunque esté en la bandeja)
/// con el aviso «Aceptar / Rechazar». Se rechaza si no hay respuesta en `CONSENT_TIMEOUT` o si el
/// visor cancela (cierra la conexión) mientras tanto. `Err` lleva el motivo que se le envía al visor.
fn ask_consent(name: &str, peer: SocketAddr, stream: &TcpStream, state: &Arc<Mutex<HostState>>) -> Result<(), &'static str> {
    let (tx, rx) = mpsc::channel();
    {
        let mut st = lock(state);
        if st.pending.is_some() {
            info!("solicitud de {name} ({}) rechazada: ya hay otra esperando respuesta", peer.ip());
            return Err("El equipo está atendiendo otra solicitud de conexión");
        }
        st.pending = Some(ConsentRequest { name: name.into(), ip: peer.ip(), since: Instant::now(), answer: tx });
    }
    info!("solicitud de conexión de {name} ({}): esperando respuesta", peer.ip());
    crate::tray::alert();

    // Lecturas cortas solo para notar si el visor cancela; no se consume nada del flujo.
    let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
    let deadline = Instant::now() + CONSENT_TIMEOUT;
    let answer = loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(true) => break Ok(()),
            Ok(false) => break Err("El usuario rechazó la conexión"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err("El usuario rechazó la conexión"),
        }
        let mut probe = [0u8; 1];
        match stream.peek(&mut probe) {
            Ok(0) => break Err("El otro equipo canceló la solicitud"),
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break Err("El otro equipo canceló la solicitud"),
        }
        if Instant::now() >= deadline {
            break Err("No se respondió a la solicitud a tiempo");
        }
    };
    lock(state).pending = None;
    crate::tray::alert_done();
    match answer {
        Ok(()) => info!("solicitud de {name}: aceptada"),
        Err(reason) => info!("solicitud de {name}: {reason}"),
    }
    answer
}

fn to_proto(d: &lcv_capture::DisplayInfo) -> DisplayInfo {
    DisplayInfo {
        index: d.index,
        name: d.name.clone(),
        width: d.width,
        height: d.height,
        x: d.x,
        y: d.y,
        primary: d.primary,
    }
}

fn target_rect(d: &lcv_capture::DisplayInfo) -> TargetRect {
    TargetRect { x: d.x, y: d.y, width: d.width, height: d.height }
}

fn run_session(stream: TcpStream, stop: Arc<AtomicBool>, peer_version: u32) -> anyhow::Result<()> {
    let fps: f32 = crate::env_or("LCVDESK_FPS", 60.0);
    let bitrate = crate::env_or("LCVDESK_BITRATE_MBPS", 25u32) * 1_000_000;
    // Un IDR periódico acota cuánto dura una posible congelación si el visor pierde la sincronía.
    let keyframe_interval = Duration::from_secs_f32(crate::env_or("LCVDESK_KEYFRAME_SEC", 2.0));

    let displays = lcv_capture::enumerate().context("no se pudieron enumerar los monitores")?;
    let start_index = displays.iter().find(|d| d.primary).map_or(0, |d| d.index);
    let mut capturer = Capturer::new(start_index).context("no se pudo iniciar la captura")?;
    // Visor con puntero aparte (v5): no se pinta en la imagen; lo envía su propio hilo.
    let cursor_apart = peer_version >= CURSOR_PROTOCOL_VERSION;
    capturer.set_with_cursor(!cursor_apart);
    let cur = capturer.display().clone();
    info!("transmitiendo monitor {} {}x{} a {fps} fps, captura {:?}", cur.index, cur.width, cur.height, capturer.backend());

    // Único escritor del socket: un hilo que consume una cola acotada. Todos los mensajes salientes
    // (video, Pong, Displays) pasan por aquí, así que no hay escrituras concurrentes al socket.
    let (tx, rx) = mpsc::sync_channel::<Message>(3);
    let keyframe = Arc::new(AtomicBool::new(false));
    let requested_display = Arc::new(AtomicU32::new(NO_DISPLAY));
    tx.send(Message::Accepted {
        host_id: crate::identity::machine_id(),
        host_name: crate::identity::machine_name(),
        displays: displays.iter().map(to_proto).collect(),
        current: cur.index,
    })?;
    // Audio: cola propia (~1 s de trozos de 20 ms) para que el vídeo no lo retrase ni lo descarte.
    let (audio_tx, audio_rx) = mpsc::sync_channel::<Message>(50);
    let audio_on = Arc::new(AtomicBool::new(false));
    // Peticiones de miniatura de monitor (botones 1 2 3… del visor).
    let (preview_tx, preview_rx) = mpsc::channel::<u32>();
    // Túnel de complementos: cola propia que nunca descarta. Si la red va lenta, quien envía espera
    // (y el complemento nota la presión en su tubería) en vez de perder datos.
    let (plugin_tx, plugin_rx) = mpsc::sync_channel::<Message>(64);
    spawn_writer(stream.try_clone()?, rx, audio_rx, plugin_rx, stop.clone());
    spawn_reader(
        stream.try_clone()?,
        tx.clone(),
        keyframe.clone(),
        requested_display.clone(),
        stop.clone(),
        displays.clone(),
        target_rect(&cur),
        audio_on.clone(),
        preview_tx,
        plugin_tx,
    );
    if peer_version >= AUDIO_PROTOCOL_VERSION {
        spawn_audio(audio_tx, audio_on, stop.clone());
        spawn_previews(preview_rx, displays.clone(), tx.clone(), stop.clone());
    } else {
        info!("el visor usa la versión {peer_version} del protocolo: sesión sin audio");
    }
    // Monitor que se transmite, para dar la posición del puntero relativa a él.
    let cursor_area = Arc::new(Mutex::new(target_rect(&cur)));
    if cursor_apart {
        spawn_cursor(tx.clone(), cursor_area.clone(), stop.clone());
    }

    let interval = Duration::from_secs_f32(1.0 / fps);
    let mut encoder: Option<H264Encoder> = None;
    let mut seq = 0u64;
    let mut last_keyframe = Instant::now();
    // Contadores para el registro de rendimiento.
    let mut stat_window = Instant::now();
    let (mut sent, mut skipped, mut encode_us, mut bytes) = (0u32, 0u32, 0u64, 0usize);
    let result = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }
        // Cambio de monitor pedido por el visor.
        let req = requested_display.swap(NO_DISPLAY, Ordering::Relaxed);
        if req != NO_DISPLAY && req != capturer.display().index {
            match Capturer::new(req) {
                Ok(mut c) => {
                    c.set_with_cursor(!cursor_apart);
                    let d = c.display().clone();
                    *lock(&cursor_area) = target_rect(&d);
                    info!("cambiando al monitor {} {}x{}", d.index, d.width, d.height);
                    capturer = c;
                    encoder = None;
                    let _ = tx.try_send(Message::Displays {
                        displays: displays.iter().map(to_proto).collect(),
                        current: d.index,
                    });
                }
                Err(e) => warn!("no se pudo cambiar al monitor {req}: {e}"),
            }
        }

        let changed = capturer.next_frame(interval)?.is_some();
        let periodic = last_keyframe.elapsed() >= keyframe_interval;
        let want_keyframe = keyframe.swap(false, Ordering::Relaxed) || periodic;
        if !changed && !want_keyframe {
            continue;
        }
        let Some(frame) = capturer.last_frame() else { continue };
        let t = Instant::now();
        if !encoder.as_ref().is_some_and(|e| e.matches(frame.width, frame.height)) {
            encoder = Some(H264Encoder::new(frame.width, frame.height, fps, bitrate)?);
        }
        let enc = encoder.as_mut().unwrap();
        if want_keyframe {
            enc.request_keyframe();
        }
        let out = enc.encode_bgra(frame.data, frame.width * 4)?;
        if out.data.is_empty() {
            continue;
        }
        if out.keyframe {
            last_keyframe = Instant::now();
        }
        encode_us += t.elapsed().as_micros() as u64;
        let size = out.data.len();
        let msg = Message::VideoFrame(VideoFrame {
            seq,
            width: frame.width & !1,
            height: frame.height & !1,
            keyframe: out.keyframe,
            host_us: t.elapsed().as_micros() as u32,
            data: out.data,
        });
        seq += 1;
        // Si el visor va lento, la cola se llena: se descarta ESTE fotograma antes de enviarlo.
        // Como no llegó a entrar en la cadena del decodificador, no provoca congelación; el IDR
        // periódico y la petición de fotograma clave del visor mantienen la imagen al día.
        match tx.try_send(msg) {
            Ok(()) => {
                sent += 1;
                bytes += size;
            }
            Err(TrySendError::Full(_)) => skipped += 1,
            Err(TrySendError::Disconnected(_)) => break Ok(()),
        }

        if stat_window.elapsed() >= Duration::from_secs(2) {
            let secs = stat_window.elapsed().as_secs_f32();
            let total = (sent + skipped).max(1);
            info!(
                "video: enviados {:.0} fps, descartados {:.0} fps, codif. media {:.1} ms, {:.1} Mbps, captura {:?}",
                sent as f32 / secs,
                skipped as f32 / secs,
                encode_us as f32 / 1000.0 / total as f32,
                bytes as f32 * 8.0 / secs / 1e6,
                capturer.backend(),
            );
            (sent, skipped, encode_us, bytes) = (0, 0, 0, 0);
            stat_window = Instant::now();
        }
    };
    stop.store(true, Ordering::Relaxed);
    let _ = stream.shutdown(Shutdown::Both);
    result
}

/// Hilo único de escritura al socket. El audio va primero: son trozos pequeños y cualquier espera
/// detrás de un fotograma grande se oiría como un corte. Después, el túnel de complementos.
fn spawn_writer(
    mut stream: TcpStream,
    rx: mpsc::Receiver<Message>,
    audio_rx: mpsc::Receiver<Message>,
    plugin_rx: mpsc::Receiver<Message>,
    stop: Arc<AtomicBool>,
) {
    thread::Builder::new()
        .name("host-writer".into())
        .spawn(move || {
            'out: loop {
                while let Ok(chunk) = audio_rx.try_recv() {
                    if write_message(&mut stream, &chunk).is_err() {
                        break 'out;
                    }
                }
                while let Ok(msg) = plugin_rx.try_recv() {
                    if write_message(&mut stream, &msg).is_err() {
                        break 'out;
                    }
                }
                match rx.recv_timeout(Duration::from_millis(5)) {
                    Ok(msg) => {
                        if write_message(&mut stream, &msg).is_err() {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            stop.store(true, Ordering::Relaxed);
            let _ = stream.shutdown(Shutdown::Both);
        })
        .ok();
}

/// Miniaturas de monitores bajo demanda: captura ese monitor con GDI (independiente de la captura
/// DXGI del monitor que se está transmitiendo), lo reduce a ≤ 240×180 y lo envía. Como mucho una por
/// monitor cada 500 ms, por si el visor insiste.
fn spawn_previews(
    rx: mpsc::Receiver<u32>,
    displays: Vec<lcv_capture::DisplayInfo>,
    tx: SyncSender<Message>,
    stop: Arc<AtomicBool>,
) {
    thread::Builder::new()
        .name("host-previews".into())
        .spawn(move || {
            let mut grabbers: std::collections::HashMap<u32, lcv_capture::Gdi> = Default::default();
            let mut last_sent: std::collections::HashMap<u32, Instant> = Default::default();
            while !stop.load(Ordering::Relaxed) {
                let index = match rx.recv_timeout(Duration::from_millis(500)) {
                    Ok(i) => i,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if last_sent.get(&index).is_some_and(|t| t.elapsed() < Duration::from_millis(500)) {
                    continue;
                }
                let Some(d) = displays.iter().find(|d| d.index == index) else { continue };
                let grabber = match grabbers.entry(index) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(e) => match lcv_capture::Gdi::new(d.x, d.y, d.width, d.height) {
                        Ok(g) => e.insert(g),
                        Err(err) => {
                            warn!("miniatura del monitor {index}: {err}");
                            continue;
                        }
                    },
                };
                if let Err(err) = grabber.capture() {
                    warn!("miniatura del monitor {index}: {err}");
                    continue;
                }
                let (width, height, rgba) = lcv_capture::thumbnail(&grabber.frame(), 240, 180);
                let msg = Message::Preview(DisplayPreview { index, width, height, rgba });
                // Si la cola del vídeo está llena se omite; el visor la vuelve a pedir enseguida.
                if tx.try_send(msg).is_ok() {
                    last_sent.insert(index, Instant::now());
                }
            }
        })
        .ok();
}

/// Envía el puntero aparte del vídeo (visores v5): su forma una vez por cursor distinto y su
/// posición cada vez que cambia, unas 120 veces por segundo como mucho.
fn spawn_cursor(tx: SyncSender<Message>, area: Arc<Mutex<TargetRect>>, stop: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("host-cursor".into())
        .spawn(move || {
            let mut shapes_sent = std::collections::HashSet::new();
            let mut last = None;
            while !stop.load(Ordering::Relaxed) {
                let r = *lock(&area);
                let msg = match lcv_capture::cursor_state() {
                    Some(c) => {
                        let (x, y) = (c.x - r.x, c.y - r.y);
                        let inside = x >= 0 && y >= 0 && (x as u32) < r.width && (y as u32) < r.height;
                        if c.visible && shapes_sent.insert(c.id) {
                            match lcv_capture::cursor_image(c.id) {
                                Some(img) => {
                                    let shape = CursorShape {
                                        id: c.id,
                                        width: img.width,
                                        height: img.height,
                                        hot_x: img.hot_x,
                                        hot_y: img.hot_y,
                                        rgba: img.rgba,
                                    };
                                    if tx.send(Message::CursorShape(shape)).is_err() {
                                        break;
                                    }
                                }
                                None => warn!("puntero {:#x}: no se pudo leer su forma", c.id),
                            }
                        }
                        Message::CursorPos { id: c.id, x, y, visible: c.visible && inside }
                    }
                    None => Message::CursorPos { id: 0, x: 0, y: 0, visible: false },
                };
                if last.as_ref() != Some(&msg) {
                    if tx.send(msg.clone()).is_err() {
                        break;
                    }
                    last = Some(msg);
                }
                thread::sleep(Duration::from_millis(8));
            }
        })
        .ok();
}

/// Captura el sonido del equipo mientras el visor lo tenga activado (botón 🔊), y lo envía.
fn spawn_audio(audio_tx: SyncSender<Message>, audio_on: Arc<AtomicBool>, stop: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("host-audio".into())
        .spawn(move || {
            let mut capture: Option<lcv_audio::Loopback> = None;
            while !stop.load(Ordering::Relaxed) {
                let want = audio_on.load(Ordering::Relaxed);
                if want && capture.is_none() {
                    info!("audio: el visor lo pidió; empezando a capturar");
                    let (tx, stop) = (audio_tx.clone(), stop.clone());
                    capture = Some(lcv_audio::Loopback::start(move |c| {
                        let msg = Message::Audio(AudioChunk {
                            sample_rate: c.sample_rate,
                            channels: c.channels,
                            pcm: c.to_le_bytes(),
                        });
                        // Si la cola está llena (red o visor lentos), se pierde este trozo.
                        !matches!(tx.try_send(msg), Err(TrySendError::Disconnected(_))) && !stop.load(Ordering::Relaxed)
                    }));
                } else if !want && capture.is_some() {
                    info!("audio: el visor lo silenció; captura detenida");
                    capture = None;
                }
                thread::sleep(Duration::from_millis(100));
            }
        })
        .ok();
}

/// Atiende los mensajes del visor: `Ping`, `RequestKeyframe`, `SelectDisplay` y la entrada remota.
#[allow(clippy::too_many_arguments)]
fn spawn_reader(
    mut stream: TcpStream,
    tx: SyncSender<Message>,
    keyframe: Arc<AtomicBool>,
    requested_display: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    displays: Vec<lcv_capture::DisplayInfo>,
    initial_target: TargetRect,
    audio_on: Arc<AtomicBool>,
    preview_tx: mpsc::Sender<u32>,
    plugin_tx: SyncSender<Message>,
) {
    thread::Builder::new()
        .name("host-reader".into())
        .spawn(move || {
            // El visor envía Ping cada segundo; si no llega nada en 8 s, damos la conexión por muerta.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(8)));
            let mut injector = Injector::new(initial_target);
            let channels: Arc<Mutex<HashMap<u32, Arc<plugins::Channel>>>> = Default::default();
            loop {
                match read_message(&mut stream) {
                    // Pong y Displays no deben bloquear la lectura: si la cola está llena, se omiten.
                    Ok(Message::Ping { t_us }) => {
                        let _ = tx.try_send(Message::Pong { t_us });
                    }
                    Ok(Message::RequestKeyframe) => keyframe.store(true, Ordering::Relaxed),
                    Ok(Message::SelectDisplay { index }) => {
                        requested_display.store(index, Ordering::Relaxed);
                        if let Some(d) = displays.iter().find(|d| d.index == index) {
                            injector.set_target(target_rect(d));
                        }
                    }
                    Ok(msg @ (Message::MouseMove { .. }
                    | Message::MouseButton { .. }
                    | Message::MouseWheel { .. }
                    | Message::Key { .. }
                    | Message::Unicode { .. }
                    | Message::ReleaseAll)) => injector.handle(&msg),
                    Ok(Message::AudioEnable { on }) => audio_on.store(on, Ordering::Relaxed),
                    Ok(Message::PreviewRequest { index }) => {
                        let _ = preview_tx.send(index);
                    }
                    Ok(Message::PluginQuery) => {
                        let _ = plugin_tx.send(Message::PluginList { items: plugins::local_list() });
                    }
                    Ok(Message::PluginOpen { id, channel }) => {
                        open_plugin(id, channel, channels.clone(), plugin_tx.clone());
                    }
                    Ok(Message::PluginData { channel, data }) => {
                        if let Some(ch) = lock(&channels).get(&channel) {
                            ch.push(data);
                        }
                    }
                    Ok(Message::PluginClose { channel }) => {
                        if let Some(ch) = lock(&channels).remove(&channel) {
                            info!("el visor cerró el complemento {} (canal {channel})", ch.plugin);
                            ch.close();
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            // Fin de la sesión: se cierran las tuberías para que el complemento se desbloquee ya.
            for (_, ch) in lock(&channels).drain() {
                ch.close();
            }
            injector.release_all();
            stop.store(true, Ordering::Relaxed);
        })
        .ok();
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Abre el lado controlado de un complemento (en su propio hilo: puede tardar hasta 10 s). La ruta y
/// los argumentos salen del registro de este equipo; del visor solo se acepta un id conocido.
fn open_plugin(
    id: String,
    channel: u32,
    channels: Arc<Mutex<HashMap<u32, Arc<plugins::Channel>>>>,
    plugin_tx: SyncSender<Message>,
) {
    let fail = move |tx: &SyncSender<Message>, reason: &str| {
        let _ = tx.send(Message::PluginError { channel, reason: reason.into() });
    };
    if !crate::settings::load_plugins_allowed() {
        info!("complemento {id}: rechazado (los complementos están desactivados en Ajustes)");
        return fail(&plugin_tx, PLUGIN_NOT_ALLOWED);
    }
    if id != plugins::IGP_SOFT {
        warn!("complemento desconocido pedido por el visor: {id:?}");
        return fail(&plugin_tx, PLUGIN_UNKNOWN);
    }
    let Some(igp) = plugins::detect(&id) else {
        info!("complemento {id}: no está instalado en este equipo");
        return fail(&plugin_tx, PLUGIN_NOT_INSTALLED);
    };
    // Se registra antes de conectar: si el visor lo cierra mientras tanto, se abandona.
    let ch = plugins::Channel::new(channel, &id);
    if let Some(old) = lock(&channels).insert(channel, ch.clone()) {
        old.close();
    }
    thread::Builder::new()
        .name("host-plugin-open".into())
        .spawn(move || {
            let name = plugins::igp_control_pipe();
            info!("complemento {id} {} (canal {channel}): conectando con la tubería {name}", igp.version);
            let pipe = plugins::Pipe::connect(&name, Duration::from_millis(500)).or_else(|e| {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    return Err(e);
                }
                // IGP_Soft no está abierto: se abre en la bandeja y se espera su tubería.
                plugins::launch(&igp.exe, &["--controlado".into()])?;
                plugins::Pipe::connect(&name, plugins::CONNECT_TIMEOUT)
            });
            let pipe = match pipe {
                Ok(p) => p,
                Err(e) => {
                    warn!("complemento {id} (canal {channel}): no se pudo conectar con {name}: {e}");
                    lock(&channels).remove(&channel);
                    ch.close();
                    let reason = if e.kind() == std::io::ErrorKind::PermissionDenied { PLUGIN_ACCESS_DENIED } else { PLUGIN_NOT_RESPONDING };
                    return fail(&plugin_tx, reason);
                }
            };
            if ch.is_closed() {
                info!("complemento {id} (canal {channel}): el visor lo cerró mientras se conectaba");
                return;
            }
            info!("complemento {id} (canal {channel}): conectado");
            let _ = plugin_tx.send(Message::PluginOpened { channel });
            let tx = plugin_tx.clone();
            ch.start(
                pipe,
                move |m| tx.send(m).is_ok(),
                move || {
                    lock(&channels).remove(&channel);
                },
            );
        })
        .ok();
}

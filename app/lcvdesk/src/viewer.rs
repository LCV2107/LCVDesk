//! Lado que controla: se conecta a un host, recibe el video, lo decodifica y envía la entrada.

use eframe::egui;
use lcv_codec::H264Decoder;
use lcv_proto::{DEFAULT_PORT, DisplayInfo, Message, PROTOCOL_VERSION, read_message, write_message};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
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
}

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
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let writer = Arc::new(Mutex::new(None));

        // Canal del hook de teclado → socket. Un hilo dedicado escribe las teclas capturadas para que
        // la función del hook (que debe ser muy rápida) nunca bloquee escribiendo en la red.
        let (key_tx, key_rx) = std::sync::mpsc::channel::<Message>();
        crate::keyhook::set_sink(Some(key_tx));
        {
            let (wr, st) = (writer.clone(), stop.clone());
            thread::Builder::new()
                .name("viewer-keys".into())
                .spawn(move || {
                    for msg in key_rx {
                        if st.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Some(s) = lock(&wr).as_mut() {
                            let _ = write_message(s, &msg);
                        }
                    }
                })
                .ok();
        }

        let (pw_tx, pw_rx) = std::sync::mpsc::channel::<Option<(String, bool)>>();

        let (sh, st, wr, t) = (shared.clone(), stop.clone(), writer.clone(), target.clone());
        thread::Builder::new()
            .name("viewer".into())
            .spawn(move || {
                let result = run(&t, initial_password, pw_rx, &sh, &st, &wr, &ctx);
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
        Self { target, display_name, shared, stop, writer, pw_tx }
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
        crate::keyhook::set_grab(false);
        crate::keyhook::set_sink(None);
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

fn run(
    target: &str,
    initial_password: Option<String>,
    pw_rx: std::sync::mpsc::Receiver<Option<(String, bool)>>,
    shared: &Mutex<Shared>,
    stop: &Arc<AtomicBool>,
    writer: &Arc<Mutex<Option<TcpStream>>>,
    ctx: &egui::Context,
) -> std::io::Result<String> {
    let target = if target.contains(':') { target.to_string() } else { format!("{target}:{DEFAULT_PORT}") };
    let addr = target
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "dirección no válida"))?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_nodelay(true)?;
    *lock(writer) = Some(stream.try_clone()?);

    let name = hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_else(|_| "Equipo".into());
    if let Some(s) = lock(writer).as_mut() {
        write_message(s, &Message::Hello { version: PROTOCOL_VERSION, name })?;
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

    loop {
        match read_message(&mut stream)? {
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
                let mut sh = lock(shared);
                sh.status = Status::Streaming;
                sh.displays = displays;
                sh.current_display = current;
                sh.host_id = host_id;
                sh.host_name = host_name;
                drop(sh);
                spawn_pinger(writer.clone(), stop.clone(), clock);
                ctx.request_repaint();
            }
            Message::Displays { displays, current } => {
                let mut sh = lock(shared);
                sh.displays = displays;
                sh.current_display = current;
            }
            Message::Rejected { reason } => return Ok(reason),
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

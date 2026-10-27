//! Lado controlado: escucha conexiones, pide permiso al usuario y transmite la pantalla.

use anyhow::{Context, bail};
use lcv_capture::Capturer;
use lcv_codec::H264Encoder;
use lcv_input::{Injector, TargetRect};
use lcv_proto::{DisplayInfo, Message, PROTOCOL_VERSION, VideoFrame, read_message, write_message};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MessageBoxW,
};
use windows::core::HSTRING;

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

    /// Corta la sesión activa (botón "Desconectar" del equipo controlado).
    pub fn disconnect(&self) {
        if let Some(s) = self.state().session.as_ref() {
            s.stop.store(true, Ordering::Relaxed);
            let _ = s.stream.shutdown(Shutdown::Both);
        }
    }
}

fn listen(port: u16, state: Arc<Mutex<HostState>>, busy: Arc<AtomicBool>) {
    let listener = match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => l,
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

fn handle(mut stream: TcpStream, state: &Arc<Mutex<HostState>>, busy: &Arc<AtomicBool>) -> anyhow::Result<()> {
    let peer = stream.peer_addr()?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let Message::Hello { version, name } = read_message(&mut stream)? else { bail!("se esperaba Hello") };
    info!("conexión de {name} ({peer})");

    let reject = |stream: &mut TcpStream, reason: &str| {
        let _ = write_message(stream, &Message::Rejected { reason: reason.into() });
    };
    if version != PROTOCOL_VERSION {
        reject(&mut stream, "Versión de LCVDesk incompatible");
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
    if !authed && !ask_consent(&name, peer) {
        reject(&mut stream, "El usuario rechazó la conexión");
        bail!("rechazada por el usuario");
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
    let result = run_session(stream, stop);
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
    // El tiempo de espera de lectura (10 s) ya está activo en este punto.
    let mac = match read_message(stream)? {
        Message::AuthResponse { mac } => mac,
        _ => return Ok(false),
    };
    let expected = crate::auth::hmac_sha256(password.as_bytes(), &nonce);
    Ok(crate::auth::constant_eq(&expected, &mac))
}

/// Aceptación explícita mínima. La ventana completa (permisos, código de verificación) llega en la Fase 4.4.
fn ask_consent(name: &str, peer: SocketAddr) -> bool {
    let text = format!(
        "El equipo «{name}» ({}) quiere ver y controlar tu pantalla.\n\n¿Permitir la conexión?\n\n\
         Versión de pruebas: la conexión todavía no está cifrada.",
        peer.ip()
    );
    let answer = unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("LCVDesk — Solicitud de conexión"),
            MB_YESNO | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND,
        )
    };
    answer == IDYES
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

fn run_session(stream: TcpStream, stop: Arc<AtomicBool>) -> anyhow::Result<()> {
    let fps: f32 = crate::env_or("LCVDESK_FPS", 60.0);
    let bitrate = crate::env_or("LCVDESK_BITRATE_MBPS", 25u32) * 1_000_000;
    // Un IDR periódico acota cuánto dura una posible congelación si el visor pierde la sincronía.
    let keyframe_interval = Duration::from_secs_f32(crate::env_or("LCVDESK_KEYFRAME_SEC", 2.0));

    let displays = lcv_capture::enumerate().context("no se pudieron enumerar los monitores")?;
    let start_index = displays.iter().find(|d| d.primary).map_or(0, |d| d.index);
    let mut capturer = Capturer::new(start_index).context("no se pudo iniciar la captura")?;
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
    spawn_writer(stream.try_clone()?, rx, stop.clone());
    spawn_reader(
        stream.try_clone()?,
        tx.clone(),
        keyframe.clone(),
        requested_display.clone(),
        stop.clone(),
        displays.clone(),
        target_rect(&cur),
    );

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
                Ok(c) => {
                    let d = c.display().clone();
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

/// Hilo único de escritura al socket: envía en orden todo lo que llega por la cola.
fn spawn_writer(mut stream: TcpStream, rx: mpsc::Receiver<Message>, stop: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("host-writer".into())
        .spawn(move || {
            for msg in rx {
                if write_message(&mut stream, &msg).is_err() {
                    break;
                }
            }
            stop.store(true, Ordering::Relaxed);
            let _ = stream.shutdown(Shutdown::Both);
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
) {
    thread::Builder::new()
        .name("host-reader".into())
        .spawn(move || {
            // El visor envía Ping cada segundo; si no llega nada en 8 s, damos la conexión por muerta.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(8)));
            let mut injector = Injector::new(initial_target);
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
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            injector.release_all();
            stop.store(true, Ordering::Relaxed);
        })
        .ok();
}

//! Captura de teclado de bajo nivel (`WH_KEYBOARD_LL`) para el lado que controla.
//!
//! Mientras controlas el equipo remoto, este hook intercepta **todas** las teclas antes de que
//! Windows las procese en tu propio PC (tecla Windows, Alt+Tab, Ctrl+Esc, Win+flechas, etc.),
//! las suprime localmente y las reenvía al host como scancodes de hardware. Así dejan de abrir
//! el menú Inicio o cambiar de ventana en tu equipo y actúan en el equipo controlado.
//!
//! El hook no se puede enganchar a Ctrl+Alt+Supr (secuencia segura de Windows); eso queda, a
//! propósito, fuera del alcance. Para soltar el teclado se usa Ctrl+Alt+Inicio (o el botón).
//!
//! Robustez (ver replica/recon.md):
//! - Windows retira un hook de bajo nivel **sin avisar** si su función tarda más que
//!   `LowLevelHooksTimeout`. Un vigilante detecta teclas que llegan al sistema sin pasar por el
//!   hook y lo reinstala. Por eso la función del hook nunca escribe en el log ni hace E/S: solo
//!   toca atómicos; el hilo `keyhook-diag` es quien registra.
//! - El canal hacia la red tiene **dueño** (una sesión del visor): un visor viejo que se cierra
//!   tarde no puede borrar el canal del nuevo.
//! - Solo se suprime la liberación de las teclas cuya pulsación se suprimió; si la pulsación llegó
//!   a este PC (antes de capturar), la liberación también llega y nada se queda atascado.

use lcv_proto::Message;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HC_ACTION, HHOOK,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_APP, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// Estado del hook, para la interfaz y el log.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Health {
    /// Aún arrancando el hilo del hook.
    Starting,
    /// Instalado y recibiendo teclas.
    Installed,
    /// `SetWindowsHookExW` falló: la tecla Windows y Alt+Tab no se pueden capturar.
    Failed,
    /// Windows lo retiró (probable `LowLevelHooksTimeout`); se está reinstalando.
    Lost,
}

static HEALTH: AtomicU8 = AtomicU8::new(0);
/// ¿Debe capturar y suprimir las teclas ahora mismo? (true solo mientras controlas).
static GRAB: AtomicBool = AtomicBool::new(false);
/// Estado de Ctrl/Alt (para detectar la combinación de escape sin depender de nada más).
static CTRL_DOWN: AtomicBool = AtomicBool::new(false);
static ALT_DOWN: AtomicBool = AtomicBool::new(false);
/// El usuario pulsó Ctrl+Alt+Inicio; la interfaz lo consulta.
static RELEASE_REQUESTED: AtomicBool = AtomicBool::new(false);
/// Hilo que instaló el hook (las reinstalaciones deben hacerse en ese mismo hilo).
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
/// Mensaje al hilo del hook para reinstalarlo.
const WM_REHOOK: u32 = WM_APP + 1;

/// Teclas (scancode | extendida<<8) cuya pulsación se suprimió: su liberación también se suprime
/// y se reenvía al remoto aunque la captura ya se haya soltado.
static SWALLOWED: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];

// Contadores de diagnóstico (solo los escribe el hook; los lee y registra `keyhook-diag`).
static CALLS: AtomicU64 = AtomicU64::new(0);
static FORWARDED: AtomicU64 = AtomicU64::new(0);
/// Captura activa pero la ventana en primer plano no es nuestra.
static SKIP_NOT_FOREGROUND: AtomicU64 = AtomicU64::new(0);
/// Captura activa, ventana nuestra, pero no hay canal hacia ninguna sesión.
static SKIP_NO_SINK: AtomicU64 = AtomicU64::new(0);
/// El canal existe pero el hilo de envío ya terminó.
static SEND_FAILED: AtomicU64 = AtomicU64::new(0);
static WIN_SEEN: AtomicU64 = AtomicU64::new(0);
static WIN_FORWARDED: AtomicU64 = AtomicU64::new(0);
/// Última llamada al hook y duración máxima de una llamada (µs desde el arranque / µs).
static LAST_CALL_US: AtomicU64 = AtomicU64::new(0);
static MAX_CALL_US: AtomicU64 = AtomicU64::new(0);

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_us() -> u64 {
    epoch().elapsed().as_micros() as u64
}

/// Canal hacia la sesión que es dueña de la captura: (id de sesión, emisor).
type Sink = Mutex<Option<(u64, Sender<Message>)>>;

fn sink() -> &'static Sink {
    static SINK: OnceLock<Sink> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

/// Identificador único para cada sesión del visor (dueña del canal de teclas).
pub fn new_owner_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Instala el hook en un hilo propio con bucle de mensajes y arranca el vigilante. Llamar una vez.
pub fn start() {
    epoch();
    std::thread::Builder::new()
        .name("keyhook".into())
        .spawn(|| unsafe {
            HOOK_THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
            let mut hook = install();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_REHOOK {
                    if let Some(h) = hook.take() {
                        // Si Windows ya lo retiró, esto falla sin más; da igual.
                        let _ = UnhookWindowsHookEx(h);
                    }
                    hook = install();
                }
            }
        })
        .ok();
    std::thread::Builder::new().name("keyhook-diag".into()).spawn(diagnostics).ok();
}

/// Instala el hook con el módulo del ejecutable como `hMod` (la forma documentada para un hook
/// global; con `None` hay sistemas que lo rechazan con el error 1428).
unsafe fn install() -> Option<HHOOK> {
    let module = unsafe { GetModuleHandleW(None) }.ok().map(|m| HINSTANCE(m.0));
    match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0) } {
        Ok(h) => {
            LAST_CALL_US.store(now_us(), Ordering::Relaxed);
            HEALTH.store(Health::Installed as u8, Ordering::SeqCst);
            tracing::info!("hook de teclado instalado (hMod={:?})", module.map(|m| m.0));
            Some(h)
        }
        Err(e) => {
            HEALTH.store(Health::Failed as u8, Ordering::SeqCst);
            tracing::error!(
                "no se pudo instalar el hook de teclado: {e}. La tecla Windows y Alt+Tab no se podrán \
                 enviar al remoto (se usará el respaldo de la ventana, solo teclas normales)"
            );
            None
        }
    }
}

/// Estado actual del hook.
pub fn health() -> Health {
    match HEALTH.load(Ordering::SeqCst) {
        1 => Health::Installed,
        2 => Health::Failed,
        3 => Health::Lost,
        _ => Health::Starting,
    }
}

/// ¿El hook está operativo? Si no, el visor usa el reenvío de teclas de egui como respaldo.
pub fn installed() -> bool {
    health() == Health::Installed
}

/// Activa o desactiva la captura. Se llama cada fotograma según si tienes el control.
pub fn set_grab(on: bool) {
    if GRAB.swap(on, Ordering::Relaxed) != on {
        let owner = sink().lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|(id, _)| *id);
        tracing::info!(
            "captura de teclado {} (hook={:?}, ventana propia en primer plano={}, canal=sesión {})",
            if on { "activada" } else { "desactivada" },
            health(),
            foreground_is_ours(),
            owner.map_or("ninguna".to_string(), |id| id.to_string()),
        );
    }
}

/// ¿La ventana en primer plano pertenece a este proceso? Solo capturamos entonces, para no robar
/// el teclado a otras aplicaciones si cambias de ventana mientras el control sigue activo.
fn foreground_is_ours() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        pid == std::process::id()
    }
}

/// Registra el canal de teclas de la sesión `owner`, que pasa a ser la dueña.
pub fn set_sink(owner: u64, tx: Sender<Message>) {
    let mut guard = sink().lock().unwrap_or_else(|e| e.into_inner());
    if let Some((prev, _)) = guard.as_ref() {
        tracing::info!("canal de teclado: la sesión {owner} sustituye a la {prev}");
    } else {
        tracing::info!("canal de teclado registrado para la sesión {owner}");
    }
    *guard = Some((owner, tx));
}

/// Quita el canal **solo si** `owner` sigue siendo la dueña. Un visor viejo que se cierra después
/// de que otro registrara su canal no lo borra.
pub fn clear_sink(owner: u64) {
    let mut guard = sink().lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some((id, _)) if *id == owner => {
            *guard = None;
            drop(guard);
            GRAB.store(false, Ordering::Relaxed);
            tracing::info!("canal de teclado de la sesión {owner} retirado");
        }
        Some((id, _)) => tracing::info!("sesión {owner} cerrada; se conserva el canal de la sesión {id}"),
        None => {}
    }
}

/// ¿Se pulsó la combinación de escape (Ctrl+Alt+Inicio) desde la última consulta?
pub fn take_release_request() -> bool {
    RELEASE_REQUESTED.swap(false, Ordering::Relaxed)
}

fn key_index(scancode: u32, extended: bool) -> usize {
    ((scancode & 0xFF) as usize) | if extended { 0x100 } else { 0 }
}

fn mark_swallowed(idx: usize, on: bool) -> bool {
    let (word, bit) = (&SWALLOWED[idx / 64], 1u64 << (idx % 64));
    let prev = if on { word.fetch_or(bit, Ordering::Relaxed) } else { word.fetch_and(!bit, Ordering::Relaxed) };
    prev & bit != 0
}

/// Envía al canal dueño. Devuelve false si no hay canal o el envío falló.
fn forward(msg: Message) -> bool {
    let guard = sink().lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        None => {
            SKIP_NO_SINK.fetch_add(1, Ordering::Relaxed);
            false
        }
        Some((_, tx)) => {
            if tx.send(msg).is_ok() {
                FORWARDED.fetch_add(1, Ordering::Relaxed);
                true
            } else {
                SEND_FAILED.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }
}

// IMPORTANTE: nada de logs ni E/S aquí. Si esta función tarda más que LowLevelHooksTimeout,
// Windows retira el hook sin avisar.
unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let start = now_us();
    let result = unsafe { handle(code, wparam, lparam) };
    let end = now_us();
    LAST_CALL_US.store(end, Ordering::Relaxed);
    MAX_CALL_US.fetch_max(end.saturating_sub(start), Ordering::Relaxed);
    result.unwrap_or_else(|| unsafe { CallNextHookEx(None, code, wparam, lparam) })
}

/// Decide qué hacer con la tecla. `Some(LRESULT(1))` la suprime en este PC; `None` la deja pasar.
unsafe fn handle(code: i32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
    CALLS.fetch_add(1, Ordering::Relaxed);
    if code != HC_ACTION as i32 {
        return None;
    }
    let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    let m = wparam.0 as u32;
    let down = m == WM_KEYDOWN || m == WM_SYSKEYDOWN;
    let up = m == WM_KEYUP || m == WM_SYSKEYUP;
    if !(down || up) || kb.scanCode == 0 {
        return None;
    }
    let extended = kb.flags.0 & LLKHF_EXTENDED.0 != 0;
    let is_win = matches!(kb.vkCode, 0x5B | 0x5C);
    let idx = key_index(kb.scanCode, extended);

    // Liberación de una tecla cuya pulsación se suprimió: también se suprime y se reenvía, haya o
    // no captura ahora (si no, quedaría pulsada en el remoto o soltada "de la nada" aquí).
    if up && mark_swallowed(idx, false) {
        forward(Message::Key { scancode: kb.scanCode as u16, extended, down: false });
        return Some(LRESULT(1));
    }

    let ours = foreground_is_ours();
    if ours {
        // Ctrl/Alt y el interruptor de captura (Ctrl+Alt+Inicio), esté o no activa la captura.
        match kb.vkCode {
            0xA2 | 0xA3 | 0x11 => CTRL_DOWN.store(down, Ordering::Relaxed),
            0xA4 | 0xA5 | 0x12 => ALT_DOWN.store(down, Ordering::Relaxed),
            0x24 if down && CTRL_DOWN.load(Ordering::Relaxed) && ALT_DOWN.load(Ordering::Relaxed) => {
                RELEASE_REQUESTED.store(true, Ordering::Relaxed);
                mark_swallowed(idx, true);
                return Some(LRESULT(1));
            }
            _ => {}
        }
    }

    if !GRAB.load(Ordering::Relaxed) {
        return None;
    }
    if is_win && down {
        WIN_SEEN.fetch_add(1, Ordering::Relaxed);
    }
    if !ours {
        SKIP_NOT_FOREGROUND.fetch_add(1, Ordering::Relaxed);
        return None;
    }
    if !down {
        // Liberación cuya pulsación llegó a este PC (antes de capturar): que la vea también este PC.
        forward(Message::Key { scancode: kb.scanCode as u16, extended, down: false });
        return None;
    }
    if forward(Message::Key { scancode: kb.scanCode as u16, extended, down: true }) {
        if is_win {
            WIN_FORWARDED.fetch_add(1, Ordering::Relaxed);
        }
        mark_swallowed(idx, true);
        return Some(LRESULT(1));
    }
    // Sin canal: mejor que la tecla funcione en local que perderla.
    None
}

/// Teclas (sin los botones del ratón, 0x01-0x06) que se comprueban con `GetAsyncKeyState` para
/// detectar un hook retirado: si una tecla está pulsada para el sistema pero el hook no ha visto
/// nada, el hook ya no recibe eventos.
const WATCHED_FIRST: i32 = 0x08;
const WATCHED_LAST: i32 = 0xFE;

/// Hilo vigilante: detecta el hook retirado y lo reinstala; vuelca los contadores al log.
fn diagnostics() {
    let mut last = [0u64; 7];
    let mut last_report = Instant::now();
    let mut last_rehook: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let grab = GRAB.load(Ordering::Relaxed);

        // 1) ¿Hook retirado? Solo tiene sentido mientras capturamos con nuestra ventana delante:
        // entonces el hook suprime las pulsaciones y GetAsyncKeyState no debería verlas.
        if grab && health() == Health::Installed && foreground_is_ours() {
            let idle_ms = now_us().saturating_sub(LAST_CALL_US.load(Ordering::Relaxed)) / 1000;
            if idle_ms > 500 {
                let pressed = (WATCHED_FIRST..=WATCHED_LAST).find(|&vk| unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0);
                if let Some(vk) = pressed
                    && last_rehook.is_none_or(|t| t.elapsed() > Duration::from_secs(2))
                {
                    tracing::warn!(
                        "hook de teclado perdido: la tecla VK 0x{vk:02X} está pulsada y el hook no recibe \
                         nada desde hace {idle_ms} ms (probable LowLevelHooksTimeout). Reinstalando"
                    );
                    rehook();
                    last_rehook = Some(Instant::now());
                }
            }
        }

        // 2) Resumen de contadores cuando hay actividad (cada 3 s como mucho).
        let now = [
            CALLS.load(Ordering::Relaxed),
            FORWARDED.load(Ordering::Relaxed),
            SKIP_NOT_FOREGROUND.load(Ordering::Relaxed),
            SKIP_NO_SINK.load(Ordering::Relaxed),
            SEND_FAILED.load(Ordering::Relaxed),
            WIN_SEEN.load(Ordering::Relaxed),
            WIN_FORWARDED.load(Ordering::Relaxed),
        ];
        if !grab {
            // Sin captura no se informa: así los contadores reflejan solo el tiempo capturando.
            last = now;
            continue;
        }
        if now != last && last_report.elapsed() >= Duration::from_secs(3) {
            let d: Vec<u64> = now.iter().zip(&last).map(|(a, b)| a - b).collect();
            let max_ms = MAX_CALL_US.swap(0, Ordering::Relaxed) as f64 / 1000.0;
            tracing::info!(
                "hook: llamadas +{} reenviadas +{} | sin ventana propia +{} sin canal +{} envío fallido +{} | \
                 tecla Windows vista +{} reenviada +{} | captura={grab} hook={:?} | llamada más lenta {max_ms:.1} ms",
                d[0], d[1], d[2], d[3], d[4], d[5], d[6], health(),
            );
            if max_ms > 100.0 {
                tracing::warn!("el hook tardó {max_ms:.1} ms en una llamada: riesgo de que Windows lo retire");
            }
            last = now;
            last_report = Instant::now();
        }
    }
}

/// Pide al hilo del hook que lo reinstale.
fn rehook() {
    HEALTH.store(Health::Lost as u8, Ordering::SeqCst);
    let tid = HOOK_THREAD.load(Ordering::SeqCst);
    if tid == 0 || unsafe { PostThreadMessageW(tid, WM_REHOOK, WPARAM(0), LPARAM(0)) }.is_err() {
        tracing::error!("no se pudo pedir la reinstalación del hook (hilo {tid})");
    }
}

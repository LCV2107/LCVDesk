//! Captura de teclado de bajo nivel (`WH_KEYBOARD_LL`) para el lado que controla.
//!
//! Mientras controlas el equipo remoto, este hook intercepta **todas** las teclas antes de que
//! Windows las procese en tu propio PC (tecla Windows, Alt+Tab, Ctrl+Esc, Win+flechas, etc.),
//! las suprime localmente y las reenvía al host como scancodes de hardware. Así dejan de abrir
//! el menú Inicio o cambiar de ventana en tu equipo y actúan en el equipo controlado.
//!
//! El hook no se puede enganchar a Ctrl+Alt+Supr (secuencia segura de Windows); eso queda, a
//! propósito, fuera del alcance. Para soltar el control, haz clic fuera de la imagen remota.
//!
//! Enfoque equivalente al "keyboard grab" de RustDesk/AnyDesk.

use lcv_proto::Message;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HC_ACTION,
    KBDLLHOOKSTRUCT, LLKHF_EXTENDED, MSG, SetWindowsHookExW, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// ¿Está el hook instalado y operativo?
static INSTALLED: AtomicBool = AtomicBool::new(false);
/// ¿Debe capturar y suprimir las teclas ahora mismo? (true solo mientras controlas).
static GRAB: AtomicBool = AtomicBool::new(false);

fn sink() -> &'static Mutex<Option<Sender<Message>>> {
    static SINK: OnceLock<Mutex<Option<Sender<Message>>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

/// Instala el hook en un hilo propio con bucle de mensajes. Llamar una vez al arrancar.
pub fn start() {
    std::thread::Builder::new()
        .name("keyhook".into())
        .spawn(|| unsafe {
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("no se pudo instalar el hook de teclado: {e}");
                    return;
                }
            };
            INSTALLED.store(true, Ordering::Relaxed);
            tracing::info!("hook de teclado instalado");
            let _ = hook; // se libera al terminar el proceso
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        })
        .ok();
}

/// ¿El hook está disponible? Si no, el visor usa el reenvío de teclas de egui como respaldo.
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// Activa o desactiva la captura. Se llama cada fotograma según si tienes el control.
pub fn set_grab(on: bool) {
    if GRAB.swap(on, Ordering::Relaxed) != on {
        tracing::info!("captura de teclado {}", if on { "activada" } else { "desactivada" });
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

/// Conecta (o desconecta con `None`) el canal al que el hook envía las teclas capturadas.
pub fn set_sink(tx: Option<Sender<Message>>) {
    *sink().lock().unwrap_or_else(|e| e.into_inner()) = tx;
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && GRAB.load(Ordering::Relaxed) && foreground_is_ours() {
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let m = wparam.0 as u32;
        let down = m == WM_KEYDOWN || m == WM_SYSKEYDOWN;
        let up = m == WM_KEYUP || m == WM_SYSKEYUP;
        if (down || up) && kb.scanCode != 0 {
            let extended = kb.flags.0 & LLKHF_EXTENDED.0 != 0;
            let msg = Message::Key { scancode: kb.scanCode as u16, extended, down };
            if let Ok(guard) = sink().lock() {
                if let Some(tx) = guard.as_ref() {
                    let _ = tx.send(msg);
                }
            }
            // Devolver 1 suprime la tecla en este PC: solo actúa en el equipo remoto.
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

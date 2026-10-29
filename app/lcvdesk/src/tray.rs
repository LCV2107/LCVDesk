//! Icono en la bandeja de Windows. Cerrar la ventana no cierra LCVDesk: se oculta y sigue
//! recibiendo conexiones (como AnyDesk). Desde el icono se vuelve a abrir o se sale del todo.
//!
//! Un hilo propio crea una ventana oculta (solo para recibir los avisos del icono) con su bucle de
//! mensajes. La ventana principal de eframe se oculta y se muestra con `ShowWindow` directamente:
//! mientras está oculta, eframe no dibuja y no atendería órdenes de su propia cola.

use crate::host::Host;
use eframe::egui;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::time::Duration;
use tracing::{info, warn};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::AttachThreadInput;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BringWindowToTop, CreatePopupMenu, FLASHW_ALL, FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx,
    GetForegroundWindow, GetWindowThreadProcessId, HWND_NOTOPMOST, HWND_TOPMOST, MB_ICONQUESTION, SWP_NOMOVE,
    SWP_NOSIZE, SWP_SHOWWINDOW, SetWindowPos, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW, GetCursorPos,
    GetMessageW, GetSystemMetrics, HICON, IMAGE_ICON, IsIconic, KillTimer, LR_SHARED, LoadImageW, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, MSG, PostMessageW, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON,
    SW_HIDE, SW_RESTORE, SW_SHOW, SendMessageW, SetForegroundWindow, SetMenuDefaultItem, SetTimer, ShowWindow,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CONTEXTMENU,
    WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Aviso del icono (clics) hacia la ventana oculta.
const WM_TRAY: u32 = WM_APP + 1;
/// Quitar el icono antes de salir (se envía desde el hilo de la interfaz).
const WM_TRAY_REMOVE: u32 = WM_APP + 2;
const ICON_ID: u32 = 1;
const CMD_OPEN: usize = 1;
const CMD_QUIT: usize = 2;
const CMD_DISCONNECT: usize = 3;

/// Ventana principal (eframe) y ventana oculta del icono.
static MAIN: AtomicIsize = AtomicIsize::new(0);
static TRAY: AtomicIsize = AtomicIsize::new(0);
/// El icono está puesto: cerrar la ventana la oculta en vez de salir.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Salida pedida (menú «Salir», actualización): el cierre no se convierte en ocultar.
static QUITTING: AtomicBool = AtomicBool::new(false);
/// Mensaje que Windows difunde cuando el Explorador se reinicia (hay que volver a poner el icono).
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static STATE: OnceLock<State> = OnceLock::new();

struct State {
    host: Host,
    ctx: egui::Context,
    name: String,
    ip: String,
}

/// Pone el icono en la bandeja. `main` es la ventana de eframe.
pub fn start(main: isize, host: Host, ctx: egui::Context, name: String, ip: String) {
    MAIN.store(main, Ordering::SeqCst);
    if STATE.set(State { host, ctx, name, ip }).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(|| {
            if let Err(e) = run() {
                warn!("bandeja: no se pudo poner el icono ({e}); cerrar la ventana cerrará LCVDesk");
            }
        })
        .ok();
}

/// ¿Está el icono? Si no (falló al crearlo), cerrar la ventana debe cerrar la app.
pub fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// ¿Se pidió salir de verdad?
pub fn quitting() -> bool {
    QUITTING.load(Ordering::SeqCst)
}

/// Sale de LCVDesk (no a la bandeja): menú «Salir», instalar una actualización.
pub fn quit(ctx: &egui::Context) {
    QUITTING.store(true, Ordering::SeqCst);
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    ctx.request_repaint();
}

/// Oculta la ventana principal. La primera vez explica que LCVDesk sigue en la bandeja.
pub fn hide_main() {
    let main = HWND(MAIN.load(Ordering::SeqCst) as _);
    let _ = unsafe { ShowWindow(main, SW_HIDE) };
    info!("ventana oculta: LCVDesk sigue en la bandeja");
    if !crate::settings::load_tray_hint_shown() {
        crate::settings::save_tray_hint_shown();
        balloon(
            "LCVDesk sigue abierto",
            "Sigue recibiendo conexiones desde la bandeja. Para cerrarlo del todo: clic derecho en el icono → Salir.",
        );
    }
}

/// Muestra y trae al frente la ventana principal.
pub fn show_main() {
    let main = HWND(MAIN.load(Ordering::SeqCst) as _);
    unsafe {
        let _ = ShowWindow(main, SW_SHOW);
        if IsIconic(main).as_bool() {
            let _ = ShowWindow(main, SW_RESTORE);
        }
        let _ = SetForegroundWindow(main);
    }
    if let Some(s) = STATE.get() {
        s.ctx.request_repaint();
    }
}

/// Llega una solicitud de conexión: la ventana sale de la bandeja, se pone al frente y por encima de
/// todo hasta que se responda (`alert_done`), con el sonido de aviso de Windows.
pub fn alert() {
    let main = HWND(MAIN.load(Ordering::SeqCst) as _);
    if main.0.is_null() {
        return;
    }
    unsafe {
        let _ = ShowWindow(main, SW_SHOW);
        if IsIconic(main).as_bool() {
            let _ = ShowWindow(main, SW_RESTORE);
        }
        let _ = SetWindowPos(main, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
        // Windows no deja que un programa en segundo plano se ponga delante sin más: se une un
        // momento la entrada del hilo de la ventana con la del que está en primer plano.
        let fg = GetForegroundWindow();
        let (ours, theirs) = (GetWindowThreadProcessId(main, None), GetWindowThreadProcessId(fg, None));
        let attached = theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, true).as_bool();
        let _ = BringWindowToTop(main);
        let focused = SetForegroundWindow(main).as_bool();
        if attached {
            let _ = AttachThreadInput(ours, theirs, false);
        }
        if !focused {
            // Si aun así Windows no lo permite, al menos parpadea en la barra de tareas.
            let fw = FLASHWINFO {
                cbSize: size_of::<FLASHWINFO>() as u32,
                hwnd: main,
                dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
                uCount: 0,
                dwTimeout: 0,
            };
            let _ = FlashWindowEx(&fw);
        }
        let _ = MessageBeep(MB_ICONQUESTION);
    }
    if let Some(s) = STATE.get() {
        s.ctx.request_repaint();
    }
}

/// Ya se respondió a la solicitud: la ventana deja de estar por encima de todo.
pub fn alert_done() {
    let main = HWND(MAIN.load(Ordering::SeqCst) as _);
    if !main.0.is_null() {
        let _ = unsafe { SetWindowPos(main, Some(HWND_NOTOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE) };
    }
    if let Some(s) = STATE.get() {
        s.ctx.request_repaint();
    }
}

/// Quita el icono (al terminar). Se espera a que el hilo del icono lo haga.
pub fn remove() {
    let tray = TRAY.load(Ordering::SeqCst);
    if tray != 0 {
        unsafe { SendMessageW(HWND(tray as _), WM_TRAY_REMOVE, None, None) };
    }
}

fn run() -> windows::core::Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("LCVDeskTray");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: class, ..Default::default() };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_thread());
        }
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::SeqCst);
        // Ventana de nivel superior que nunca se muestra (las ventanas «solo mensajes» no reciben
        // TaskbarCreated). Sin título, para que no la confunda la búsqueda de la ventana «LCVDesk».
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR::null(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        TRAY.store(hwnd.0 as isize, Ordering::SeqCst);
        add_icon(hwnd)?;
        ACTIVE.store(true, Ordering::SeqCst);
        info!("bandeja: icono puesto");
        // Texto del icono al día (quién te controla), cada 2 s.
        SetTimer(Some(hwnd), 1, 2000, None);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn add_icon(hwnd: HWND) -> windows::core::Result<()> {
    let mut nid = base(hwnd);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = app_icon();
    copy_wide(&mut nid.szTip, &tooltip());
    if unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool() { Ok(()) } else { Err(windows::core::Error::from_thread()) }
}

fn base(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: ICON_ID, ..Default::default() }
}

/// El icono de la aplicación incrustado en el .exe (recurso 1, lo pone build.rs), en tamaño pequeño.
fn app_icon() -> HICON {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        LoadImageW(Some(instance.into()), PCWSTR(1 as _), IMAGE_ICON, cx, cy, LR_SHARED)
            .map(|h| HICON(h.0))
            .unwrap_or_default()
    }
}

fn tooltip() -> String {
    let Some(s) = STATE.get() else { return "LCVDesk".into() };
    match s.host.state().session.as_ref() {
        Some(session) => format!("LCVDesk — «{}» está controlando este equipo", session.name),
        None => format!("LCVDesk — {} ({})", s.name, s.ip),
    }
}

fn balloon(title: &str, text: &str) {
    let tray = TRAY.load(Ordering::SeqCst);
    if tray == 0 {
        return;
    }
    let mut nid = base(HWND(tray as _));
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO;
    copy_wide(&mut nid.szInfoTitle, title);
    copy_wide(&mut nid.szInfo, text);
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

/// Copia texto a un búfer UTF-16 de tamaño fijo, recortándolo y terminándolo en 0.
fn copy_wide(dst: &mut [u16], text: &str) {
    let src: Vec<u16> = text.encode_utf16().take(dst.len() - 1).collect();
    dst[..src.len()].copy_from_slice(&src);
    dst[src.len()] = 0;
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => show_main(),
                WM_RBUTTONUP | WM_CONTEXTMENU => menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let mut nid = base(hwnd);
            nid.uFlags = NIF_TIP;
            copy_wide(&mut nid.szTip, &tooltip());
            let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
            LRESULT(0)
        }
        WM_TRAY_REMOVE => {
            unsafe {
                let _ = KillTimer(Some(hwnd), 1);
                let _ = Shell_NotifyIconW(NIM_DELETE, &base(hwnd));
            }
            ACTIVE.store(false, Ordering::SeqCst);
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::SeqCst) => {
            // El Explorador se reinició: el icono desapareció con él.
            let _ = add_icon(hwnd);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Menú del clic derecho: estado, abrir, desconectar (si te controlan) y salir.
fn menu(hwnd: HWND) {
    let Some(s) = STATE.get() else { return };
    let controlled_by = s.host.state().session.as_ref().map(|x| x.name.clone());
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let status = match &controlled_by {
            Some(name) => format!("● «{name}» está controlando este equipo"),
            None => format!("{} · {}", s.name, s.ip),
        };
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &HSTRING::from(status));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("Abrir LCVDesk"));
        if let Some(name) = &controlled_by {
            let _ = AppendMenuW(menu, MF_STRING, CMD_DISCONNECT, &HSTRING::from(format!("Desconectar a «{name}»")));
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Salir"));
        let _ = SetMenuDefaultItem(menu, CMD_OPEN as u32, 0);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Sin esto, el menú no se cierra al hacer clic fuera (comportamiento documentado de Windows).
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None).0 as usize;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        match cmd {
            CMD_OPEN => show_main(),
            CMD_DISCONNECT => s.host.disconnect(),
            CMD_QUIT => quit_from_tray(hwnd, &s.ctx),
            _ => {}
        }
    }
}

/// «Salir» con la ventana oculta: eframe no dibuja mientras está oculta, así que se le pide cerrar
/// con un WM_CLOSE de verdad. Si en 3 s no se ha cerrado, se termina el proceso igualmente.
fn quit_from_tray(hwnd: HWND, ctx: &egui::Context) {
    info!("bandeja: salir");
    QUITTING.store(true, Ordering::SeqCst);
    unsafe {
        let _ = KillTimer(Some(hwnd), 1);
        let _ = Shell_NotifyIconW(NIM_DELETE, &base(hwnd));
    }
    ACTIVE.store(false, Ordering::SeqCst);
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    let main = HWND(MAIN.load(Ordering::SeqCst) as _);
    let _ = unsafe { PostMessageW(Some(main), windows::Win32::UI::WindowsAndMessaging::WM_CLOSE, WPARAM(0), LPARAM(0)) };
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(3));
        warn!("bandeja: la ventana no se cerró en 3 s; terminando el proceso");
        std::process::exit(0);
    });
}

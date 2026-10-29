// Sin consola en release: es una aplicación de ventana.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;
mod discovery;
mod host;
mod identity;
mod keyhook;
mod keymap;
mod plugins;
mod recents;
mod settings;
mod uac;
mod ui;
mod updater;
mod viewer;

use std::sync::Mutex;

fn main() -> anyhow::Result<()> {
    // Antes de crear ventanas o duplicar la salida DXGI: coordenadas reales, sin virtualizar por DPI.
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
        // Identidad estable en la barra de tareas: así el anclaje sobrevive a las actualizaciones.
        let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(windows::core::w!("LCVDesk.App"));
    }
    init_logging();
    tracing::info!("LCVDesk {} iniciado", updater::current_version());

    // Instancia única: si LCVDesk ya está abierto (p. ej. tras un relanzado por actualización o un
    // doble clic), la segunda copia trae al frente la ventana existente y sale, en vez de pelearse
    // por los puertos 47070/47071 y fallar con el error 10048. Comportamiento tipo AnyDesk.
    // Solo en compilaciones de desarrollo: permite abrir una segunda copia (con otro LCVDESK_PORT)
    // junto al LCVDesk instalado, para probar la interfaz sin cerrarlo.
    let dev_multi = cfg!(debug_assertions) && std::env::var_os("LCVDESK_DEV_MULTI").is_some();
    if !dev_multi && already_running() {
        tracing::info!("ya hay una instancia en marcha; traigo su ventana al frente y salgo");
        focus_existing_window();
        return Ok(());
    }

    // Hook de teclado de bajo nivel para reenviar las teclas del sistema (tecla Windows, Alt+Tab…)
    // al equipo remoto mientras controlas, en vez de que las procese este PC.
    keyhook::start();

    let port = env_or("LCVDESK_PORT", lcv_proto::DEFAULT_PORT);
    let host = host::Host::start(port);
    let discovery = discovery::Discovery::start(
        identity::machine_id(),
        identity::machine_name(),
        port,
        host.busy_flag(),
    );
    ui::run(host, discovery)
}

/// Variable de entorno numérica con valor por defecto (útil para pruebas de rendimiento).
pub fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// ¿Hay ya otra instancia de LCVDesk en marcha? Se comprueba con un mutex con nombre de sesión:
/// la primera instancia lo crea y lo conserva; una segunda ve que ya existe.
fn already_running() -> bool {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::w;
    unsafe {
        match CreateMutexW(None, false, w!("Local\\LCVDesk.SingleInstance.Mutex")) {
            Ok(_handle) => {
                // No cerramos el handle a propósito: así el mutex sigue vivo toda la ejecución
                // (la versión de `windows` no llama a `CloseHandle` al soltar el valor).
                GetLastError() == ERROR_ALREADY_EXISTS
            }
            // Si no se pudiese crear el mutex, no bloqueamos el arranque por precaución.
            Err(_) => false,
        }
    }
}

/// Trae al frente la ventana de la instancia que ya está en marcha (restaurándola si está minimizada).
fn focus_existing_window() {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };
    use windows::core::{PCWSTR, w};
    unsafe {
        if let Ok(hwnd) = FindWindowW(PCWSTR::null(), w!("LCVDesk")) {
            if !hwnd.0.is_null() {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}

/// Registro en `%LOCALAPPDATA%\LCVDesk\lcvdesk.log` (se reinicia en cada arranque).
fn init_logging() {
    let dir = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("LCVDesk");
    let _ = std::fs::create_dir_all(&dir);
    let Ok(file) = std::fs::File::create(dir.join("lcvdesk.log")) else { return };
    tracing_subscriber::fmt().with_writer(Mutex::new(file)).with_ansi(false).with_thread_names(true).init();
    std::panic::set_hook(Box::new(|info| tracing::error!("pánico: {info}")));
}

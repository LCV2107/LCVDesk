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
mod tray;
mod uac;
mod ui;
mod updater;
mod viewer;

use std::path::PathBuf;
use std::sync::Mutex;

/// Nombre visible del programa: ventana, bandeja, instalador y carpetas.
pub const APP_NAME: &str = "IGPDesk";
/// Nombre anterior: se usa solo para traer los datos de las versiones que se llamaban así.
const OLD_NAME: &str = "LCVDesk";

fn main() -> anyhow::Result<()> {
    // Antes de crear ventanas o duplicar la salida DXGI: coordenadas reales, sin virtualizar por DPI.
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
        // Identidad estable en la barra de tareas: así el anclaje sobrevive a las actualizaciones.
        let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(windows::core::w!("LCVDesk.App"));
    }
    let migrated = migrate_data_dir();
    init_logging();
    tracing::info!("{APP_NAME} {} iniciado", updater::current_version());
    if migrated {
        tracing::info!("datos copiados desde %LOCALAPPDATA%\\{OLD_NAME}");
    }

    // Instancia única: si el programa ya está abierto (p. ej. tras un relanzado por actualización o un
    // doble clic), la segunda copia trae al frente la ventana existente y sale, en vez de pelearse
    // por los puertos 47070/47071 y fallar con el error 10048. Comportamiento tipo AnyDesk.
    // Solo en compilaciones de desarrollo: permite abrir una segunda copia (con otro LCVDESK_PORT)
    // junto a la versión instalada, para probar la interfaz sin cerrarlo.
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

/// ¿Hay ya otra instancia en marcha? Se comprueba con un mutex con nombre de sesión:
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

/// Trae al frente la ventana de la instancia que ya está en marcha: la muestra si estaba oculta en la
/// bandeja y la restaura si estaba minimizada.
fn focus_existing_window() {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };
    use windows::core::PCWSTR;
    unsafe {
        if let Ok(hwnd) = FindWindowW(PCWSTR::null(), &windows::core::HSTRING::from(APP_NAME)) {
            if !hwnd.0.is_null() {
                let _ = ShowWindow(hwnd, SW_SHOW);
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}

/// Carpeta de datos del programa: `%LOCALAPPDATA%\IGPDesk` (identidad, contraseñas, equipos, ajustes).
pub fn data_dir() -> PathBuf {
    local_app_data().join(APP_NAME)
}

fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// La primera vez tras el cambio de nombre copia `%LOCALAPPDATA%\LCVDesk` a la carpeta nueva, para que
/// el equipo conserve su identidad, las contraseñas guardadas y la lista de equipos. Copia en vez de
/// mover: si se vuelve a una versión anterior (que aún usa la carpeta vieja), sigue teniendo sus datos.
fn migrate_data_dir() -> bool {
    let (old, new) = (local_app_data().join(OLD_NAME), data_dir());
    if new.exists() || !old.is_dir() {
        return false;
    }
    copy_dir(&old, &new).is_ok()
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if entry.file_name() != "lcvdesk.log" {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Registro en `%LOCALAPPDATA%\IGPDesk\igpdesk.log` (se reinicia en cada arranque).
fn init_logging() {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let Ok(file) = std::fs::File::create(dir.join("igpdesk.log")) else { return };
    tracing_subscriber::fmt().with_writer(Mutex::new(file)).with_ansi(false).with_thread_names(true).init();
    std::panic::set_hook(Box::new(|info| tracing::error!("pánico: {info}")));
}

// Sin consola en release: es una aplicación de ventana.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod host;
mod keymap;
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
    }
    init_logging();
    tracing::info!("LCVDesk {} iniciado", env!("CARGO_PKG_VERSION"));

    let port = env_or("LCVDESK_PORT", lcv_proto::DEFAULT_PORT);
    let host = host::Host::start(port);
    ui::run(host)
}

/// Variable de entorno numérica con valor por defecto (útil para pruebas de rendimiento).
pub fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
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

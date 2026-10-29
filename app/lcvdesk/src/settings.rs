//! Preferencias ligeras (tema, canal, sonido, complementos): un archivo por ajuste en `%LOCALAPPDATA%\LCVDesk`.

use std::path::PathBuf;

/// Tema guardado ("system", "light" u "dark"); "system" si no hay nada.
pub fn load_theme() -> String {
    std::fs::read_to_string(file_in_dir("theme")).map(|s| s.trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "system".into())
}

pub fn save_theme(value: &str) {
    write_file("theme", value);
}

fn channel_file() -> PathBuf {
    file_in_dir("channel")
}

/// Canal de actualizaciones guardado ("stable" o "beta"); "stable" por defecto.
pub fn load_channel() -> String {
    std::fs::read_to_string(channel_file()).map(|s| s.trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "stable".into())
}

pub fn save_channel(value: &str) {
    write_file("channel", value);
}

/// ¿Reproducir el sonido del equipo remoto? Activado por defecto.
pub fn load_audio() -> bool {
    std::fs::read_to_string(file_in_dir("audio")).map(|s| s.trim() != "off").unwrap_or(true)
}

pub fn save_audio(on: bool) {
    write_file("audio", if on { "on" } else { "off" });
}

fn file_in_dir(name: &str) -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("LCVDesk")
        .join(name)
}

fn write_file(name: &str, value: &str) {
    let path = file_in_dir(name);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, value);
}

/// ¿Permitir que quien controla este equipo abra complementos (IGP_Soft)? Activado por defecto.
pub fn load_plugins_allowed() -> bool {
    std::fs::read_to_string(file_in_dir("plugins")).map(|s| s.trim() != "off").unwrap_or(true)
}

pub fn save_plugins_allowed(on: bool) {
    write_file("plugins", if on { "on" } else { "off" });
}

/// ¿Ya se explicó (una vez) que cerrar la ventana deja LCVDesk en la bandeja?
pub fn load_tray_hint_shown() -> bool {
    file_in_dir("tray_hint").exists()
}

pub fn save_tray_hint_shown() {
    write_file("tray_hint", "shown");
}

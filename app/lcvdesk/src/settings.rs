//! Preferencias ligeras (tema, canal, sonido, complementos, vista): un archivo por ajuste en `%LOCALAPPDATA%\LCVDesk`.

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

/// Vista guardada para un equipo remoto («fit», «width» o un porcentaje); vacía si no hay ninguna.
/// Archivo `views`, una línea `id=vista` por equipo.
pub fn load_view(host_id: &str) -> String {
    let text = std::fs::read_to_string(file_in_dir("views")).unwrap_or_default();
    text.lines().find_map(|l| l.strip_prefix(host_id)?.strip_prefix('=')).unwrap_or("").trim().to_string()
}

pub fn save_view(host_id: &str, view: &str) {
    let text = std::fs::read_to_string(file_in_dir("views")).unwrap_or_default();
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty() && l.split_once('=').is_none_or(|(id, _)| id != host_id))
        .map(str::to_string)
        .collect();
    lines.push(format!("{host_id}={view}"));
    write_file("views", &lines.join("\n"));
}

/// Tamaño del puntero remoto (1, 1.5 o 2 veces su tamaño real); 1 por defecto.
pub fn load_cursor_scale() -> f32 {
    std::fs::read_to_string(file_in_dir("cursor_scale"))
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|k| (1.0..=2.0).contains(k))
        .unwrap_or(1.0)
}

pub fn save_cursor_scale(k: f32) {
    write_file("cursor_scale", &k.to_string());
}

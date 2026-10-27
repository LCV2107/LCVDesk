//! Lista de equipos recientes/guardados en `%LOCALAPPDATA%\LCVDesk\devices.json`.
//!
//! Cada conexión con éxito se guarda automáticamente. La clave es el identificador de equipo
//! (estable), así que se reconoce aunque cambie la IP. Se pueden marcar favoritos o quitar.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedDevice {
    pub id: String,
    pub name: String,
    /// Última dirección usada para conectar (`ip:puerto` o nombre).
    pub addr: String,
    #[serde(default)]
    pub favorite: bool,
    /// Marca de tiempo Unix (segundos) de la última conexión.
    #[serde(default)]
    pub last_connected: i64,
}

fn file() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("LCVDesk")
        .join("devices.json")
}

/// Carga la lista, ordenada: primero favoritos, luego por conexión más reciente.
pub fn load() -> Vec<SavedDevice> {
    let mut list: Vec<SavedDevice> =
        std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    list.sort_by(|a, b| b.favorite.cmp(&a.favorite).then(b.last_connected.cmp(&a.last_connected)));
    list
}

fn write(list: &[SavedDevice]) {
    let path = file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(list) {
        let _ = std::fs::write(&path, json);
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Registra (o actualiza) una conexión con éxito.
pub fn record(id: &str, name: &str, addr: &str) {
    let mut list = load();
    if let Some(d) = list.iter_mut().find(|d| d.id == id) {
        d.name = name.to_string();
        d.addr = addr.to_string();
        d.last_connected = now();
    } else {
        list.push(SavedDevice {
            id: id.to_string(),
            name: name.to_string(),
            addr: addr.to_string(),
            favorite: false,
            last_connected: now(),
        });
    }
    write(&list);
}

/// Quita un equipo de la lista.
pub fn remove(id: &str) {
    let mut list = load();
    list.retain(|d| d.id != id);
    write(&list);
}

/// Marca o desmarca un equipo como favorito.
pub fn set_favorite(id: &str, favorite: bool) {
    let mut list = load();
    if let Some(d) = list.iter_mut().find(|d| d.id == id) {
        d.favorite = favorite;
        write(&list);
    }
}

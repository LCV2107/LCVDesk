//! Identidad persistente del equipo: un identificador estable usado por el descubrimiento en red y
//! por la lista de "Recientes". No es secreto; solo sirve para reconocer al mismo equipo aunque
//! cambie de IP.

use std::path::PathBuf;
use std::sync::OnceLock;

fn id_file() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("LCVDesk")
        .join("identity")
}

/// Identificador del equipo (16 bytes en hexadecimal). Se crea una vez y se reutiliza.
pub fn machine_id() -> String {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let path = id_file();
        if let Ok(s) = std::fs::read_to_string(&path) {
            let s = s.trim().to_string();
            if s.len() >= 8 {
                return s;
            }
        }
        let id: String = crate::auth::random_nonce().iter().take(16).map(|b| format!("{b:02x}")).collect();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&path, &id);
        id
    })
    .clone()
}

/// Nombre visible del equipo (el nombre de host de Windows).
pub fn machine_name() -> String {
    hostname::get().map(|h| h.to_string_lossy().into_owned()).unwrap_or_else(|_| "Equipo".into())
}

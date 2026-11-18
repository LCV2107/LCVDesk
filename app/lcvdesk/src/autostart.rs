//! Arranque con Windows: un valor en `HKCU\...\CurrentVersion\Run` que abre IGPDesk en la bandeja al
//! iniciar sesión, para que el equipo pueda recibir conexiones sin que nadie lo abra a mano.
//!
//! Es por usuario y no necesita permisos de administrador. Cubre el inicio de sesión, no la pantalla
//! de bloqueo: antes de iniciar sesión no hay escritorio donde abrir el programa.

use crate::APP_NAME;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegDeleteValueW, RegGetValueW, RegOpenKeyExW,
    RegSetValueExW,
};
use windows::core::HSTRING;

const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// Argumento con el que se arranca desde el registro: abrir directamente en la bandeja.
pub const TRAY_ARG: &str = "--bandeja";

/// ¿Se pidió arrancar en la bandeja (inicio de sesión)?
pub fn started_in_tray() -> bool {
    std::env::args().any(|a| a == TRAY_ARG)
}

/// Aplica el ajuste guardado al arrancar. Si está activado, reescribe la ruta del ejecutable (por si
/// cambió) y, si no, quita el valor. Las compilaciones de desarrollo no tocan el registro.
pub fn sync() {
    if cfg!(debug_assertions) {
        return;
    }
    let on = crate::settings::load_autostart();
    if let Err(e) = set(on) {
        tracing::warn!("arranque con Windows: no se pudo {} ({e})", if on { "activar" } else { "desactivar" });
    }
}

/// Activa o desactiva el arranque con Windows en el registro.
pub fn set(on: bool) -> windows::core::Result<()> {
    let key = open_run()?;
    let name = HSTRING::from(APP_NAME);
    let r = if on {
        let exe = std::env::current_exe().map_err(|e| windows::core::Error::new(windows::core::HRESULT(-1), e.to_string()))?;
        let command = format!("\"{}\" {TRAY_ARG}", exe.display());
        let data: Vec<u8> = command.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        unsafe { RegSetValueExW(key, &name, None, REG_SZ, Some(&data)) }
    } else {
        match unsafe { RegDeleteValueW(key, &name) } {
            ERROR_FILE_NOT_FOUND => ERROR_SUCCESS,
            r => r,
        }
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    r.ok()
}

/// ¿Está el valor en el registro? (Lo que Windows hará de verdad al iniciar sesión.)
pub fn is_registered() -> bool {
    let (subkey, name) = (HSTRING::from(RUN), HSTRING::from(APP_NAME));
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, &subkey, &name, RRF_RT_REG_SZ, None, None, Some(&mut size)) == ERROR_SUCCESS }
}

fn open_run() -> windows::core::Result<HKEY> {
    let mut key = HKEY::default();
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(RUN), None, KEY_SET_VALUE, &mut key) }.ok()?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Escribe y borra el valor real de `Run` (deja el registro como estaba).
    /// `cargo test -p lcvdesk autostart_roundtrip -- --ignored`
    #[test]
    #[ignore]
    fn autostart_roundtrip() {
        let before = is_registered();
        set(true).unwrap();
        assert!(is_registered());
        set(false).unwrap();
        assert!(!is_registered());
        set(false).unwrap(); // borrar lo que no existe no es un error
        if before {
            set(true).unwrap();
        }
    }
}

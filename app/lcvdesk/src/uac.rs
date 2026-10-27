//! Casilla opcional: mostrar los diálogos de UAC en el escritorio normal.
//!
//! Windows dibuja UAC (y la pantalla de bloqueo) en el "escritorio seguro", que ningún programa remoto
//! puede ver ni tocar. Poniendo `PromptOnSecureDesktop = 0` los diálogos de UAC salen en el escritorio
//! normal, donde LCVDesk sí los transmite y puedes hacer clic en «Sí». **Reduce una protección de
//! Windows**, así que es una opción que el usuario activa a conciencia y puede revertir.
//!
//! No afecta a la pantalla de bloqueo ni al inicio de sesión: esos siguen en el escritorio seguro.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HWND};
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

const SUBKEY: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Policies\\System";

/// Estado de la función (no del valor crudo del registro):
/// `Some(true)` = UAC en escritorio normal (función activada), `Some(false)` = escritorio seguro
/// (comportamiento por defecto de Windows), `None` = no se pudo leer.
pub fn is_enabled() -> Option<bool> {
    let subkey: Vec<u16> = OsStr::new(SUBKEY).encode_wide().chain([0]).collect();
    let value = w!("PromptOnSecureDesktop");
    let mut data = 0u32;
    let mut size = size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    // RegGetValueW devuelve un WIN32_ERROR, no un Result.
    match r {
        ERROR_SUCCESS => Some(data == 0),
        // Si el valor no existe, Windows usa el escritorio seguro: función desactivada.
        ERROR_FILE_NOT_FOUND => Some(false),
        _ => None,
    }
}

/// Activa o desactiva la función. Lanza `reg.exe` elevado → Windows muestra una solicitud de UAC al
/// usuario local (una sola vez). Devuelve `false` si el usuario cancela esa elevación.
pub fn set(enable: bool) -> bool {
    // enable → PromptOnSecureDesktop = 0 (UAC en escritorio normal); restaurar → = 1.
    let value = if enable { "0" } else { "1" };
    let args = format!(
        "add \"HKLM\\{SUBKEY}\" /v PromptOnSecureDesktop /t REG_DWORD /d {value} /f"
    );
    let args: Vec<u16> = OsStr::new(&args).encode_wide().chain([0]).collect();
    let result = unsafe {
        ShellExecuteW(
            Some(HWND::default()),
            w!("runas"),
            w!("reg.exe"),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW devuelve >32 si tuvo éxito.
    result.0 as usize > 32
}

//! Autenticación por contraseña para el lado controlado (host).
//!
//! Esquema reto-respuesta: el host envía un `nonce` aleatorio y el visor responde con
//! `HMAC-SHA256(contraseña, nonce)`. La contraseña **nunca viaja por la red**, ni siquiera cifrada,
//! así que funciona aunque el canal todavía no tenga cifrado E2E (Fase 4.4).
//!
//! La contraseña del host se guarda en `%LOCALAPPDATA%\LCVDesk\access.bin` protegida con DPAPI
//! (ligada al usuario de Windows): otro usuario u otra máquina no pueden descifrarla.

use sha2::{Digest, Sha256};
use std::path::PathBuf;

const BLOCK: usize = 64;

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}

/// `HMAC-SHA256(key, msg)` implementado a mano para no arrastrar más dependencias.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&sha256(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

/// Comparación en tiempo constante (no revela en qué byte difieren).
pub fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// 32 bytes aleatorios del generador del sistema.
pub fn random_nonce() -> Vec<u8> {
    use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
    let mut buf = [0u8; 32];
    let status = unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_ok() {
        return buf.to_vec();
    }
    // Respaldo poco probable: mezcla de reloj y direcciones; suficiente para que el nonce sea único.
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let seed = (t as u64) ^ (&buf as *const _ as u64) ^ (std::process::id() as u64);
    sha256(&seed.to_le_bytes()).to_vec()
}

fn access_file() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("LCVDesk")
        .join("access.bin")
}

/// ¿El host tiene una contraseña de acceso configurada?
pub fn has_password() -> bool {
    access_file().metadata().map(|m| m.len() > 0).unwrap_or(false)
}

/// Devuelve la contraseña configurada (descifrada con DPAPI), o `None` si no hay o falló.
pub fn load_password() -> Option<String> {
    let blob = std::fs::read(access_file()).ok()?;
    let plain = unprotect(&blob)?;
    String::from_utf8(plain).ok()
}

/// Guarda (o reemplaza) la contraseña de acceso, cifrada con DPAPI.
pub fn save_password(password: &str) -> bool {
    let path = access_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match protect(password.as_bytes()) {
        Some(blob) => std::fs::write(path, blob).is_ok(),
        None => false,
    }
}

/// Elimina la contraseña de acceso (vuelve a pedir aceptación manual siempre).
pub fn clear_password() -> bool {
    let path = access_file();
    match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

// --- Contraseñas recordadas por equipo (para "entrar automáticamente") ---
// Se guardan en %LOCALAPPDATA%\LCVDesk\passwords.json como { id_equipo: blob_DPAPI_en_hex }.

fn creds_file() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("LCVDesk")
        .join("passwords.json")
}

fn load_creds() -> std::collections::HashMap<String, String> {
    std::fs::read(creds_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_creds(map: &std::collections::HashMap<String, String>) {
    let path = creds_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(map) {
        let _ = std::fs::write(path, json);
    }
}

/// Guarda la contraseña de un equipo (cifrada con DPAPI) para no volver a pedirla.
pub fn remember_password(id: &str, password: &str) {
    if id.is_empty() {
        return;
    }
    if let Some(blob) = protect(password.as_bytes()) {
        let mut map = load_creds();
        map.insert(id.to_string(), to_hex(&blob));
        save_creds(&map);
    }
}

/// Devuelve la contraseña recordada de un equipo, si la hay.
pub fn recall_password(id: &str) -> Option<String> {
    if id.is_empty() {
        return None;
    }
    let hex = load_creds().remove(id)?;
    let blob = from_hex(&hex)?;
    String::from_utf8(unprotect(&blob)?).ok()
}

/// Olvida la contraseña recordada de un equipo.
pub fn forget_password(id: &str) {
    let mut map = load_creds();
    if map.remove(id).is_some() {
        save_creds(&map);
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

fn protect(plain: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData};
    use windows::core::PCWSTR;

    let input = CRYPT_INTEGER_BLOB { cbData: plain.len() as u32, pbData: plain.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(&input, PCWSTR::null(), None, None, None, 0, &mut out).ok()?;
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut _)));
        Some(bytes)
    }
}

fn unprotect(blob: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptUnprotectData};

    let input = CRYPT_INTEGER_BLOB { cbData: blob.len() as u32, pbData: blob.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(&input, None, None, None, None, 0, &mut out).ok()?;
        let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut _)));
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_known_vector() {
        // RFC 4231, caso de prueba 2: key = "Jefe", data = "what do ya want for nothing?"
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let expected =
            hex_lit("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        assert_eq!(mac.to_vec(), expected);
    }

    #[test]
    fn constant_eq_works() {
        assert!(constant_eq(b"abc", b"abc"));
        assert!(!constant_eq(b"abc", b"abd"));
        assert!(!constant_eq(b"abc", b"ab"));
    }

    fn hex_lit(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
}

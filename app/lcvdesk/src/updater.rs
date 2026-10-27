//! Actualización desde GitHub Releases.
//!
//! Comprueba la API de GitHub, compara la versión, descarga el `.msi` publicado, verifica su SHA-256
//! y lo instala encima (actualización mayor vía `msiexec`). Nunca actualiza solo: el usuario decide.
//!
//! El repositorio se toma de la variable `LCVDESK_UPDATE_REPO` o del valor por defecto `DEFAULT_REPO`.

use eframe::egui;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tracing::{info, warn};

/// Repositorio `usuario/repo` donde se publican las versiones. Cámbialo por el tuyo.
const DEFAULT_REPO: &str = "LCV2107/LCVDesk";

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn repo() -> String {
    std::env::var("LCVDESK_UPDATE_REPO").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_REPO.into())
}

#[derive(Clone)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub page: String,
    pub msi_url: String,
    pub sha_url: Option<String>,
    pub size: u64,
}

pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Found(Release),
    Downloading(u8),
    Installing,
    Error(String),
}

pub struct Updater {
    status: Arc<Mutex<Status>>,
    busy: Arc<AtomicBool>,
}

impl Updater {
    pub fn new() -> Self {
        Self { status: Arc::new(Mutex::new(Status::Idle)), busy: Arc::new(AtomicBool::new(false)) }
    }

    pub fn status(&self) -> MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Busca una versión nueva en segundo plano (no bloquea la interfaz).
    pub fn check(&self, ctx: egui::Context) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (status, busy) = (self.status.clone(), self.busy.clone());
        *status.lock().unwrap() = Status::Checking;
        ctx.request_repaint();
        std::thread::Builder::new()
            .name("updater-check".into())
            .spawn(move || {
                let result = fetch_latest();
                *status.lock().unwrap() = match result {
                    Ok(Some(r)) => {
                        info!("versión disponible: {}", r.version);
                        Status::Found(r)
                    }
                    Ok(None) => Status::UpToDate,
                    Err(e) => {
                        warn!("no se pudo comprobar actualizaciones: {e}");
                        Status::Error(e)
                    }
                };
                busy.store(false, Ordering::SeqCst);
                ctx.request_repaint();
            })
            .ok();
    }

    /// Descarga, verifica e instala. Al terminar lanza el instalador elevado y cierra la app.
    pub fn install(&self, release: Release, ctx: egui::Context) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (status, busy) = (self.status.clone(), self.busy.clone());
        let ctx2 = ctx.clone();
        std::thread::Builder::new()
            .name("updater-install".into())
            .spawn(move || {
                let set = |s: Status| {
                    *status.lock().unwrap() = s;
                    ctx2.request_repaint();
                };
                set(Status::Downloading(0));
                match download_and_verify(&release, &status, &ctx2) {
                    Ok(path) => {
                        set(Status::Installing);
                        if let Err(e) = launch_installer(&path) {
                            set(Status::Error(e));
                            busy.store(false, Ordering::SeqCst);
                            return;
                        }
                        // El instalador corre elevado en su propio proceso; cerramos para no bloquear archivos.
                        std::thread::sleep(Duration::from_millis(600));
                        ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Err(e) => {
                        set(Status::Error(e));
                        busy.store(false, Ordering::SeqCst);
                    }
                }
            })
            .ok();
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(600))
        .build()
}

/// Consulta las versiones y devuelve la estable más reciente mayor que la actual que traiga un `.msi`.
fn fetch_latest() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{}/releases?per_page=30", repo());
    let body = agent()
        .get(&url)
        .set("User-Agent", "LCVDesk-Updater")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| describe(e))?
        .into_string()
        .map_err(|e| e.to_string())?;
    let releases: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let current = parse_version(current_version()).ok_or("versión local inválida")?;

    let mut best: Option<(Ver, Release)> = None;
    for r in releases.as_array().into_iter().flatten() {
        if r["draft"].as_bool() == Some(true) || r["prerelease"].as_bool() == Some(true) {
            continue;
        }
        let Some(v) = r["tag_name"].as_str().and_then(parse_version) else { continue };
        if cmp(&v, &current) <= 0 {
            continue;
        }
        if best.as_ref().is_some_and(|(bv, _)| cmp(&v, bv) <= 0) {
            continue;
        }
        let (mut msi_url, mut sha_url, mut size) = (None, None, 0u64);
        for a in r["assets"].as_array().into_iter().flatten() {
            let name = a["name"].as_str().unwrap_or("");
            let dl = a["browser_download_url"].as_str().unwrap_or("").to_string();
            if name.ends_with(".msi") {
                msi_url = Some(dl);
                size = a["size"].as_u64().unwrap_or(0);
            } else if name.ends_with(".sha256") {
                sha_url = Some(dl);
            }
        }
        let Some(msi_url) = msi_url else { continue };
        best = Some((
            v,
            Release {
                version: r["tag_name"].as_str().unwrap_or("").trim_start_matches('v').to_string(),
                notes: r["body"].as_str().unwrap_or("").trim().to_string(),
                page: r["html_url"].as_str().unwrap_or("").to_string(),
                msi_url,
                sha_url,
                size,
            },
        ));
    }
    Ok(best.map(|(_, r)| r))
}

fn download_and_verify(
    release: &Release,
    status: &Arc<Mutex<Status>>,
    ctx: &egui::Context,
) -> Result<std::path::PathBuf, String> {
    let resp = agent()
        .get(&release.msi_url)
        .set("User-Agent", "LCVDesk-Updater")
        .call()
        .map_err(|e| describe(e))?;
    let total = resp
        .header("Content-Length")
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(release.size);

    let dir = std::env::temp_dir().join("LCVDesk");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("LCVDesk-{}-x64.msi", release.version));

    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    let mut done = 0u64;
    let mut last_pct = u8::MAX;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        use std::io::Write;
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        hasher.update(&buf[..n]);
        done += n as u64;
        if total > 0 {
            let pct = ((done * 100 / total) as u8).min(100);
            if pct != last_pct {
                last_pct = pct;
                *status.lock().unwrap() = Status::Downloading(pct);
                ctx.request_repaint();
            }
        }
    }
    drop(file);

    if let Some(sha_url) = &release.sha_url {
        let expected = agent()
            .get(sha_url)
            .set("User-Agent", "LCVDesk-Updater")
            .call()
            .map_err(|e| describe(e))?
            .into_string()
            .map_err(|e| e.to_string())?;
        let expected = expected.split_whitespace().next().unwrap_or("").to_lowercase();
        let actual = hex(&hasher.finalize());
        if expected != actual {
            let _ = std::fs::remove_file(&path);
            return Err("la descarga no coincide con la huella SHA-256 publicada".into());
        }
    } else {
        return Err("la versión publicada no trae su archivo .sha256 de verificación".into());
    }
    Ok(path)
}

/// Lanza `msiexec` elevado para instalar la actualización mayor.
fn launch_installer(path: &std::path::Path) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{PCWSTR, w};

    let args = format!("/i \"{}\" /qb", path.display());
    let args: Vec<u16> = OsStr::new(&args).encode_wide().chain([0]).collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            w!("msiexec.exe"),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize > 32 {
        Ok(())
    } else {
        Err("no se pudo iniciar el instalador (¿se canceló la elevación?)".into())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn describe(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("GitHub respondió {code}"),
        ureq::Error::Transport(t) => format!("sin conexión ({t})"),
    }
}

type Ver = (u32, u32, u32, bool);

/// Analiza "v1.2.3" o "1.2.3-beta". El cuarto campo es true si NO es prerelease (ordena por encima).
fn parse_version(tag: &str) -> Option<Ver> {
    let s = tag.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = s.split_once('-').map_or((s, false), |(c, _)| (c, true));
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch, !pre))
}

fn cmp(a: &Ver, b: &Ver) -> i32 {
    for (x, y) in [(a.0, b.0), (a.1, b.1), (a.2, b.2)] {
        if x != y {
            return if x > y { 1 } else { -1 };
        }
    }
    // Una versión final (a.3 == true) va por encima de la misma con prerelease.
    (a.3 as i32) - (b.3 as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering() {
        let v = |s| parse_version(s).unwrap();
        assert!(cmp(&v("0.3.0"), &v("0.2.0")) > 0);
        assert!(cmp(&v("v1.0.0"), &v("0.9.9")) > 0);
        assert!(cmp(&v("1.2.3"), &v("1.2.3")) == 0);
        assert!(cmp(&v("1.2.3"), &v("1.2.3-beta")) > 0);
        assert!(cmp(&v("1.2.0"), &v("1.10.0")) < 0);
    }
}

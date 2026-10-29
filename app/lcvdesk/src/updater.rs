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
    // En las compilaciones de CI, la versión real viene de la etiqueta (p. ej. 0.2.0-beta.1) vía
    // LCVDESK_VERSION; así el mismo commit sirve para beta y estable. En local, la de Cargo.
    match option_env!("LCVDESK_VERSION") {
        Some(v) if !v.is_empty() => v,
        _ => env!("CARGO_PKG_VERSION"),
    }
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
    /// La actualización es crítica (las notas incluyen la marca `[critical]`): se obliga a instalar.
    pub critical: bool,
    /// Es una vuelta atrás (de una beta a la estable, más antigua): hay que desinstalar antes.
    pub downgrade: bool,
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

    /// Busca una versión nueva en segundo plano (no bloquea la interfaz). `beta` incluye los prereleases.
    pub fn check(&self, ctx: egui::Context, beta: bool) {
        self.spawn_check(ctx, move || fetch_latest(beta));
    }

    /// Busca la última versión estable para volver a ella desde una beta, aunque sea más antigua.
    pub fn check_stable(&self, ctx: egui::Context) {
        self.spawn_check(ctx, fetch_stable);
    }

    fn spawn_check(&self, ctx: egui::Context, fetch: impl FnOnce() -> Result<Option<Release>, String> + Send + 'static) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (status, busy) = (self.status.clone(), self.busy.clone());
        *status.lock().unwrap() = Status::Checking;
        ctx.request_repaint();
        std::thread::Builder::new()
            .name("updater-check".into())
            .spawn(move || {
                let result = fetch();
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
                        if let Err(e) = launch_installer(&path, release.downgrade) {
                            set(Status::Error(e));
                            busy.store(false, Ordering::SeqCst);
                            return;
                        }
                        // El instalador corre elevado en su propio proceso; cerramos para no bloquear archivos.
                        std::thread::sleep(Duration::from_millis(600));
                        crate::tray::quit(&ctx2);
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

/// Consulta las versiones y devuelve la más reciente mayor que la actual que traiga un `.msi`.
/// Con `beta=false` ignora los prereleases; con `beta=true` también los considera.
fn fetch_latest(beta: bool) -> Result<Option<Release>, String> {
    let current = parse_version(current_version()).ok_or("versión local inválida")?;
    let best = list_releases()?
        .into_iter()
        .filter(|(v, _, pre)| (beta || !pre) && cmp(v, &current) > 0)
        .max_by(|(a, ..), (b, ..)| cmp(a, b).cmp(&0));
    Ok(best.map(|(_, r, _)| r))
}

/// La última versión estable, para volver a ella desde una beta. Si es más antigua que la instalada,
/// se marca como vuelta atrás (`downgrade`). `None` si ya estás en ella.
fn fetch_stable() -> Result<Option<Release>, String> {
    let current = parse_version(current_version()).ok_or("versión local inválida")?;
    let best = list_releases()?.into_iter().filter(|(_, _, pre)| !pre).max_by(|(a, ..), (b, ..)| cmp(a, b).cmp(&0));
    Ok(best.filter(|(v, ..)| cmp(v, &current) != 0).map(|(v, mut r, _)| {
        r.downgrade = cmp(&v, &current) < 0;
        // Volver atrás es una elección del usuario: nunca se impone como actualización crítica.
        r.critical &= !r.downgrade;
        r
    }))
}

/// Todas las versiones publicadas (no borradores) con su `.msi` y `.sha256`: (versión, datos, ¿prerelease?).
fn list_releases() -> Result<Vec<(Ver, Release, bool)>, String> {
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

    let mut out = Vec::new();
    for r in releases.as_array().into_iter().flatten() {
        if r["draft"].as_bool() == Some(true) {
            continue;
        }
        let prerelease = r["prerelease"].as_bool() == Some(true);
        let Some(v) = r["tag_name"].as_str().and_then(parse_version) else { continue };
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
        // Exigir ambos: sin el .sha256 no podríamos verificar la descarga, así que se descarta.
        let Some(msi_url) = msi_url else { continue };
        let Some(sha_url) = sha_url else { continue };
        let notes = r["body"].as_str().unwrap_or("").trim().to_string();
        // Una actualización crítica se marca escribiendo [critical] en las notas de la versión.
        let low = notes.to_lowercase();
        let critical = low.contains("[critical]") || low.contains("[critica]") || low.contains("[crítica]");
        out.push((
            v,
            Release {
                version: r["tag_name"].as_str().unwrap_or("").trim_start_matches('v').to_string(),
                notes,
                page: r["html_url"].as_str().unwrap_or("").to_string(),
                msi_url,
                sha_url: Some(sha_url),
                size,
                critical,
                downgrade: false,
            },
            prerelease,
        ));
    }
    Ok(out)
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

/// Código de actualización del MSI (`UpgradeCode` en installer\lcvdesk.wxs).
const UPGRADE_CODE: &str = "{934321C7-3C9D-40DE-B5CF-742C669601B6}";

/// Instala la actualización (elevado) y, al terminar, vuelve a abrir LCVDesk con el nivel del usuario.
///
/// Lo hace un script de PowerShell que primero **espera a que esta instancia se cierre**: si msiexec
/// corre con el .exe aún abierto, no puede reemplazarlo y lo deja para el reinicio (te quedabas en la
/// versión anterior). Con `downgrade` (volver de una beta a la estable, más antigua) desinstala antes
/// la versión actual, porque un MSI no reemplaza archivos por otros de versión menor.
fn launch_installer(path: &std::path::Path, downgrade: bool) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
    use windows::core::{PCWSTR, w};

    let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    let exe = format!("{program_files}\\LCVDesk\\lcvdesk.exe");
    // Cadenas de PowerShell entre comillas simples: la comilla simple se escapa duplicándola.
    let q = |s: &str| s.replace('\'', "''");
    let uninstall = if downgrade {
        format!(
            "$wi = New-Object -ComObject WindowsInstaller.Installer\n\
             $codes = $wi.GetType().InvokeMember('RelatedProducts', 'GetProperty', $null, $wi, @('{UPGRADE_CODE}'))\n\
             foreach ($c in @($codes)) {{ Start-Process msiexec.exe -ArgumentList \"/x $c /qb /norestart\" -Wait }}\n"
        )
    } else {
        String::new()
    };
    let script = format!(
        "$ErrorActionPreference = 'SilentlyContinue'\n\
         $t = 0\n\
         while ((Get-Process -Id {pid}) -and $t -lt 200) {{ Start-Sleep -Milliseconds 300; $t++ }}\n\
         {uninstall}\
         $p = Start-Process msiexec.exe -ArgumentList '/i \"{msi}\" /qb /norestart' -Wait -PassThru\n\
         if ($p.ExitCode -eq 0 -or $p.ExitCode -eq 3010) {{ Start-Process explorer.exe -ArgumentList '\"{exe}\"' }}\n",
        pid = std::process::id(),
        msi = q(&path.display().to_string()),
        exe = q(&exe),
    );
    let script_path = path.with_file_name("lcvdesk-update.ps1");
    // Con BOM, para que PowerShell 5 lea bien rutas con caracteres no ASCII.
    std::fs::write(&script_path, [&[0xEF, 0xBB, 0xBF][..], script.as_bytes()].concat()).map_err(|e| e.to_string())?;

    let args = format!("-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File \"{}\"", script_path.display());
    let args: Vec<u16> = OsStr::new(&args).encode_wide().chain([0]).collect();
    let result = unsafe {
        ShellExecuteW(None, w!("runas"), w!("powershell.exe"), PCWSTR(args.as_ptr()), PCWSTR::null(), SW_HIDE)
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

// (major, minor, patch, orden_de_prerelease). El 4º campo ordena las betas entre sí: una versión
// final es u64::MAX (por encima de cualquier beta); una beta.N vale peso_de_tipo + N.
type Ver = (u32, u32, u32, u64);

/// Analiza "v1.2.3", "1.2.3-beta.3" o "1.2.3-rc.1".
fn parse_version(tag: &str) -> Option<Ver> {
    let s = tag.trim().trim_start_matches(['v', 'V']);
    let (core, pre) = s.split_once('-').map_or((s, ""), |(c, p)| (c, p));
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next().unwrap_or("0").parse().ok()?;
    let pre_order = if pre.is_empty() { u64::MAX } else { prerelease_order(pre) };
    Some((major, minor, patch, pre_order))
}

/// Orden de un identificador de prerelease: rc > beta > alpha, y dentro de cada uno por su número.
fn prerelease_order(pre: &str) -> u64 {
    let low = pre.to_lowercase();
    let base = if low.starts_with("rc") {
        3_000_000
    } else if low.starts_with("beta") {
        2_000_000
    } else if low.starts_with("alpha") {
        1_000_000
    } else {
        0
    };
    // Número al final (beta.3 -> 3, beta.10 -> 10).
    let rev_digits: String = low.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
    let num: u64 = rev_digits.chars().rev().collect::<String>().parse().unwrap_or(0);
    base + num.min(999_999)
}

fn cmp(a: &Ver, b: &Ver) -> i32 {
    for (x, y) in [(a.0, b.0), (a.1, b.1), (a.2, b.2)] {
        if x != y {
            return if x > y { 1 } else { -1 };
        }
    }
    if a.3 != b.3 {
        return if a.3 > b.3 { 1 } else { -1 };
    }
    0
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
        // Las betas se ordenan por su número, y la final va por encima de cualquier beta.
        assert!(cmp(&v("0.3.0-beta.3"), &v("0.3.0-beta.1")) > 0);
        assert!(cmp(&v("0.3.0-beta.10"), &v("0.3.0-beta.2")) > 0);
        assert!(cmp(&v("0.3.0"), &v("0.3.0-beta.9")) > 0);
        assert!(cmp(&v("0.3.1-beta.1"), &v("0.3.0-beta.1")) > 0);
        assert!(cmp(&v("1.0.0-rc.1"), &v("1.0.0-beta.9")) > 0);
    }
}

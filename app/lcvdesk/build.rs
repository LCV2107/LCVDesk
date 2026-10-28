// Recompila el crate cuando cambia LCVDESK_VERSION, para que `option_env!("LCVDESK_VERSION")`
// (la versión tomada de la etiqueta en CI) quede bien incrustada en el binario.
fn main() {
    println!("cargo:rerun-if-env-changed=LCVDESK_VERSION");
    // Incrusta el icono de la aplicación en el .exe (barra de tareas, Explorador, accesos directos).
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        // Versión de archivo creciente también entre betas: sin ella el MSI trata el .exe como
        // "sin versión" y puede no reemplazarlo al actualizar (te quedabas en la versión anterior).
        let version = std::env::var("LCVDESK_VERSION")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_default());
        let packed = file_version(&version);
        res.set_version_info(winresource::VersionInfo::FILEVERSION, packed);
        res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
        res.set("ProductVersion", &version);
        if let Err(e) = res.compile() {
            println!("cargo:warning=no se pudo incrustar el icono/versión: {e}");
        }
    }
}

/// "0.3.1-beta.2" -> 0.3.1.20002 empaquetado en u64. El 4º campo ordena las prerelease:
/// alpha.N = 10000+N, beta.N = 20000+N, rc.N = 30000+N, final = 65535 (por encima de todas).
#[cfg(windows)]
fn file_version(version: &str) -> u64 {
    let (core, pre) = version.split_once('-').unwrap_or((version, ""));
    let mut it = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0).min(0xFFFF));
    let (major, minor, patch) = (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0));
    let rank = if pre.is_empty() {
        0xFFFF
    } else {
        let digits: String = pre.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
        let n: u64 = digits.chars().rev().collect::<String>().parse().unwrap_or(0).min(9999);
        let low = pre.to_lowercase();
        let base = if low.starts_with("rc") { 30000 } else if low.starts_with("beta") { 20000 } else { 10000 };
        base + n
    };
    (major << 48) | (minor << 32) | (patch << 16) | rank
}

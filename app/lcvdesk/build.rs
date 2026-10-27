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
        if let Err(e) = res.compile() {
            println!("cargo:warning=no se pudo incrustar el icono: {e}");
        }
    }
}

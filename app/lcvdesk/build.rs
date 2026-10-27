// Recompila el crate cuando cambia LCVDESK_VERSION, para que `option_env!("LCVDESK_VERSION")`
// (la versión tomada de la etiqueta en CI) quede bien incrustada en el binario.
fn main() {
    println!("cargo:rerun-if-env-changed=LCVDESK_VERSION");
}

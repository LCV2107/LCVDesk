# LCVDesk

Escritorio remoto para PCs Windows de una misma red local (LAN). Sin servidores externos, sin relay.
Uso interno de la oficina. Licencia: [AGPL-3.0](LICENSE).

Objetivo: latencia mínima (15–30 ms extremo a extremo en LAN gigabit).

## Documentación

| Documento | Contenido |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Diseño (Fase 3): componentes, protocolo, latencia, carpetas, dependencias |
| [docs/research/01-anydesk.md](docs/research/01-anydesk.md) | Fase 1: comportamiento de AnyDesk y evidencia |
| [docs/research/02-rustdesk.md](docs/research/02-rustdesk.md) | Fase 2: módulos de RustDesk, qué se reutiliza y qué sobra |
| [docs/research/03-licencia.md](docs/research/03-licencia.md) | Decisión de licencia (AGPL-3.0, uso interno) |
| [docs/RELEASING.md](docs/RELEASING.md) | Publicar versiones por GitHub y actualización automática |
| [reference/README.md](reference/README.md) | Código de referencia (RustDesk, hwcodec) con commits fijados |
| [THIRD_PARTY.md](THIRD_PARTY.md) | Atribuciones |

## Estado

| Fase | Estado |
|---|---|
| 1. Análisis de AnyDesk | Completada |
| 2. Estudio de RustDesk | Completada |
| 3. Diseño | Completada |
| 4.1 Captura y video | Implementada — en pruebas ([docs/TESTING.md](docs/TESTING.md)) |
| 4.1+ Varias pantallas, varias subredes, UAC en remoto | Implementada — en pruebas |
| 4.2 Mouse y teclado | Implementada — en pruebas |
| 4.3 Descubrimiento | Pendiente |
| 4.4 Cifrado y aceptación | Pendiente |
| 4.5 Portapapeles y archivos | Pendiente |
| Actualización por GitHub (updater) | Implementada — requiere fijar el repositorio |
| Complemento IGP_Soft (gestión de ventanas en remoto) | Diseñado, pendiente de implementar |

## Requisitos de compilación

- Rust estable, toolchain `x86_64-pc-windows-msvc`.
- Visual Studio 2022 con "Desarrollo para escritorio con C++" y **Windows 11 SDK**.
- WiX Toolset 5 para el MSI: `dotnet tool install --global wix --version 5.0.2` y
  `wix extension add -g WixToolset.Firewall.wixext/5.0.2`.

## Compilar y generar el instalador

```powershell
cargo run -p lcvdesk            # ejecutar en modo desarrollo
cargo test                      # pruebas
.\tools\build-msi.ps1           # release + dist\LCVDesk-<versión>-x64.msi
```

## Estructura

```
docs/         diseño e investigación
reference/    código externo de referencia (no versionado; ver reference/README.md)
tools/        scripts (análisis, referencias, MSI, firewall)
crates/       bibliotecas: lcv-proto (protocolo), lcv-capture (DXGI/GDI multi-monitor),
              lcv-codec (H.264), lcv-input (ratón/teclado con SendInput)
app/lcvdesk/  ejecutable: host, visor, interfaz, mapa de teclado, casilla de UAC
installer/    definición del MSI (WiX)
```

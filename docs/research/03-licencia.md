# Decisión de licencia

Fecha: 2026-10-22 · **Decisión: opción A — LCVDesk se publica bajo AGPL-3.0 y reutiliza código de RustDesk.**

Orientación general, no asesoría legal.

## Contexto

- RustDesk es **AGPL-3.0**: copiar o adaptar su código hace que LCVDesk sea AGPL-3.0.
- LCVDesk es de **uso interno** de la oficina y no se distribuirá fuera de la organización.

## Obligaciones en uso interno

| Situación | ¿Obligación? |
|---|---|
| Instalar en las PCs de la propia organización | **No** — no es "transmitir" (conveying) según la FAQ de la FSF |
| Usuarios internos que se conectan remotamente (§13) | Tener el código disponible para ellos — se cumple con este repositorio |
| Entregar a otra empresa, cliente o tercero | **Sí** — habría que ofrecer el código fuente bajo AGPL-3.0 |
| Venderlo / publicarlo | Permitido, siempre con código abierto AGPL-3.0 |
| Cerrarlo en el futuro | Requiere reescribir las partes derivadas de RustDesk |

## Componentes de terceros

| Componente | Licencia | Nota |
|---|---|---|
| RustDesk (`src/`, `hbb_common`, `base`, `clipboard`) | AGPL-3.0 | — |
| `libs/scrap`, `libs/enigo` | MIT declarada en `Cargo.toml` | Modificadas dentro del repo AGPL → tratarlas como AGPL |
| `libs/clipboard/src/windows/wf_cliprdr.c` | Apache-2.0 (FreeRDP) | Compatible con AGPLv3 |
| `hwcodec` (rustdesk-org) | No declarada | Usa FFmpeg (LGPL/GPL según compilación) |
| H.264 | Patentes | Uso interno de pocos equipos: sin riesgo práctico; los codificadores GPU vienen licenciados con el driver |

## Buenas prácticas adoptadas

- Archivo [LICENSE](../../LICENSE) con el texto de la AGPL-3.0.
- [THIRD_PARTY.md](../../THIRD_PARTY.md) con atribuciones.
- Conservar los avisos de copyright en los archivos copiados de RustDesk y marcar las modificaciones.

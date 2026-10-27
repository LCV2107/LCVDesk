# Componentes de terceros

LCVDesk se distribuye bajo la [GNU Affero General Public License v3.0](LICENSE).

| Componente | Origen | Licencia | Uso en LCVDesk |
|---|---|---|---|
| RustDesk | https://github.com/rustdesk/rustdesk — Copyright (c) Purslane Tech Pte. Ltd. y colaboradores | AGPL-3.0 | Código adaptado (se detalla por archivo al copiarlo) |
| scrap | https://github.com/quadrupleslap/scrap (versión modificada por RustDesk) | MIT (original) / AGPL-3.0 (modificaciones de RustDesk) | Captura DXGI/GDI |
| enigo | https://github.com/enigo-rs/enigo (versión modificada por RustDesk) | MIT (original) / AGPL-3.0 (modificaciones de RustDesk) | Inyección de entrada |
| FreeRDP `wf_cliprdr.c` | https://github.com/FreeRDP/FreeRDP | Apache-2.0 | (Opcional) archivos vía portapapeles |
| hwcodec | https://github.com/rustdesk-org/hwcodec | No declarada | (Fase 4.1b) códec GPU |
| OpenH264 | https://github.com/cisco/openh264 (vía crate `openh264`) | BSD-2-Clause | Códec H.264 por software, compilado desde el código fuente. La licencia de patentes de Cisco solo cubre sus binarios oficiales; en uso interno no supone un riesgo práctico |

Las dependencias de crates.io se listan en `Cargo.lock` con sus propias licencias.

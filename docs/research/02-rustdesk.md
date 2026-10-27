# Fase 2 — Estudio de RustDesk

Fecha: 2026-10-22 · Copia de referencia: [reference/rustdesk](../../reference/rustdesk)
(commit `828d1101afd2e656eb1c1d18511009bdf5336238`, submódulo `hbb_common` `edefa08b…`). ~125 000 líneas de Rust.

## 1. Mapa de módulos

Rutas relativas a `reference/rustdesk/`.

| Área | Archivos | Funcionamiento en Windows | ¿Para LAN? |
|---|---|---|---|
| Captura | `libs/scrap/src/dxgi/mod.rs`, `dxgi/gdi.rs` | `DuplicateOutput1` → `AcquireNextFrame`; respaldo GDI; salida como textura GPU (`set_output_texture`) o buffer | Sí (núcleo) |
| Códec | `libs/scrap/src/common/codec.rs`, `vpxcodec.rs`, `hwcodec.rs`, `vram.rs` | VP8/VP9 (libvpx, `VPX_DL_REALTIME`, CBR), AV1; H.264/H.265 hardware vía `hwcodec`; VRAM = textura directa al codificador GPU | Sí — H.264 GPU + VRAM es lo más rápido |
| Bucle de video / QoS | `src/server/video_service.rs`, `src/server/video_qos.rs` | `INIT_FPS = 15`, `MAX_FPS = 120`, umbral de red buena 150 ms | Diseñado para internet; en LAN, fps fijos |
| Entrada | `libs/enigo/src/win/win_impl.rs`, `src/server/input_service.rs`, `src/keyboard.rs` | `SendInput` con scancodes (`MapVirtualKeyExW`) y `KEYEVENTF_UNICODE`; marca `dwExtraInfo`; cliente usa `rdev` | Sí |
| Red | `libs/hbb_common/src/tcp.rs`, `bytes_codec.rs`, `src/kcp_stream.rs` | TCP `set_nodelay(true)`, tramas con longitud, protobuf (`libs/base/protos/message.proto`); KCP sobre UDP | TCP + tramas, sí |
| Conexión directa | `src/rendezvous_mediator.rs` → `direct_server()` (línea ~1343) | TCP 21118 (`RENDEZVOUS_PORT + 2`), **desactivado por defecto** (`direct-server`) | Sí, extrayéndolo |
| Descubrimiento | `src/lan.rs` | Broadcast UDP `255.255.255.255:21119`, mensaje `PeerDiscovery`, respuesta unicast; un socket por interfaz IPv4 | Sí |
| Cifrado | `libs/hbb_common/src/tcp.rs`, `src/client.rs` (~1660–1770) | libsodium: `box_` (X25519) → `secretbox` (XSalsa20-Poly1305) | **No** (ver §3) |
| Aceptación | `src/server/connection.rs` → `try_start_cm()` (~2354), `src/ui_cm_interface.rs` | `ApproveMode::{Password, Click, Both}`; `ipc::Data::Login` al Connection Manager; `authorize()` / `close()` | Patrón sí |
| Portapapeles | `src/clipboard.rs` | `arboard` + `clipboard-master` | Sí |
| Archivos vía portapapeles | `libs/clipboard/src/windows/wf_cliprdr.c` | Código FreeRDP (Apache-2.0), `IDataObject` / `FileContents` | Más adelante |
| Transferencia de archivos | `libs/base/src/fs.rs`, mensajes `FileTransfer*` | Bloques, digest, reanudación, confirmación | Sí |

## 2. Lo que sobra para solo LAN

- `src/rendezvous_mediator.rs` (excepto `direct_server`), `libs/hbb_common/protos/rendezvous.proto`.
- Hole punching, STUN, UPnP, relay.
- `libs/hbb_common/src/webrtc.rs`, `websocket.rs`, `proxy.rs`, KCP.
- `src/hbbs_http/*`, updater, 2FA, libreta de direcciones, terminal, impresora remota.
- Linux/macOS/Android/iOS, Wayland/DRM, interfaz Flutter/Sciter.

`src/server/connection.rs` (7 800 líneas) y `src/client.rs` (5 900) mezclan todo lo anterior: **es más
eficiente escribir un núcleo nuevo** reutilizando `scrap`, `enigo` y los patrones, que recortar el fork.

## 3. Hallazgo de seguridad

La identidad del par en RustDesk depende de una clave **firmada por el servidor rendezvous**
(`SignedId`, `decode_id_pk`). Sin servidor, `src/client.rs` registra
`"pk mismatch, fall back to non-secure"` / envía `Message::new()` vacío y la sesión **queda sin cifrar**.

→ LCVDesk implementa cifrado propio sin servidor: **Noise XX** + identidad por equipo + SAS + lista de confianza.

## 4. Latencia

1. DXGI → textura → **H.264 hardware** (NVENC/AMF/QSV), sin B-frames ni lookahead.
2. Decodificación hardware (D3D11VA) y render directo de textura.
3. TCP nodelay, fps fijos, sin rampa de QoS.
4. Software (openh264 / libvpx) como respaldo.

## 5. `hwcodec` (dependencia del códec GPU)

Copia: [reference/hwcodec](../../reference/hwcodec) (commit `ec38ebaf2454269d4c5668a57e3813c6e5541be7`).

- `build.rs` **siempre** enlaza FFmpeg desde vcpkg (`VCPKG_ROOT` obligatorio) y usa `bindgen` (requiere LLVM).
- Feature `vram`: compila además los SDK de NVIDIA (`cpp/nv`), AMD (`cpp/amf`) e Intel (`cpp/mfx`) y enlaza `d3d11`/`dxgi`.
- **No declara licencia** en el repositorio (aceptable para uso interno).

Por eso LCVDesk empieza con `openh264` (sin dependencias nativas) y añade `hwcodec` en la Fase 4.1b.

## 6. Dependencias nativas de RustDesk (`vcpkg.json`)

`libvpx`, `libyuv`, `opus`, `aom`, `libjpeg-turbo`, `mfx-dispatch`, FFmpeg (receta propia en `res/vcpkg`).

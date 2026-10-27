# LCVDesk — Arquitectura

Escritorio remoto para PCs Windows de una misma LAN. Sin servidores externos, sin relay, sin NAT traversal.
Uso interno de la oficina. Licencia: **AGPL-3.0** (reutiliza código de RustDesk).

Objetivo principal: **latencia mínima** (meta: 15–30 ms extremo a extremo en LAN gigabit, 1080p60).

---

## 1. Visión general

```
        PC HOST (el que se controla)                         PC VISOR (el que controla)
 ┌──────────────────────────────────────┐            ┌──────────────────────────────────────┐
 │ Captura DXGI ──► Codificador H.264 ──┼──┐      ┌──┼──► Decodificador ──► Render wgpu     │
 │ (textura GPU)    (GPU o openh264)    │  │      │  │                      (shader YUV→RGB)│
 │                                      │  │ TCP  │  │                                      │
 │ Inyección SendInput ◄────────────────┼──┼◄─────┼──┼── Captura teclado/mouse (scancodes)  │
 │ Portapapeles  ◄──────────────────────┼─►│Noise │◄─┼──► Portapapeles                      │
 │ Ventana de aceptación (egui)         │  │cifrado  │  Lista de equipos (egui)             │
 │ Anuncio de descubrimiento (UDP) ─────┼──┼──────┼──┼──► Escucha de descubrimiento         │
 └──────────────────────────────────────┘  └──────┘  └──────────────────────────────────────┘
```

Un único ejecutable `lcvdesk.exe` cumple ambos roles: siempre **anuncia** el equipo y **escucha**
conexiones (host), y desde la ventana principal puede **conectarse** a otro (visor).

### Lo que tomamos de cada referencia

| Idea | Origen | Uso en LCVDesk |
|---|---|---|
| Captura DXGI Desktop Duplication + respaldo GDI | AnyDesk (logs) y RustDesk `libs/scrap/src/dxgi` | Copiamos y adaptamos el código de RustDesk |
| Textura GPU directa al codificador hardware (sin copia a CPU) | RustDesk `vram.rs` / `hwcodec` | Fase 4.1b |
| Inyección `SendInput` con scancodes, marca `dwExtraInfo` | RustDesk `libs/enigo/src/win` | Copiamos y adaptamos |
| Descubrimiento multicast con reintentos (1 s, máx. 10) | AnyDesk (logs) | Implementación propia |
| Puerto TCP fijo para conexión directa | AnyDesk (7070) / RustDesk (21118) | Puerto propio 47070 |
| Ventana de aceptación en la sesión del usuario | AnyDesk "accept window" / RustDesk Connection Manager | Implementación propia (egui) |
| Separación servicio (SYSTEM) + agente de sesión | AnyDesk / RustDesk | Fase opcional 4.6 |
| Cifrado | **Propio** — RustDesk queda en texto plano en conexiones directas sin su servidor | Noise XX |

---

## 2. Componentes

### 2.1 Descubrimiento (`lcv-net::discovery`)
- **UDP multicast** `239.255.47.71:47071` (TTL 1, no sale de la subred) + **broadcast** `255.255.255.255:47071`
  como respaldo para redes que filtran multicast.
- Al iniciar y cada vez que cambia la red: anuncio `Announce` cada 1 s, hasta 10 veces; después, solo
  responde a consultas `Query` del visor (unicast).
- Contenido del anuncio: `device_id` (huella de la clave pública estática), nombre del equipo, puerto TCP,
  versión del protocolo, IPs locales.
- Envío por **cada interfaz IPv4** activa (como `create_broadcast_sockets` en RustDesk `lan.rs`).
- El descubrimiento **no es de confianza**: solo sirve para listar equipos. La identidad se verifica en el
  handshake Noise.

### 2.2 Conexión directa (`lcv-net::transport`)
- **TCP 47070** con `TCP_NODELAY`, buffers de socket ampliados (4 MB), keep-alive.
- Conexión principal: control + video + entrada + portapapeles. Una segunda conexión TCP para
  **archivos** (para que una transferencia grande no retrase el video), autenticada con la misma sesión.
- Tramado: `[u32 longitud][bloque cifrado]`. Mensajes serializados con `serde` + `postcard`.
- TCP es full-duplex: el video (host→visor) y la entrada (visor→host) no se bloquean entre sí.

### 2.3 Cifrado de extremo a extremo (`lcv-crypto`)
- **Noise_XX_25519_ChaChaPoly_BLAKE2s** (crate `snow`): autenticación mutua de las claves estáticas,
  secreto hacia adelante (claves efímeras por sesión).
- Cada equipo genera al primer arranque un par de claves X25519, guardado en
  `%ProgramData%\LCVDesk\identity.key` protegido con **DPAPI** (machine scope).
- `device_id` = primeros 10 bytes de BLAKE2s(clave pública), mostrado como `XXXX-XXXX-XXXX-XXXX`.
- **Código de verificación (SAS)**: 6 dígitos derivados del hash del handshake, mostrado en ambas
  pantallas → permite comprobar que no hay intermediario.
- **Lista de confianza** (`trusted.json`): huellas de visores aceptados con "Recordar este equipo".
  Por defecto, aun así, se pide aceptación (configurable).
- Mensajes Noise ≤ 65 535 bytes → los fotogramas grandes se fragmentan en bloques de 64 KB.
- Rendimiento ChaCha20-Poly1305: >1 GB/s por núcleo; impacto en latencia < 0,1 ms por fotograma.

### 2.4 Aceptación de la conexión
Flujo en el host:
1. Llega una conexión TCP → handshake Noise (identidad del visor ya verificada).
2. El visor envía `Hello { nombre, usuario, permisos solicitados }`.
3. El host muestra una **ventana siempre visible** (egui): nombre, IP, huella, código SAS, casillas de
   permisos (ver / controlar / portapapeles / archivos), botones **Aceptar** / **Rechazar** y
   "Recordar este equipo". Tiempo límite 30 s → rechazo automático.
4. Hasta aceptar, **no se envía ni un píxel** ni se acepta entrada.
5. Durante la sesión: indicador flotante "Controlado por X" con botón **Desconectar**.

### 2.5 Captura de pantalla (`lcv-capture`)
- Adaptado de RustDesk `libs/scrap/src/dxgi/mod.rs` + `gdi.rs`.
- `IDXGIOutputDuplication::AcquireNextFrame` con timeout = intervalo de fotograma; solo produce
  fotograma cuando la pantalla cambia (ahorro de ancho de banda en reposo).
- Dos modos de salida: **textura D3D11** (para codificador GPU, sin copia) o **BGRA en memoria**
  (para codificador software).
- Respaldo GDI (`BitBlt`) cuando DXGI falla (p. ej. escritorio seguro / UAC → `E_ACCESSDENIED`).
- Información del cursor (`DXGI_OUTDUPL_POINTER_SHAPE_INFO`): se envía forma + posición por separado;
  el visor dibuja el cursor localmente → movimiento del ratón sin esperar al video.
- Selección de monitor; cambio de resolución detectado y renegociado.

### 2.6 Códec de video (`lcv-codec`)
Trait `Encoder` / `Decoder` con backends intercambiables:

| Backend | Etapa | Latencia de codificación 1080p | Dependencias nativas |
|---|---|---|---|
| **openh264** (software) | 4.1 | ~5–12 ms | Solo MSVC (el crate compila el código fuente) |
| **hwcodec VRAM** (NVENC / AMF / QuickSync) | 4.1b | ~1–4 ms, sin copia CPU | vcpkg + FFmpeg, LLVM (bindgen) |

Configuración de baja latencia: perfil Baseline/Main sin B-frames, sin lookahead, `GOP` infinito con
**IDR bajo demanda** (al conectar, al perder sincronía), control de bitrate CBR alto (LAN: 20–50 Mbps),
modo "screen content". Un fotograma = un mensaje.

Visor: decodificación (openh264 o D3D11VA vía hwcodec) → planos YUV a texturas wgpu → shader
YUV→RGB → presentación con `PresentMode::Mailbox`/`Immediate` (sin esperar VSync).

### 2.7 Entrada remota (`lcv-input`)
- **Visor**: captura teclas por **scancode** (independiente del idioma del teclado) con un hook de bajo
  nivel `WH_KEYBOARD_LL` mientras la ventana remota tiene el foco → así se envían Win, Alt+Tab, etc.
- Mouse: coordenadas **absolutas normalizadas** (0–65535 sobre el monitor remoto), botones, rueda
  vertical/horizontal.
- **Host**: `SendInput` con `KEYEVENTF_SCANCODE` (+ `KEYEVENTF_EXTENDEDKEY`) y
  `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`. Adaptado de RustDesk `enigo/src/win/win_impl.rs`.
- Al desconectar se liberan todas las teclas pulsadas (evita "teclas pegadas").
- Limitación conocida: `Ctrl+Alt+Supr` y las ventanas UAC necesitan el servicio SYSTEM (Fase 4.6).

### 2.8 Portapapeles y archivos (`lcv-clipboard`, `lcv-files`)
- Portapapeles: `arboard` (texto, imagen) + `clipboard-master` (`AddClipboardFormatListener`) para
  detectar cambios. Sincronización bidireccional, ignorando los cambios que nosotros mismos aplicamos.
- Archivos: panel de transferencia (enviar / recibir) por la conexión de archivos. Bloques de 256 KB,
  hash BLAKE3 por archivo, reanudación, confirmación antes de sobrescribir.
- Copiar/pegar archivos vía portapapeles de Windows (`CF_HDROP` / FreeRDP `wf_cliprdr.c`): opcional,
  posterior.

### 2.9 Procesos
- **Fases 4.1–4.5**: un único proceso de usuario (`lcvdesk.exe`) en la sesión iniciada, con icono en la
  bandeja del sistema. Suficiente para controlar el escritorio normal.
- **Fase 4.6 (opcional)**: servicio Windows `LCVDeskService` (SYSTEM) que lanza el agente en la sesión
  activa con token SYSTEM (`CreateProcessAsUserW`) → permite controlar UAC, la pantalla de bloqueo y
  enviar `Ctrl+Alt+Supr` (`SendSAS`). Es el modelo de AnyDesk (`--service`) y RustDesk.

---

## 3. Protocolo (mensajes)

```rust
enum Message {
    // Sesión
    Hello { name, user, version, want: Permissions },
    Accepted { granted: Permissions, displays: Vec<DisplayInfo> },
    Rejected { reason },
    Ping { t }, Pong { t },                        // medición de latencia (RTT)
    // Video
    VideoConfig { codec, width, height, display },
    VideoFrame { pts_us, keyframe, data: Bytes },  // fragmentado a nivel de cifrado
    RequestKeyframe,
    CursorShape { id, w, h, hot_x, hot_y, rgba }, CursorPos { x, y, visible },
    SelectDisplay { index },
    // Entrada
    Key { scancode, extended, down },
    MouseMove { x, y }, MouseButton { button, down }, MouseWheel { dx, dy },
    ReleaseAll,
    // Portapapeles / archivos
    ClipboardText { text }, ClipboardImage { w, h, png },
    FileOffer {..}, FileAccept {..}, FileChunk {..}, FileDone {..}, FileCancel {..},
}
```

---

## 4. Presupuesto de latencia (meta, 1080p60, LAN gigabit)

| Etapa | Software (4.1) | GPU (4.1b) |
|---|---|---|
| Captura DXGI (espera del siguiente cambio no incluida) | 1 ms | <1 ms |
| Conversión BGRA→I420 | 1–2 ms | 0 (en GPU) |
| Codificación | 5–12 ms | 1–4 ms |
| Cifrado + red (≈150 KB por fotograma a 1 Gbps) | 1–2 ms | 1–2 ms |
| Decodificación | 2–5 ms | 1–2 ms |
| Render + presentación | 1–8 ms | 1–8 ms |
| **Total** | **~12–30 ms** | **~5–17 ms** |

El render depende del monitor del visor (con 60 Hz sin VSync el peor caso añade ~8 ms de tearing).

---

## 5. Estructura de carpetas

```
LCVDesk/
├── Cargo.toml                  # workspace
├── LICENSE                     # AGPL-3.0
├── THIRD_PARTY.md              # atribuciones: RustDesk (AGPL), scrap/enigo (MIT), FreeRDP (Apache-2.0)
├── docs/
│   ├── ARCHITECTURE.md
│   └── TESTING.md              # cómo probar cada parte entre dos PCs
├── crates/
│   ├── lcv-proto/              # Message, Permissions, serialización postcard, tramado
│   ├── lcv-crypto/             # identidad (X25519 + DPAPI), Noise XX, SAS, lista de confianza
│   ├── lcv-net/                # transporte TCP cifrado, fragmentación, descubrimiento UDP
│   ├── lcv-capture/            # DXGI Desktop Duplication + GDI (adaptado de RustDesk scrap)
│   ├── lcv-codec/              # trait Encoder/Decoder; backend openh264; backend hwcodec (feature)
│   ├── lcv-input/              # SendInput (adaptado de enigo), hook de teclado del visor
│   ├── lcv-clipboard/          # sincronización de portapapeles
│   └── lcv-files/              # transferencia de archivos
├── app/
│   └── lcvdesk/                # binario: bandeja, host, visor, UI egui, render wgpu
│       └── src/
│           ├── main.rs
│           ├── host/           # sesión del lado controlado
│           ├── viewer/         # sesión del lado que controla + render
│           └── ui/             # lista de equipos, ventana de aceptación, ajustes
└── tools/
    └── firewall.ps1            # reglas de firewall (perfil Privado, solo subred local)
```

---

## 6. Dependencias principales

| Crate | Uso |
|---|---|
| `tokio` (full) | E/S asíncrona de red |
| `socket2` | opciones de socket, multicast por interfaz |
| `serde`, `postcard` | serialización de mensajes |
| `bytes` | buffers sin copia |
| `snow` | Noise Protocol |
| `blake2`, `blake3` | huellas / hashes de archivos |
| `windows` | DPAPI, D3D11, DXGI, `SendInput`, hooks, red |
| `winapi` | requerido por el código adaptado de RustDesk scrap/enigo |
| `openh264` | H.264 software (codificar y decodificar) |
| `hwcodec` (git rustdesk-org, feature `gpu`) | H.264/H.265 por GPU — Fase 4.1b |
| `yuv` o `dcv-color-primitives` | conversión BGRA↔I420 con SIMD |
| `eframe` / `egui` (backend wgpu) | interfaz y ventana remota |
| `wgpu` | render del video con shader YUV→RGB |
| `tray-icon` | icono en la bandeja |
| `arboard`, `clipboard-master` | portapapeles |
| `anyhow`, `thiserror`, `tracing`, `tracing-subscriber` | errores y registros |
| `directories`, `hostname`, `local-ip-address` | rutas de configuración, nombre del equipo, IPs |

Las versiones exactas se fijan al crear el workspace (`Cargo.lock` versionado).

### Requisitos para compilar
- Rust estable, toolchain `x86_64-pc-windows-msvc` (rustup).
- Visual Studio 2022 con "Desarrollo para escritorio con C++" (ya instalado).
- Solo para la Fase 4.1b: LLVM (para `bindgen`) y vcpkg con FFmpeg (receta de RustDesk `res/vcpkg`).

---

## 7. Decisiones tomadas al implementar

| Tema | Diseño original | Implementado | Motivo |
|---|---|---|---|
| E/S de red (4.1) | `tokio` | Hilos de `std` con sockets bloqueantes | Menos latencia y código más simple con 1–2 conexiones; se revisará si hace falta |
| Captura (4.1) | Copiar `scrap` de RustDesk | Implementación propia con `windows` 0.62 siguiendo el mismo diseño | Evita arrastrar `hbb_common`; ~250 líneas |
| Render (4.1) | Shader YUV→RGB en wgpu | YUV→RGBA en CPU (crate `yuv`, SIMD) + textura egui | Primera versión funcional; el shader se hará con el códec GPU (4.1b) |
| Aceptación (4.1) | Ventana completa en 4.4 | `MessageBox` Sí/No mínimo desde ya | Nadie puede ver una pantalla sin permiso, ni siquiera en pruebas |
| Instalador | — | WiX 5.0.2 (MS-RL) | WiX 6+ exige aceptar la tarifa de mantenimiento (OSMF) para uso comercial |
| CRT | — | Estático (`+crt-static`) | El `.exe` no depende de VC++ Redistributable en los PCs destino |

## 8. Plan de implementación (Fase 4)

1. **Video**: captura DXGI → openh264 → TCP (aún sin cifrar, solo pruebas) → visor wgpu. Medición de
   latencia integrada (overlay con fps, ms de codificación/red/decodificación).
   1b. Backend GPU con hwcodec.
2. **Entrada**: teclado (scancodes + hook) y mouse; cursor local.
3. **Descubrimiento**: lista automática de equipos en la ventana principal.
4. **Cifrado + aceptación**: Noise XX, identidad DPAPI, ventana de aceptación, SAS, lista de confianza.
   A partir de aquí no existe modo sin cifrar.
5. **Portapapeles y archivos**.
6. *(Opcional)* Servicio SYSTEM para UAC / pantalla de bloqueo / Ctrl+Alt+Supr.

Hasta la parte 4, las pruebas deben hacerse solo en la red de la oficina (tráfico sin cifrar).

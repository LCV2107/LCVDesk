# Fase 1 — Análisis de comportamiento de AnyDesk

Fecha: 2026-10-22 · Objetivo: entender la arquitectura general (no copiar código, no tocar licencia ni cifrado).

## Objetivo analizado

| Campo | Valor |
|---|---|
| Archivo | `C:\Program Files (x86)\AnyDesk\AnyDesk.exe` |
| Versión | 9.7.15 (AnyDesk Software GmbH) |
| SHA-256 | `89e0a41a37d360d81bb0bfd89537dbbb3bdf45917adfe88ae3cb19d4f6b3c81e` |
| Tamaño | 8 375 224 bytes, PE32 (x86) |
| Acompañante | `gcapi.dll` — SHA-256 `73170761d6776c0debacfbbc61b6988cb8270a20174bf5c049768a264bb8ffaf` |

Herramientas: REA (`inspect_pe_resources`, evidencia `ev_7b2dbfdb…`), parser estático propio de solo lectura
([tools/research/pe_inventory.py](../../tools/research/pe_inventory.py)), consultas de sistema de solo lectura
(servicios, firewall, sockets, módulos cargados) y los registros de diagnóstico que AnyDesk escribe en texto plano.

## 1. Análisis estático: el binario está empaquetado

| Observación | Detalle |
|---|---|
| Sin tabla de importaciones | El encabezado PE no declara directorio de importaciones |
| `.text` | 10 KB (stub cargador), entropía 6,52 |
| `.itext` | 29 MB virtuales, **0 bytes en disco** (se rellena en tiempo de ejecución) |
| `.data` | 8 MB, **entropía 8,00/8** → contenido comprimido/cifrado |
| Ruta PDB | `C:\Gitlab-Runner\...\release\app-32\win_loader\Static\AnyDesk.pdb` |
| Recursos | Solo iconos, versión y manifiesto |
| Cadenas legibles | Solo el bloque de versión |

**Conclusión**: el `.exe` es un cargador; el programa real está empaquetado. Desempaquetarlo implicaría
eludir su protección (anticircunvención + EULA), por lo que **el análisis estático se detuvo aquí**.

`gcapi.dll` no está empaquetada: es la **Google Chrome API** (`LaunchGoogleChrome`,
`GoogleChromeCompatibilityCheck`, pdb `gcapi_dll.dll.pdb`, compilada en 2016). Sin relación con el escritorio remoto.

## 2. Observación del sistema (solo lectura)

| Tema | Observación |
|---|---|
| Servicio | `AnyDesk`, inicio automático, **LocalSystem**, `"...\AnyDesk.exe" --service`, sesión 0 |
| Procesos | Uno en sesión 0 (servicio) y otro en la sesión 1 del usuario |
| Puertos | Servicio escucha **TCP 7070** (IPv4+IPv6, todas las interfaces); **UDP 50001** enlazado en `0.0.0.0` |
| Red externa | TLS a `64.34.91.127:443` permanente (red de rendezvous/relay de AnyDesk) |
| Firewall | Reglas "AnyDesk" de entrada, permitir, TCP+UDP, cualquier puerto, perfiles Dominio/Privado/Público |
| Módulos (sin sesión) | Ambos: `ws2_32`, `mswsock`, `iphlpapi`, `dnsapi`, `winhttp`, `crypt32`, `gdiplus`. Servicio: `firewallapi`, `fwpuclnt`, `netprofm`. Usuario: `wtsapi32`, `windowscodecs`, `msctf` |

## 3. Registros de diagnóstico de AnyDesk

Archivos: `C:\ProgramData\AnyDesk\ad_svc.trace` (servicio) y `%APPDATA%\AnyDesk\ad.trace` (sesión de usuario).
No se leyeron los `.conf` (pueden contener hashes de contraseñas).

| Pregunta | Líneas del registro (citadas) | Conclusión |
|---|---|---|
| Captura de pantalla | `capture.dda_stream_capture - Initializing D3D.` / `Initializing DXGI.` / `Using '\\.\DISPLAY1' (1)`; `capture.fallback_capture - Selecting GDI capture.`; `gdi.gdi_surface - Initialized to 1920x1080 (X8R8G8B8).`; `Unable to reinit d3d (... Access is denied. (0x80070005)). Using fall[back]` | **DXGI Desktop Duplication** con respaldo **GDI** (p. ej. al pasar a escritorio seguro) |
| Procesos | Etiquetas `gsvc` (servicio global), `ctrl` (control, portapapeles), `back` (backend de captura/audio); `app.service - Creating an IPC accept socket.` | 3 roles comunicados por IPC |
| Descubrimiento | `app.discovery - Joined multicast group 239.255.102.18:50001.`; `Updated Discovery settings: repeat interval (1000), repeat limit (10), buffer interval (5000)` | **Multicast UDP**, anuncio cada 1 s, máx. 10 repeticiones |
| Conexión directa | `anynet.tcp_acceptor - Listening on TCP port 7070 with IPv4.` / `IPv6`; `Attempting UPnP forwarding to port 7070.`; `punch_connector - [192.168.0.67:7070] Connecting` | Puerto fijo 7070; el cliente prueba **todas las IPs candidatas en paralelo** |
| Aceptación | `app.session - Send route info for the accept window.` | El servicio delega la ventana de aceptación al proceso de usuario |
| Funciones de sesión | `app.session - Session features: deskrt, clipboard, chat, audio, input, tcp_tunnel.` | — |
| Portapapeles | `clipbrd.capture - Setting new capture formats: text files.`; `Unregistered clipboard notification.`; `app.session - Purging clipboard files.` | Notificaciones de portapapeles de Windows; formatos texto y archivos |

## 4. Respuestas a las preguntas de la Fase 1

| Pregunta | Estado | Respuesta |
|---|---|---|
| API de captura | Respondida (registros) | DXGI Desktop Duplication + GDI |
| Mouse y teclado | Sin resolver | `user32` cargada; API exacta no observable sin desempaquetar |
| Portapapeles | Parcial | Notificaciones del portapapeles, texto + archivos |
| Archivos | Parcial | Vía portapapeles ("clipboard files"); detalle desconocido |
| Descubrimiento | Respondida | Multicast `239.255.102.18:50001` |
| Conexión | Respondida | TCP 7070 directo, UPnP, conexión paralela a IPs candidatas, red propia por 443 |
| Permiso del usuario | Parcial | Ventana de aceptación en la sesión del usuario, solicitada por el servicio vía IPC |
| Protocolo / cifrado | Excluido a propósito | — |

## 5. Limitaciones

1. REA sin motor de desensamblado (Ghidra/IDA no configurados); de todas formas solo se vería el cargador.
2. No se ejecutó ni depuró AnyDesk; los procesos ya estaban en ejecución.
3. Puertos y módulos observados en reposo (sin sesión activa).
4. El EULA de AnyDesk prohíbe la ingeniería inversa; no se profundizó más allá de la observación pasiva.

## 6. Lo que adopta LCVDesk

- Captura DXGI con respaldo GDI.
- Descubrimiento multicast con reintentos limitados.
- Puerto TCP fijo para conexión directa.
- Ventana de aceptación en la sesión del usuario.
- (Opcional) separación servicio SYSTEM + agente de sesión.

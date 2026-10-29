# Complemento IGP_Soft: parte de LCVDesk

Estado: **implementado en `v0.4.0-beta.1`**, probado en un solo PC y pendiente de la prueba entre dos.
IGP_Soft ya terminó su parte (fases 0 a 3, versión `1.0.2-exp.2`, commit `d08500a` de su rama
`experimental`). La referencia es su `IGP_Soft-plan-tunel.xml` **versión 4**: los valores de
`<contrato estado="final">` se usan tal cual, sin cambiar nombres. Código: `app/lcvdesk/src/plugins.rs`
(detección, tuberías y relevo), `host.rs` (`open_plugin`), `viewer.rs` (`launch_local_panel`) y
`ui.rs` (`plugins_button`, `plugins_section`).

## Qué hace LCVDesk

LCVDesk es solo un **túnel**. No interpreta los mensajes de IGP_Soft.

```
PC QUE CONTROLA                                          PC CONTROLADO
IGP_Soft --remoto --tubo=LCVDesk.IGP.<guid>              IGP_Soft (normal o --controlado)
      │ cliente                                                ▲ servidor
      ▼                                                        │ IGP_Soft.Control.v1.<SessionId>
tubería servidor (la crea el visor)                      cliente (lo conecta el host)
      │                                                        │
LCVDesk visor ═══ PluginData (bytes tal cual, en orden) ═══ LCVDesk host
```

1. **Detectar** IGP_Soft en los dos PCs y comparar versiones.
2. **PC controlado (host):** conectarse como cliente a `IGP_Soft.Control.v1.<SessionId>`. Si la
   tubería no existe, abrir `"<exe>" --controlado` y reintentar durante 10 s.
3. **PC que controla (visor):** crear una tubería servidor `LCVDesk.IGP.<guid>` y abrir
   `"<exe>" --remoto --tubo=<nombre>`. IGP_Soft se conecta en 10 s como máximo.
4. **Copiar bytes** en los dos sentidos a la vez, sin filtrar nada (tampoco los latidos). Si algo
   se corta, cerrar enseguida las dos tuberías.

## Diseño

### Detección (`plugins.rs`, en los dos lados)

- **Instalado:** `HKLM\Software\IGP_Soft` (vista de 64 bits), con «Carpeta» + `IGP_Soft.exe` y
  «Version». Tiene preferencia.
- **Portable:** `HKCU\Software\IGP_Soft`, con «Ruta» y «Version».
- Se comprueba además que el `.exe` existe en disco. Si el registro apunta a un archivo borrado,
  cuenta como «no instalado».
- **Versiones:** igualdad exacta sin distinguir mayúsculas, incluido `-exp.N`
  (`eq_ignore_ascii_case`).
- **Si falta o no coincide:** se muestra un aviso que dice qué PC tiene qué versión, con el enlace
  `https://github.com/LCV2107/IGP_Soft/releases/latest`.

### Mensajes nuevos de LCVDesk (protocolo v4)

Son genéricos para cualquier complemento futuro. Se añaden al final del `enum Message`, como se
hizo con el audio, y el test de índices los fija.

| Mensaje | Sentido | Para qué |
|---|---|---|
| `PluginQuery` | visor → host | ¿Qué complementos tienes y con qué versión? |
| `PluginList { items: [{id, version}] }` | host → visor | Respuesta. `version` vacía si no está. |
| `PluginOpen { id, channel }` | visor → host | Abre el lado controlado del complemento. |
| `PluginOpened { channel }` | host → visor | Tubería conectada. |
| `PluginError { channel, reason }` | host → visor | `no_instalado`, `no_responde` (10 s sin tubería), `acceso_denegado` (caso L2), `no_permitido`, `desconocido`. |
| `PluginData { channel, data }` | los dos | Bytes copiados tal cual de la tubería. |
| `PluginClose { channel }` | los dos | Un lado se cerró: el otro cierra su tubería. |

- `id` es un texto fijo: `"igp_soft"`. **El host solo acepta ids que conoce.** La ruta del `.exe` y
  los argumentos salen siempre **de su propio registro**, nunca del visor. Así una sesión no puede
  hacer que el host ejecute otra cosa.
- `PluginData` lleva **bloques de bytes** (hasta 64 KB por mensaje), no líneas. Al copiar los bytes
  en orden, cada línea JSON llega idéntica, con su `\n`, y LCVDesk no necesita partir ni medir
  líneas. El límite de 1 MB por línea lo aplica IGP_Soft.
- Si el otro PC tiene LCVDesk v3 o anterior, el visor no ofrece complementos y muestra «Actualiza
  LCVDesk en el equipo remoto».

### Tuberías dúplex (detalle importante)

En Windows, si un handle **síncrono** tiene un `ReadFile` en espera, un `WriteFile` desde otro hilo
**queda bloqueado** hasta que el primero termine. Es justo el bloqueo que advierte el contrato
(«dúplex de verdad»). Por eso:

- Las dos tuberías se abren con **`FILE_FLAG_OVERLAPPED`**: lectura y escritura en curso a la vez
  sobre el mismo handle, cada una con su `OVERLAPPED` y su evento.
- **Host:** `CreateFileW(\\.\pipe\IGP_Soft.Control.v1.<SessionId>, GENERIC_READ|GENERIC_WRITE, …,
  FILE_FLAG_OVERLAPPED)`. `SessionId` sale de `ProcessIdToSessionId(GetCurrentProcessId())`; el
  host corre como el usuario de la sesión.
- **Visor:** `CreateNamedPipeW(\\.\pipe\LCVDesk.IGP.<guid>, PIPE_ACCESS_DUPLEX |
  FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE |
  PIPE_REJECT_REMOTE_CLIENTS, 1 instancia, 64 KB, 64 KB)`. La ACL solo admite al usuario actual.
  `ConnectNamedPipe` en modo overlapped, con 10 s de espera.
- Por cada canal hay dos hilos:
  - **pipe→red:** lee de la tubería y envía `PluginData`;
  - **red→pipe:** recibe de una cola y escribe en la tubería.

  Si cualquiera de los dos falla, se cierra el canal y se envía `PluginClose`.

### Colas y prioridad en la conexión

- Hoy el host descarta vídeo cuando la cola está llena (`try_send`). **El túnel nunca descarta.**
  Tendrá su propia cola acotada (64 bloques) y envío **bloqueante**: si la red va lenta, el hilo
  pipe→red espera, IGP_Soft nota la presión en su tubería y no se pierde nada.
- `spawn_writer` del host atiende primero el audio, luego el túnel y después lo demás. Un bloque de
  64 KB tarda menos de 1 ms en una LAN, así que no afecta al vídeo.
- En el visor, `PluginData` va por el mismo `writer` que la entrada de ratón y teclado. Los
  bloques son pequeños, así que la entrada no se retrasa de forma apreciable.

### Secuencia

1. Sesión aceptada con v4 → el visor envía `PluginQuery` y detecta su propio IGP_Soft.
2. El usuario abre **Complementos → IGP_Soft**. Si falta en algún PC o las versiones no
   coinciden, se avisa con el enlace y no se abre nada.
3. El visor envía `PluginOpen{"igp_soft", canal}`. El host conecta la tubería o abre
   `--controlado` y reintenta 10 s. Responde `PluginOpened` o `PluginError`.
4. Solo con `PluginOpened`, el visor crea `LCVDesk.IGP.<guid>`, abre `--remoto --tubo=…` y espera
   la conexión (10 s). Lo que llegue antes del host se guarda en memoria y se entrega al conectar.
5. Se copian bytes en los dos sentidos.
6. **Cierres:**
   - Si se cierra el panel remoto, el visor envía `PluginClose` y el host cierra su tubería: el PC
     controlado se desbloquea al momento.
   - Si el PC controlado responde `ocupado` y cierra, esa línea llega al panel remoto, que la
     muestra, y después llega `PluginClose`.
   - Si se cae la sesión de LCVDesk, los dos lados cierran sus tuberías. El panel remoto reintenta
     10 s, avisa y se cierra.
7. **Reconexión en menos de 10 s:** el contrato la admite, pero LCVDesk hoy no reanuda sesiones
   caídas. Queda fuera de esta versión. Si algún día se reanudan sesiones, se recreará la tubería
   con el mismo nombre.

### Interfaz

- **Barra de la sesión:** botón **🔌 Complementos**. Su texto cambia con el estado («Abriendo
  IGP_Soft…», «IGP_Soft conectado») y su menú muestra:
  - el nombre;
  - la versión aquí y la versión en el remoto;
  - el estado: listo, versión distinta, no instalado aquí o allí, o abierto;
  - los botones **Abrir IGP_Soft** y **Cerrar IGP_Soft remoto**.
- El ratón solo se reenvía al remoto cuando el puntero está sobre la imagen y no sobre un menú o
  una vista previa. Antes, un clic en el menú también llegaba al PC remoto.
- **Inicio → Ajustes → Complementos:** IGP_Soft detectado en este PC (versión y ruta) y la casilla
  **«Permitir que me controlen con complementos»** (activada por defecto). Si está desactivada, el
  host responde `PluginError{no_permitido}`.
- Los avisos de versión o de falta de instalación llevan el botón **Descargar IGP_Soft**, que abre
  la página de versiones en el navegador.

## Fases (parada para revisión en cada una)

Base nueva **0.4.0** (nueva función); las entregas salen como `v0.4.0-beta.N`.

| Fase | Qué | Cómo se prueba |
|---|---|---|
| P1 | `plugins.rs`: detección en el registro, tuberías overlapped (cliente y servidor), lanzar el proceso | Tests: lectura y escritura simultáneas en las dos puntas sin bloqueo, 1 MB en una línea, cierre de un lado detectado por el otro, detección con claves de prueba en HKCU |
| P2 | Protocolo v4 (`Plugin*`), relevo en host y visor, cola del túnel | Tests de protocolo (índices, ida y vuelta). Prueba local completa: LCVDesk conectado a sí mismo (`127.0.0.1`), IGP_Soft normal en este PC y el panel `--remoto` manejando sus propias pantallas |
| P3 | Interfaz: botón Complementos, ajuste «Permitir», avisos con enlace | Beta `v0.4.0-beta.1` y prueba real entre dos PCs, los dos con IGP_Soft `1.0.2-exp.2` |

## Pruebas hechas (un solo PC, IGP_Soft 1.0.2-exp.2)

- **Tuberías** (`cargo test -p lcvdesk plugins`): 3 MB en cada sentido a la vez sin bloqueo, una
  línea de 1 MB entera, el cierre de un lado se ve en el otro, plazos de `accept`.
- **Túnel con IGP_Soft** (`igp_soft_end_to_end`, `--ignored`): saludo aceptado, estado completo y
  latidos en los dos sentidos.
- **Sesión real** (`igp_soft_through_session`, `--ignored`): host y visor por TCP en 127.0.0.1,
  `PluginOpen` → IGP_Soft abierto en los dos lados en 0,3 s y conectado 14 s después (los latidos
  cruzan); los bytes contados coinciden en los dos extremos.
- **Interfaz:** la app conectada a sí misma, menú Complementos → «Abrir IGP_Soft» → ventana
  «IGP_Soft — Controlando IGPLCV» con los monitores y sus vistas previas.

## Pendiente de probar (del contrato)

- **L2:** IGP_Soft como administrador con LCVDesk sin elevar. Hasta probarlo, se ejecutan los dos
  con el mismo nivel. LCVDesk lo detecta como `no_responde` o «acceso denegado» y lo registra en el
  log con el código de error para facilitar el diagnóstico.
- Control real entre dos PCs.

## Encaje con el contrato

Nada del contrato obliga a cambiar IGP_Soft. Hay dos notas para el futuro:

1. **Servicio SYSTEM** (propuesta pendiente en LCVDesk para ventanas elevadas y Ctrl+Alt+Supr): la
   tubería de IGP_Soft solo admite al usuario de la sesión, así que un host que corriera como SYSTEM
   no podría conectarse. Si se hace ese servicio, el relevo del complemento tendrá que quedarse en
   el proceso de LCVDesk del usuario, que sigue existiendo para la interfaz. No cambia el contrato.
2. **Reconexión:** LCVDesk no la usa por ahora (ver el paso 7 de la secuencia).

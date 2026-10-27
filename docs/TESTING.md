# Pruebas entre dos PCs

## Preparación (una vez)

1. Generar el instalador en el PC de desarrollo:
   ```powershell
   .\tools\build-msi.ps1
   ```
   Resultado: `dist\LCVDesk-<versión>-x64.msi`.
2. Copiar el MSI a los dos PCs (carpeta compartida, USB…) e instalarlo con doble clic
   (pide permisos de administrador). Instala en `C:\Program Files\LCVDesk`, crea el acceso
   "LCVDesk" en el menú Inicio y abre el puerto **TCP 47070** en el firewall solo para la subred local.
3. Ambos PCs deben estar en la misma subred (p. ej. `192.168.0.x`).

Para actualizar: instalar el MSI nuevo encima (sustituye la versión anterior).
Para desinstalar: Configuración → Aplicaciones → LCVDesk → Desinstalar.

Registro de diagnóstico de cada PC: `%LOCALAPPDATA%\LCVDesk\lcvdesk.log`.

---

## Fase 4.1 — Captura y transmisión de video

> Versión de pruebas **sin cifrar**: usar solo en la red de la oficina.

Llamamos **PC-A** al que se controla y **PC-B** al que mira.

1. Abrir LCVDesk en los dos PCs (menú Inicio → LCVDesk).
2. En **PC-A**, anotar la IP que aparece en la barra superior: `Este equipo: NOMBRE · 192.168.0.20:47070`.
3. En **PC-B**, escribir esa IP y pulsar **Conectar**.
4. En **PC-A** aparece "El equipo «PC-B» quiere ver tu pantalla. ¿Permitir?" → **Sí**.
5. En **PC-B** se ve la pantalla de PC-A. En PC-A la barra superior muestra en rojo
   "Tu pantalla está siendo vista por «PC-B»".

### Qué comprobar

| Prueba | Resultado esperado |
|---|---|
| Rechazar la solicitud en PC-A (**No**) | PC-B muestra "El usuario rechazó la conexión" |
| Mover ventanas / reproducir un video en PC-A | Imagen fluida en PC-B, ~60 fps |
| Pantalla quieta en PC-A | fps baja a ~0 y el ancho de banda casi a 0 (solo se envían cambios) |
| Botón **Desconectar** en PC-B o en PC-A | Ambos vuelven al estado inicial |
| Cerrar LCVDesk en PC-A durante la sesión | PC-B muestra "Conexión perdida" |
| Pulsar Win+L o abrir una ventana UAC en PC-A | La imagen se congela o se pone negra; al volver al escritorio se recupera sola (limitación conocida hasta la Fase 4.6) |
| Cambiar la resolución de PC-A | La imagen se adapta al nuevo tamaño |

### Medir la latencia

La barra de PC-B muestra:

`1920x1080  60 fps  12.3 Mbps | red (ida y vuelta) 0.8 ms  host 6.1 ms  decod. 3.2 ms  pres. 0.5 ms`

| Campo | Significado | Valor orientativo en LAN gigabit |
|---|---|---|
| red (ida y vuelta) | Ping de la aplicación PC-B → PC-A → PC-B | < 2 ms cableado, 2–10 ms Wi-Fi |
| host | Conversión de color + codificación H.264 en PC-A | 4–12 ms (software) |
| decod. | Decodificación + conversión a RGBA en PC-B | 2–6 ms |
| pres. | Desde que llega el fotograma hasta que se sube a la GPU | < 2 ms |

Latencia aproximada extremo a extremo ≈ host + red/2 + decod. + pres. + hasta un refresco del monitor.

**Prueba visual**: abrir en PC-A un cronómetro con milisegundos (p. ej. https://www.online-stopwatch.com
o `Get-Date -Format HH:mm:ss.fff` en un bucle), poner los dos monitores uno al lado del otro y hacer una
foto con el móvil a ambos: la diferencia entre los dos valores es la latencia real.

### Ajustes de rendimiento (opcional)

Variables de entorno en PC-A antes de abrir LCVDesk (por ejemplo desde PowerShell):

```powershell
$env:LCVDESK_FPS = 30; $env:LCVDESK_BITRATE_MBPS = 15
& 'C:\Program Files\LCVDesk\lcvdesk.exe'
```

| Variable | Defecto | Efecto |
|---|---|---|
| `LCVDESK_FPS` | 60 | Fotogramas por segundo máximos |
| `LCVDESK_BITRATE_MBPS` | 25 | Ancho de banda objetivo del codificador |
| `LCVDESK_PORT` | 47070 | Puerto TCP (si se cambia, abrirlo en el firewall y escribir `IP:puerto` al conectar) |
| `LCVDESK_KEYFRAME_SEC` | 2 | Cada cuántos segundos se envía un fotograma clave (menor = recupera antes de un corte, más ancho de banda) |

## Fase 4.2 y mejoras — Control, varias pantallas, varias subredes, UAC

### Mouse y teclado
1. Conecta como antes y acepta en PC-A.
2. En PC-B, **haz clic sobre la imagen**: aparece "Controlando: tu ratón y teclado van al equipo remoto".
3. Mueve el ratón, haz clic, usa la rueda y escribe en PC-A desde PC-B.

| Prueba | Esperado |
|---|---|
| Mover el ratón y hacer clic | El puntero de PC-A sigue al de PC-B dentro de la imagen |
| Escribir texto (acentos, ñ) | Aparece correcto en PC-A |
| Atajos: Ctrl+C, Ctrl+V, Alt+Tab | Funcionan en PC-A |
| Clic fuera de la imagen | Se suelta el control; las teclas no quedan "pegadas" |
| Desconectar durante una tecla pulsada | PC-A no se queda con teclas pegadas (se envía "soltar todo") |

Limitación: la tecla **Windows** sola y algunas combinaciones del sistema pueden no pasar; los scancodes
cubren la mayoría del uso normal. El teclado va por eventos de la ventana (no por hook de bajo nivel todavía).

### Varias pantallas
Si PC-A tiene más de un monitor, en PC-B aparece un desplegable **Monitor** en la barra. Cámbialo y la
vista salta a ese monitor en vivo. El marcado con ★ es el principal.

### Varias subredes
El MSI abre el puerto 47070 en los perfiles **Privado** y **de Dominio**, así que funciona aunque PC-A
y PC-B estén en subredes distintas de la oficina (p. ej. `192.168.0.x` y `192.168.1.x`), siempre que haya
enrutado entre ellas. Puedes conectar por **IP** o por **nombre de equipo** (`PC-TALLER`).

Para restringir a rangos concretos (recomendado si la red es grande), en PC-A como administrador:
```powershell
cd 'C:\Program Files\LCVDesk'   # o la carpeta tools del proyecto
.\firewall.ps1 -Ranges '192.168.0.0/24','192.168.1.0/24'
.\firewall.ps1 -Reset           # vuelve a Privado+Dominio
```

### Ejecutar programas de administrador (UAC) en remoto
Por defecto, cuando en PC-A salta un aviso de UAC (p. ej. al abrir un instalador), **la pantalla se
atenúa** y no se puede tocar en remoto. Para permitirlo:

1. En **PC-A**, en la pantalla de inicio de LCVDesk, marca
   **"Mostrar los avisos de UAC en el escritorio normal"**.
2. Windows pedirá confirmación de administrador (una vez).
3. A partir de ahí, los avisos de UAC salen en el escritorio normal y se ven y se aceptan en remoto.
4. Para restaurar la protección, desmarca la casilla.

| Prueba | Esperado |
|---|---|
| Casilla desmarcada + abrir algo como admin en PC-A | La vista remota se atenúa/congela (no se puede aceptar) |
| Casilla marcada + abrir algo como admin en PC-A | El aviso de UAC se ve en PC-B y se puede pulsar "Sí" |

No afecta a la pantalla de bloqueo ni al inicio de sesión (Win+L + PIN): eso sigue fuera de alcance.

### Prueba en un solo PC

Se puede conectar a `127.0.0.1` desde el mismo equipo: se verá el "efecto espejo" infinito, pero sirve
para comprobar que todo funciona antes de pasar a dos PCs.

### Diagnóstico de rendimiento (fps)

El host escribe cada 2 s una línea en `%LOCALAPPDATA%\LCVDesk\lcvdesk.log` durante la sesión:

```
video: enviados 58 fps, descartados 2 fps, codif. media 9.3 ms, 14.2 Mbps, captura Dxgi
```

- **captura Dxgi**: bien. **captura Gdi**: la aceleración por hardware no está disponible en ese equipo
  (fps bajos y más CPU); conviene revisar por qué DXGI falló (mira líneas `DXGI no disponible`).
- **enviados** bajos + **descartados** altos: el visor o la red no dan abasto → baja
  `LCVDESK_FPS` o `LCVDESK_BITRATE_MBPS` en el host.
- **codif. media** alta (>20 ms): la CPU del host es el límite; baja la resolución, los fps, o espera
  al códec por GPU (Fase 4.1b).

Ajustes rápidos en el host (PowerShell) antes de abrir LCVDesk:
```powershell
$env:LCVDESK_FPS = 30; $env:LCVDESK_KEYFRAME_SEC = 1
& 'C:\Program Files\LCVDesk\lcvdesk.exe'
```

### Si algo falla

| Síntoma | Causa probable |
|---|---|
| "Conexión perdida: ... tiempo de espera" al conectar | LCVDesk no está abierto en PC-A, IP incorrecta o firewall (¿subred distinta?) |
| "No se puede recibir conexiones" en la barra | Otro programa usa el puerto 47070 |
| Imagen negra permanente | PC-A en escritorio seguro (UAC / bloqueo) o captura no disponible: revisar el log |

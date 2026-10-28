# Publicar una versión y actualización automática

LCVDesk se actualiza desde **GitHub Releases**. Al hacer push de una etiqueta `vX.Y.Z`, un flujo de
GitHub Actions compila el MSI, calcula su SHA-256 y crea la Release. La app comprueba esas Releases y
ofrece instalar la nueva versión.

## Configurar el repositorio (una vez)

1. Crear el repositorio en GitHub y subir el proyecto.
2. Indicar a la app de qué repositorio leer las versiones. Dos formas:
   - Editar `DEFAULT_REPO` en [app/lcvdesk/src/updater.rs](../app/lcvdesk/src/updater.rs) (por defecto `LCV2107/LCVDesk`), **o**
   - Definir la variable de entorno `LCVDESK_UPDATE_REPO=usuario/repo` en los equipos.
3. Si el repositorio es privado, el updater de la app (que no se autentica) no verá las Releases;
   para actualización automática el repositorio debe ser **público**, o las Releases deben serlo.

## Ramas y canales

- Rama **`main`** = versiones **estables**. Rama **`beta`** = integración y pruebas.
- La app tiene un **canal** (Ajustes → *Canal de actualizaciones*): **Estable** (por defecto) o **Beta**.
  - Estable: solo recibe Releases finales.
  - Beta: recibe también los **prerelease** (y las estables cuando salen).
- Versionado: las betas apuntan a la próxima estable → `0.2.0-beta.1`, `-beta.2`… y luego `0.2.0`.
  `Cargo.toml` guarda solo la **base** (`0.2.0`); el sufijo viene de la etiqueta y se incrusta en el
  binario vía `LCVDESK_VERSION` (por eso el mismo commit probado en beta puede re-etiquetarse estable).

## Publicar una beta

1. Trabajar en la rama `beta`. `Cargo.toml` tiene la **base** de la próxima versión (p. ej. `0.2.0`).
2. Confirmar y etiquetar con el sufijo:
   ```bash
   git switch beta
   git commit -am "cambios de la beta"
   git tag v0.2.0-beta.1
   git push origin beta
   git push origin v0.2.0-beta.1
   ```
3. El flujo publica un **prerelease** `LCVDesk-0.2.0-beta.1-x64.msi` (+ `.sha256`). Lo reciben solo los
   equipos en canal **Beta**.

## Promocionar a estable (tras las pruebas)

El mismo commit probado se etiqueta como estable y se fusiona a `main` (no se recompila algo nuevo):
```bash
git tag v0.2.0            # sobre el MISMO commit probado en beta
git push origin v0.2.0    # el flujo publica la Release estable
git switch main && git merge --ff-only beta && git push origin main
```

## Actualizaciones críticas (obligatorias)

- Por defecto las actualizaciones **no son obligatorias**: la app solo muestra un aviso y el usuario
  decide cuándo instalar.
- Para **forzar** una actualización (p. ej. un fallo de seguridad), incluir la marca **`[critical]`**
  en las notas de la Release. La app mostrará una pantalla de *Actualización obligatoria* que solo deja
  actualizar o salir (no interrumpe una sesión en curso; el bloqueo aparece al volver al inicio).

## Cómo actualiza el usuario

- Al abrir LCVDesk comprueba en segundo plano si hay una versión nueva en su canal.
- Si la hay y **no es crítica**: aparece un aviso discreto arriba («Actualización disponible») con
  **Instalar** o **Ahora no**; también en Ajustes → *Actualizaciones*.
- Si es **crítica**: pantalla de actualización obligatoria.
- Al instalar: descarga el MSI, **verifica su SHA-256**, pide confirmación de administrador (UAC) y lo
  instala encima. El instalador **espera a que LCVDesk se cierre** antes de ejecutar msiexec (si el
  .exe sigue abierto no se puede reemplazar); al terminar se vuelve a abrir.
- **Volver a estable:** con una beta instalada, elegir el canal *Estable* ofrece «Volver a la estable».
  Si la estable es más antigua que la beta, se desinstala la beta y se instala la estable (el anclaje
  a la barra de tareas puede perderse en ese caso).

## Versión del MSI

El MSI no admite sufijos, así que `build-msi.ps1` le da una versión numérica que crece en **cada**
publicación, también entre betas: `mayor.menor.(parche*1000 + rango)`, con alpha.N = N, beta.N = 100+N,
rc.N = 200+N y final = 999 (p. ej. `0.3.1-beta.2` → `0.3.1102`, `0.3.1` → `0.3.1999`). El `.exe` lleva
además su versión de archivo (`0.3.1.20002`), para que Windows Installer siempre lo reemplace.

## Notas

- La verificación SHA-256 es obligatoria: si la Release no trae el `.sha256`, la app no instala.
- Build local del MSI y su huella: `.\tools\build-msi.ps1 [-Version 0.2.0-beta.1]` (genera el `.sha256`).
- Para probar una beta por primera vez desde un equipo en estable: instalar el MSI de la beta a mano
  una vez (el updater estable no ofrece prereleases); a partir de ahí, cambiar el canal a Beta.

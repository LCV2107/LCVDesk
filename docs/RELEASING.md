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

## Publicar una versión nueva

1. Subir la versión en [Cargo.toml](../Cargo.toml) → `[workspace.package] version = "0.3.0"`.
2. Confirmar y etiquetar con el **mismo** número, con prefijo `v`:
   ```bash
   git commit -am "LCVDesk 0.3.0"
   git tag v0.3.0
   git push origin main --tags
   ```
3. El flujo [release.yml](../.github/workflows/release.yml) compila y publica solo. La Release queda con
   `LCVDesk-0.3.0-x64.msi` y `LCVDesk-0.3.0-x64.msi.sha256`.
   - El cuerpo de la Release son las notas (créalo en GitHub o deja que ponga un texto por defecto).
   - La etiqueta debe coincidir con `Cargo.toml` o el flujo falla a propósito.

## Cómo actualiza el usuario

- Al abrir LCVDesk, comprueba en segundo plano si hay una versión mayor.
- En la pantalla de inicio, sección **Actualizaciones**: muestra la versión instalada, un botón
  **Buscar actualizaciones** y, si hay una nueva, **Descargar e instalar**.
- Al instalar: descarga el MSI, **verifica su SHA-256**, pide confirmación de administrador (UAC) y lo
  instala encima (actualización mayor). LCVDesk se cierra para no bloquear archivos; al terminar, se
  vuelve a abrir desde el menú Inicio.

## Notas

- Las versiones **prerelease** de GitHub se ignoran (solo se ofrecen las estables).
- La verificación SHA-256 es obligatoria: si la Release no trae el `.sha256`, la app no instala.
- Build local del MSI y su huella: `.\tools\build-msi.ps1` (genera también el `.sha256`).

# Código de referencia

Copias de solo lectura de proyectos externos, usadas como fuente para estudiar y copiar código.
**No forman parte del build** de LCVDesk ni se versionan dentro de este repositorio (ver `.gitignore`).

| Carpeta | Origen | Commit | Fecha | Licencia |
|---|---|---|---|---|
| `rustdesk/` | https://github.com/rustdesk/rustdesk | `828d1101afd2e656eb1c1d18511009bdf5336238` | 2026-10-22 | AGPL-3.0 |
| `rustdesk/libs/hbb_common/` | https://github.com/rustdesk/hbb_common (submódulo) | `edefa08b798b7fbf70b274a4cdbf701c9e06f7a3` | 2026-10-22 | AGPL-3.0 |
| `hwcodec/` | https://github.com/rustdesk-org/hwcodec | `ec38ebaf2454269d4c5668a57e3813c6e5541be7` | 2026-10-21 | No declarada |

Para volver a descargarlas en estos mismos commits:

```powershell
.\tools\fetch-references.ps1
```

Cuando se copie código de aquí a `crates/`, anotar en el archivo copiado el origen (ruta + commit) y
mantener el aviso de copyright original.

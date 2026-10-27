# Compila LCVDesk en release y genera dist\LCVDesk-<versión>-x64.msi
# Requisitos: Rust (MSVC), Windows SDK, WiX 5 (dotnet tool install --global wix --version 5.0.2)
# y la extensión de firewall (wix extension add -g WixToolset.Firewall.wixext/5.0.2).
#
# -Version admite el sufijo de prerelease (p. ej. 0.2.0-beta.1). La versión del MSI es siempre la
# base numérica (0.2.0, el MSI no admite sufijos); la versión completa se incrusta en el binario
# vía LCVDESK_VERSION y da nombre al archivo. Sin -Version se toma la de Cargo.toml.
param([string]$Version, [switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..')
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:USERPROFILE\.dotnet\tools;$env:Path"

Push-Location $root
try {
    if (-not $Version) {
        $Version = ((cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages |
            Where-Object name -eq 'lcvdesk').version
    }
    $base = ($Version -split '-')[0]   # versión numérica para el MSI (sin -beta.N)
    # La versión completa (con sufijo) se incrusta en el binario para que el updater la compare.
    $env:LCVDESK_VERSION = $Version

    if (-not $SkipBuild) {
        cargo build --release -p lcvdesk
        if ($LASTEXITCODE) { throw 'cargo build falló' }
    }
    New-Item -ItemType Directory -Force dist | Out-Null
    $msi = "dist\LCVDesk-$Version-x64.msi"
    wix build installer\lcvdesk.wxs -arch x64 -ext WixToolset.Firewall.wixext `
        -d Version=$base -d BinDir=target\release -o $msi
    if ($LASTEXITCODE) { throw 'wix build falló' }

    # Huella SHA-256 que el updater verifica tras descargar.
    $hash = (Get-FileHash $msi -Algorithm SHA256).Hash.ToLower()
    $name = Split-Path $msi -Leaf
    "$hash  $name" | Out-File -FilePath "$msi.sha256" -Encoding ascii -NoNewline
    Write-Host "MSI generado: $root\$msi"
    Write-Host "SHA-256:      $root\$msi.sha256"
}
finally {
    Pop-Location
}

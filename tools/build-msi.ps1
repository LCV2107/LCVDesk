# Compila LCVDesk en release y genera dist\LCVDesk-<versión>-x64.msi
# Requisitos: Rust (MSVC), Windows SDK, WiX 5 (dotnet tool install --global wix --version 5.0.2)
# y la extensión de firewall (wix extension add -g WixToolset.Firewall.wixext/5.0.2).
#
# -Version admite el sufijo de prerelease (p. ej. 0.2.0-beta.1). El MSI no admite sufijos, así que
# se le da una versión numérica derivada y creciente (ver abajo); la versión completa se incrusta en
# el binario vía LCVDESK_VERSION y da nombre al archivo. Sin -Version se toma la de Cargo.toml.
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
    # Versión del MSI: estrictamente creciente en cada publicación, también entre betas, para que
    # Windows Installer siempre la trate como actualización. Formato mayor.menor.(parche*1000+rango):
    # alpha.N = N, beta.N = 100+N, rc.N = 200+N, final = 999. P. ej. 0.3.1-beta.2 -> 0.3.1102,
    # 0.3.1 -> 0.3.1999. (El campo admite hasta 65535: vale para parches 0..64.)
    $core, $pre = $Version -split '-', 2
    $maj, $min, $pat = ($core -split '\.') + @('0', '0') | Select-Object -First 3
    $rank = 999
    if ($pre) {
        $n = if ($pre -match '(\d+)$') { [Math]::Min([int]$Matches[1], 99) } else { 0 }
        $rank = switch -Regex ($pre) { '^rc' { 200 + $n } '^beta' { 100 + $n } default { $n } }
    }
    $base = "$maj.$min.$([int]$pat * 1000 + $rank)"
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

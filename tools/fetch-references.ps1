# Descarga los repositorios de referencia en los commits documentados en reference/README.md.
$ErrorActionPreference = 'Stop'
$root = Join-Path $PSScriptRoot '..\reference'

$refs = @(
    @{ Dir = 'rustdesk'; Url = 'https://github.com/rustdesk/rustdesk.git'; Commit = '828d1101afd2e656eb1c1d18511009bdf5336238'; Submodules = $true },
    @{ Dir = 'hwcodec';  Url = 'https://github.com/rustdesk-org/hwcodec.git'; Commit = 'ec38ebaf2454269d4c5668a57e3813c6e5541be7'; Submodules = $false }
)

foreach ($r in $refs) {
    $path = Join-Path $root $r.Dir
    if (-not (Test-Path $path)) {
        git init -q $path
        git -C $path remote add origin $r.Url
    }
    git -C $path fetch -q --depth 1 origin $r.Commit
    git -C $path checkout -q FETCH_HEAD
    if ($r.Submodules) {
        git -C $path submodule update -q --init --depth 1 --recursive
    }
    Write-Host "$($r.Dir) -> $(git -C $path rev-parse HEAD)"
}

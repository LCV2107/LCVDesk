# Restringe (o reabre) la regla de firewall de LCVDesk a rangos de red concretos.
# Ejecutar como administrador DESPUÉS de instalar el MSI.
#
# Ejemplos:
#   .\firewall.ps1 -Ranges '192.168.0.0/24','192.168.1.0/24'   # solo esas subredes
#   .\firewall.ps1 -Reset                                       # volver a Privado+Dominio (como el MSI)
param(
    [string[]]$Ranges,
    [switch]$Reset,
    [int]$Port = 47070
)
$ErrorActionPreference = 'Stop'
$name = 'LCVDesk (TCP 47070)'

Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule

if ($Reset -or -not $Ranges) {
    New-NetFirewallRule -DisplayName $name -Direction Inbound -Action Allow `
        -Protocol TCP -LocalPort $Port -Profile Domain,Private | Out-Null
    Write-Host "Regla restablecida: perfiles Dominio y Privado, puerto $Port."
}
else {
    New-NetFirewallRule -DisplayName $name -Direction Inbound -Action Allow `
        -Protocol TCP -LocalPort $Port -Profile Domain,Private -RemoteAddress $Ranges | Out-Null
    Write-Host "Regla aplicada al puerto $Port solo para: $($Ranges -join ', ')"
}

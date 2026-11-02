# Restringe (o reabre) la regla de firewall de IGPDesk a rangos de red concretos.
# Ejecutar como administrador DESPUÉS de instalar el MSI.
#
# Ejemplos:
#   .\firewall.ps1 -Ranges '192.168.0.0/24','192.168.1.0/24'   # solo esas subredes
#   .\firewall.ps1 -Reset                                       # volver a Privado+Dominio (como el MSI)
param(
    [string[]]$Ranges,
    [switch]$Reset,
    [int]$Port = 47070,
    [int]$DiscoveryPort = 47071
)
$ErrorActionPreference = 'Stop'
$name = 'IGPDesk (TCP 47070)'
$discoName = 'IGPDesk (descubrimiento UDP 47071)'

# También las de antes del cambio de nombre (LCVDesk), por si quedó alguna creada con este script.
foreach ($n in $name, $discoName, 'LCVDesk (TCP 47070)', 'LCVDesk (descubrimiento UDP 47071)') {
    Get-NetFirewallRule -DisplayName $n -ErrorAction SilentlyContinue | Remove-NetFirewallRule
}

if ($Reset -or -not $Ranges) {
    New-NetFirewallRule -DisplayName $name -Direction Inbound -Action Allow `
        -Protocol TCP -LocalPort $Port -Profile Domain,Private | Out-Null
    New-NetFirewallRule -DisplayName $discoName -Direction Inbound -Action Allow `
        -Protocol UDP -LocalPort $DiscoveryPort -Profile Domain,Private | Out-Null
    Write-Host "Reglas restablecidas: perfiles Dominio y Privado, TCP $Port y UDP $DiscoveryPort."
}
else {
    New-NetFirewallRule -DisplayName $name -Direction Inbound -Action Allow `
        -Protocol TCP -LocalPort $Port -Profile Domain,Private -RemoteAddress $Ranges | Out-Null
    New-NetFirewallRule -DisplayName $discoName -Direction Inbound -Action Allow `
        -Protocol UDP -LocalPort $DiscoveryPort -Profile Domain,Private -RemoteAddress $Ranges | Out-Null
    Write-Host "Reglas aplicadas (TCP $Port y UDP $DiscoveryPort) solo para: $($Ranges -join ', ')"
}

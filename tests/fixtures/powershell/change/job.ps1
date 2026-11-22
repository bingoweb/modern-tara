$untouched = Get-WmiObject Win32_BIOS # UNCHANGED
$os = Get-WmiObject Win32_OperatingSystem # OLD:PSAvoidUsingWMICmdlet:modern
$os2 = Get-CimInstance -ClassName Win32_OperatingSystem # DECOY
gci | Out-Null # DECOY
$v = $null ?? 'x' # OLD:PSUseCompatibleSyntax:baseline
Write-Output -InputObject $v # DECOY

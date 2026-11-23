function Get-Bios {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidUsingWMICmdlet', '')]
    param()
    $b = Get-WmiObject Win32_BIOS # OLD:PSAvoidUsingWMICmdlet:modern
    $b
}

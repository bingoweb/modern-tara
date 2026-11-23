# modern-tara's PSScriptAnalyzer runner. Reads a job file (groups of files,
# each with its own settings file), writes the findings as UTF-8 JSON to -Out.
param(
    [Parameter(Mandatory = $true)][string]$Job,
    [Parameter(Mandatory = $true)][string]$Out
)
$ErrorActionPreference = 'Stop'
$result = [ordered]@{ pssa = $null; missing = $false; results = @(); errors = @(); failed = @() }
$module = Get-Module -ListAvailable -Name PSScriptAnalyzer | Sort-Object Version -Descending | Select-Object -First 1
if (-not $module) {
    $result.missing = $true
} else {
    $result.pssa = $module.Version.ToString()
    Import-Module $module -ErrorAction Stop
    $spec = Get-Content -LiteralPath $Job -Raw -Encoding utf8 | ConvertFrom-Json
    $found = [System.Collections.Generic.List[object]]::new()
    foreach ($group in $spec.groups) {
        foreach ($file in $group.files) {
            try {
                $records = Invoke-ScriptAnalyzer -Path $file -Settings $group.settings -IncludeSuppressed -ErrorAction Stop
                foreach ($r in $records) {
                    $found.Add([ordered]@{
                        RuleName   = $r.RuleName
                        Severity   = $r.Severity.ToString()
                        ScriptPath = $r.ScriptPath
                        Line       = $r.Line
                        Column     = $r.Column
                        Message    = $r.Message
                    })
                }
            } catch {
                $result.errors += "${file}: $($_.Exception.Message)"
                $result.failed += $file
            }
        }
    }
    $result.results = $found.ToArray()
}
$json = ConvertTo-Json -InputObject $result -Depth 4 -Compress
[System.IO.File]::WriteAllText($Out, $json, [System.Text.UTF8Encoding]::new($false))

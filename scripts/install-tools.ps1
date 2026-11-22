#Requires -Version 7.4
# Installs the external tools modern-tara runs, at the exact versions in
# tools.toml, for the current user only. Every download is checked against
# its SHA-256 in tools.toml. Safe to run again: a tool that is already at its
# pinned version is left alone.
#
#   ruff, ast-grep    GitHub release archive for this platform, the
#                     executable copied to -BinDir
#   PSScriptAnalyzer  Save-PSResource -AsNupkg to a temp folder, checked, then
#                     Install-PSResource -Scope CurrentUser from that folder
#   npm set           npm ci --ignore-scripts in js/ (from js/package-lock.json)
param(
    # Where ruff and ast-grep go; modern-tara also looks here when they are not on PATH.
    [string]$BinDir = (Join-Path $HOME '.local' 'bin'),
    # Only these parts (install-tools.sh runs the PSScriptAnalyzer part through this).
    [ValidateSet('ruff', 'ast-grep', 'PSScriptAnalyzer', 'npm')]
    [string[]]$Only = @('ruff', 'ast-grep', 'PSScriptAnalyzer', 'npm')
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

function Read-ToolsToml([string]$Path) {
    $data = @{}
    $section = ''
    foreach ($raw in Get-Content -LiteralPath $Path -Encoding utf8) {
        $line = $raw.Trim()
        if ($line -eq '' -or $line.StartsWith('#')) { continue }
        if ($line -match '^\[([^\]]+)\]$') { $section = $Matches[1].Trim(); $data[$section] = [ordered]@{}; continue }
        if ($section -and $line -match '^"?([^"=]+?)"?\s*=\s*"([^"]*)"\s*(#.*)?$') { $data[$section][$Matches[1].Trim()] = $Matches[2]; continue }
        throw "tools.toml: cannot read this line: $raw"
    }
    $data
}

# The x.y.z in the first line a tool prints, or '' when it cannot run.
function Get-ToolVersion([string]$Exe) {
    if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) { return '' }
    $line = (& $Exe --version 2>$null | Select-Object -First 1) -as [string]
    if ($line -match '(\d+\.\d+\.\d+)') { return $Matches[1] }
    ''
}

function Get-Sha256([string]$Name) {
    $sha = $toml['sha256'][$Name]
    if (-not $sha) { throw "tools.toml: [sha256] has no entry for $Name" }
    $sha.ToLowerInvariant()
}

function Assert-Sha256([string]$File, [string]$Name) {
    $got = (Get-FileHash -LiteralPath $File -Algorithm SHA256).Hash.ToLowerInvariant()
    $want = Get-Sha256 $Name
    if ($got -ne $want) { throw "${Name}: SHA-256 is $got, tools.toml expects $want" }
}

function New-TempFolder {
    $dir = Join-Path ([IO.Path]::GetTempPath()) "modern-tara-install-$PID-$([guid]::NewGuid().ToString('n').Substring(0, 8))"
    New-Item -ItemType Directory -Force -Path $dir
}

$arch = switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
    'X64' { 'x86_64' }
    'Arm64' { 'aarch64' }
    default { "$_" }
}
$platform = if ($IsWindows) { 'pc-windows-msvc' } elseif ($IsMacOS) { 'apple-darwin' } else { 'unknown-linux-gnu' }
$exeSuffix = if ($IsWindows) { '.exe' } else { '' }

# A release archive from GitHub, checked, its executable copied to $BinDir.
function Install-ReleaseBinary([string]$Tool, [string]$Version, [string]$Url, [string]$Asset) {
    $target = Join-Path $BinDir "$Tool$exeSuffix"
    if ((Get-ToolVersion $target) -eq $Version) { return "$Tool ${Version}: already installed" }
    $null = Get-Sha256 $Asset
    $tmp = New-TempFolder
    try {
        $archive = Join-Path $tmp $Asset
        Invoke-WebRequest -Uri $Url -OutFile $archive -MaximumRetryCount 3
        Assert-Sha256 $archive $Asset
        $out = Join-Path $tmp 'x'
        New-Item -ItemType Directory -Path $out | Out-Null
        if ($Asset.EndsWith('.zip')) { Expand-Archive -LiteralPath $archive -DestinationPath $out -Force } else { & tar -xzf $archive -C $out }
        $exe = Get-ChildItem -LiteralPath $out -Recurse -File -Filter "$Tool$exeSuffix" | Select-Object -First 1
        if (-not $exe) { throw "${Asset}: no $Tool$exeSuffix inside" }
        New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
        Copy-Item -LiteralPath $exe.FullName -Destination $target -Force
        if (-not $IsWindows) { & chmod 0755 $target }
        "$Tool ${Version}: installed to $target"
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

$toml = Read-ToolsToml (Join-Path $root 'tools.toml')
$pins = $toml['tools']
foreach ($name in 'ruff', 'ast-grep', 'PSScriptAnalyzer') {
    if (-not $pins -or -not $pins[$name]) { throw "tools.toml: [tools] has no $name" }
}

if ('ruff' -in $Only) {
    $v = $pins['ruff']
    $asset = if ($IsWindows) { "ruff-$arch-$platform.zip" } else { "ruff-$arch-$platform.tar.gz" }
    Install-ReleaseBinary 'ruff' $v "https://github.com/astral-sh/ruff/releases/download/$v/$asset" $asset
}

if ('ast-grep' -in $Only) {
    $v = $pins['ast-grep']
    $asset = "app-$arch-$platform.zip"
    Install-ReleaseBinary 'ast-grep' $v "https://github.com/ast-grep/ast-grep/releases/download/$v/$asset" $asset
}

if ('PSScriptAnalyzer' -in $Only) {
    $v = $pins['PSScriptAnalyzer']
    if (Get-Module -ListAvailable -Name PSScriptAnalyzer | Where-Object { $_.Version -eq [version]$v }) {
        "PSScriptAnalyzer ${v}: already installed"
    } else {
        $name = "PSScriptAnalyzer.$v.nupkg"
        $null = Get-Sha256 $name
        $tmp = New-TempFolder
        $repo = "modern-tara-$PID"
        try {
            Save-PSResource -Name PSScriptAnalyzer -Version $v -Repository PSGallery -AsNupkg -Path $tmp -TrustRepository -Quiet
            $nupkg = Get-ChildItem -LiteralPath $tmp -File -Filter '*.nupkg' | Where-Object { $_.Name -ieq $name } | Select-Object -First 1
            if (-not $nupkg) { throw "Save-PSResource did not write $name" }
            Assert-Sha256 $nupkg.FullName $name
            Register-PSResourceRepository -Name $repo -Uri $tmp -Trusted
            Install-PSResource -Name PSScriptAnalyzer -Version $v -Repository $repo -Scope CurrentUser -TrustRepository -Quiet
            "PSScriptAnalyzer ${v}: installed"
        } finally {
            Unregister-PSResourceRepository -Name $repo -ErrorAction SilentlyContinue
            Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

if ('npm' -in $Only) {
    # npm ci only when a pinned package is missing or differs; no install scripts run.
    $js = Join-Path $root 'js'
    $stale = @($toml['npm'].GetEnumerator() | Where-Object {
            $pkg = Join-Path $js 'node_modules' $_.Key 'package.json'
            -not (Test-Path -LiteralPath $pkg) -or ((Get-Content -LiteralPath $pkg -Raw | ConvertFrom-Json).version -ne $_.Value)
        })
    if ($stale.Count -eq 0) {
        'npm set: already installed'
    } else {
        if (-not (Get-Command npm -ErrorAction SilentlyContinue)) { throw 'npm not found: install Node.js 26 or newer first.' }
        Push-Location $js
        try {
            npm ci --ignore-scripts --no-audit --no-fund
            if ($LASTEXITCODE -ne 0) { throw "npm ci failed ($LASTEXITCODE)" }
        } finally {
            Pop-Location
        }
    }
}

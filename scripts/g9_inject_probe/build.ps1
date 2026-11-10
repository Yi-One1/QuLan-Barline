# G9 phase-1 minimal read-only probe DLL build script.
#
# Uses cl.exe only. No cmd.exe, no Add-Type, no reg.exe, no bare cargo,
# no Cargo.toml / src/ / root build.ps1 changes. Writes build-result.json
# so the Python host can tell honestly whether phase-1 is executable.
#
# Engineer round: this script only COMPILES. It never loads the DLL into
# Explorer. Phase-1 live execution is operator-only via --allow-inject.

$ErrorActionPreference = 'Continue'

$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$src = Join-Path $here 'probe.c'
$dll = Join-Path $here 'g9_readonly_probe.dll'
$resultPath = Join-Path $here 'build-result.json'

function Write-BuildResult {
    param(
        [bool]$compiled,
        [string]$reason,
        [string]$toolchain,
        [string]$stdoutTail
    )
    $obj = [ordered]@{
        compiled    = $compiled
        reason      = $reason
        toolchain   = $toolchain
        dll         = $dll
        sha256      = ''
        source      = 'probe.c'
        source_sha256 = ''
        stdout_tail = $stdoutTail
        built_at    = (Get-Date).ToString('o')
        note        = 'Compile only. Engineer round does NOT load this DLL into Explorer; phase-1 live run is operator-only.'
    }
    if ($compiled -and (Test-Path -LiteralPath $dll)) {
        try {
            $obj.sha256 = (Get-FileHash -LiteralPath $dll -Algorithm SHA256).Hash.ToLowerInvariant()
        } catch {
            $obj.sha256 = ''
        }
    }
    # P4: record probe.c digest so the host can verify source/binary consistency.
    if (Test-Path -LiteralPath $src) {
        try {
            $obj.source_sha256 = (Get-FileHash -LiteralPath $src -Algorithm SHA256).Hash.ToLowerInvariant()
        } catch {
            $obj.source_sha256 = ''
        }
    }
    $json = $obj | ConvertTo-Json -Depth 4
    $enc = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($resultPath, $json, $enc)
    Write-Host $json
}

if (-not (Test-Path -LiteralPath $src)) {
    Write-BuildResult $false 'probe.c missing' 'none' ''
    exit 1
}

# 1) Find cl.exe: PATH -> vswhere -> direct disk lookup of VS/BuildTools roots.
$clCmd = Get-Command cl.exe -ErrorAction SilentlyContinue
$clPath = ''
if ($clCmd) { $clPath = $clCmd.Source }

function Resolve-ClFromRoots {
    $roots = @(
        "${env:ProgramFiles}\Microsoft Visual Studio",
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio",
        'C:\Program Files\Microsoft Visual Studio',
        'C:\Program Files (x86)\Microsoft Visual Studio',
        'D:\APP\Microsoft Visual Studio',
        'D:\Program Files\Microsoft Visual Studio'
    ) | Select-Object -Unique
    foreach ($root in $roots) {
        if (-not $root -or -not (Test-Path -LiteralPath $root)) { continue }
        $found = Get-ChildItem -LiteralPath $root -Filter 'cl.exe' -Recurse -File `
                 -ErrorAction SilentlyContinue |
                 Where-Object { $_.FullName -like '*\Hostx64\x64\cl.exe' } |
                 Sort-Object FullName -Descending | Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    return ''
}

if (-not $clPath) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path -LiteralPath $vswhere) {
        $vsRoot = & $vswhere -latest -products * -property installationPath 2>$null |
                  Select-Object -First 1
        if ($vsRoot) {
            $msvcDir = Join-Path $vsRoot 'VC\Tools\MSVC'
            $latest = Get-ChildItem -LiteralPath $msvcDir -Directory -ErrorAction SilentlyContinue |
                      Sort-Object Name -Descending | Select-Object -First 1
            if ($latest) {
                $guess = Join-Path $latest.FullName 'bin\Hostx64\x64\cl.exe'
                if (Test-Path -LiteralPath $guess) { $clPath = $guess }
            }
        }
    }
}
if (-not $clPath) { $clPath = Resolve-ClFromRoots }

if (-not $clPath) {
    Write-BuildResult $false 'cl.exe not found (MSVC toolchain unavailable in this session); phase-1 not executable' 'none' ''
    exit 0
}

# 2) Derive INCLUDE / LIB / PATH at PROCESS scope only. No .bat sourcing,
#    no cmd.exe, no Add-Type, no registry, no system changes. Directory
#    lookups are read-only; the variables die with this PowerShell session.
$clDir = Split-Path -Parent $clPath
$msvcRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $clDir))
$kitsRoot = 'C:\Program Files (x86)\Windows Kits\10'
$sdk = Get-ChildItem -LiteralPath (Join-Path $kitsRoot 'Include') -Directory `
       -ErrorAction SilentlyContinue | Sort-Object Name -Descending |
       Select-Object -First 1
$sdkVer = if ($sdk) { $sdk.Name } else { '' }

$includeParts = New-Object System.Collections.ArrayList
[void]$includeParts.Add((Join-Path $msvcRoot 'include'))
$libParts = New-Object System.Collections.ArrayList
[void]$libParts.Add((Join-Path $msvcRoot 'lib\x64'))
if ($sdkVer) {
    $inc = Join-Path $kitsRoot ('Include\' + $sdkVer)
    $lib = Join-Path $kitsRoot ('Lib\' + $sdkVer)
    foreach ($sub in @('ucrt', 'um', 'shared', 'winrt')) {
        $p = Join-Path $inc $sub
        if (Test-Path -LiteralPath $p) { [void]$includeParts.Add($p) }
    }
    foreach ($sub in @('ucrt\x64', 'um\x64')) {
        $p = Join-Path $lib $sub
        if (Test-Path -LiteralPath $p) { [void]$libParts.Add($p) }
    }
}
$env:INCLUDE = ($includeParts -join ';')
$env:LIB = ($libParts -join ';')
$env:PATH = $clDir + ';' + $env:PATH
"INCLUDE=" + $env:INCLUDE
"LIB=" + $env:LIB

# 3) Compile. Any failure is reported honestly; never pretend success.
$prev = Get-Location
try {
    Set-Location -LiteralPath $here
    $output = & $clPath /nologo /LD /W3 "/Fe:$dll" $src /link user32.lib kernel32.lib 2>&1
    $tail = ($output | Out-String)
} finally {
    Set-Location -LiteralPath $prev
}
if ($null -eq $tail) { $tail = '' }
if ($tail.Length -gt 4000) { $tail = $tail.Substring($tail.Length - 4000) }

if (Test-Path -LiteralPath $dll) {
    Write-BuildResult $true 'compiled' $clPath $tail
    exit 0
} else {
    Write-BuildResult $false ('compile failed, cl exit=' + $LASTEXITCODE + ' (see stdout_tail)') $clPath $tail
    exit 0
}

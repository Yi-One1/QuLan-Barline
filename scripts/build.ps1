<#
.SYNOPSIS
    曲栏 Barline 构建脚本 —— 自动注入 MSVC 链接器环境后调用 cargo。

.DESCRIPTION
    为什么需要这个脚本：
    在 Git Bash / MSYS2 环境下构建时，PATH 里的 `usr/bin/link.exe`（GNU coreutils 的 link）
    会抢占 MSVC 的 `link.exe`，导致 `link: extra operand` 之类的诡异报错。
    本脚本用「目录探测 + 环境注入」的方式，把 MSVC 的 Hostx64/x64 放到 PATH 最前面，
    并显式设置 LIB / INCLUDE，使链接器与 Windows SDK 头文件/库稳定命中。

    探测顺序（不依赖 vswhere，因为 BuildTools 有时未注册到 VS Installer 数据库）：
      1. 环境变量 BARLINE_MSVC_ROOT / BARLINE_SDK_ROOT 显式指定
      2. %ProgramFiles% 与 %ProgramFiles(x86)% 下的 Visual Studio\{2022,2019}\{BuildTools,Community,Professional,Enterprise}
      3. vswhere.exe（若可用，作为兜底补充）
      4. Windows SDK: %ProgramFiles(x86)%\Windows Kits\10\Lib\<version>（取最高版本）

.PARAMETER Task
    cargo 子命令，默认 build。可选 build / run / check / clippy / test / clean。

.PARAMETER Profile
    debug 或 release，默认 release。

.PARAMETER NoCargo
    只打印/注入环境变量，不执行 cargo（用于 CI 里自行组合命令）。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\build.ps1
    powershell -ExecutionPolicy Bypass -File scripts\build.ps1 -Task run -Profile debug
#>
[CmdletBinding()]
param(
    [ValidateSet('build', 'run', 'check', 'clippy', 'test', 'clean')]
    [string]$Task = 'build',

    [ValidateSet('debug', 'release')]
    [string]$Profile = 'release',

    [switch]$NoCargo
)

$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------- 工具函数
function Write-Step($msg) { Write-Host "[build] $msg" -ForegroundColor Cyan }
function Write-Ok  ($msg) { Write-Host "[ ok  ] $msg" -ForegroundColor Green }
function Write-Warn2($msg) { Write-Host "[warn ] $msg" -ForegroundColor Yellow }

function Get-VsInstallRoots {
    $roots = New-Object System.Collections.Generic.List[string]

    if ($env:BARLINE_MSVC_ROOT) { $roots.Add($env:BARLINE_MSVC_ROOT) }

    $bases = @($env:ProgramFiles, ${env:ProgramFiles(x86)}) | Where-Object { $_ -and (Test-Path $_) }
    $editions = @('BuildTools', 'Community', 'Professional', 'Enterprise')
    $years = @('2022', '2019')

    foreach ($base in $bases) {
        $vsDir = Join-Path $base 'Microsoft Visual Studio'
        if (-not (Test-Path $vsDir)) { continue }
        foreach ($yearDir in Get-ChildItem -Path $vsDir -Directory -ErrorAction SilentlyContinue) {
            if ($yearDir.Name -notin $years) { continue }
            foreach ($edDir in Get-ChildItem -Path $yearDir.FullName -Directory -ErrorAction SilentlyContinue) {
                if ($edDir.Name -in $editions) { $roots.Add($edDir.FullName) }
            }
        }
    }

    # vswhere 兜底（本机可能查不到，故不作为主路径）
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        try {
            $p = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
            if ($p) { $roots.Add($p.Trim()) }
        } catch { }
    }

    return $roots | Select-Object -Unique
}

function Get-MsvcToolset {
    param([string]$VsRoot)
    $msvcDir = Join-Path $VsRoot 'VC\Tools\MSVC'
    if (-not (Test-Path $msvcDir)) { return $null }
    # 按版本号降序取最新的一个
    $ver = Get-ChildItem -Path $msvcDir -Directory -ErrorAction SilentlyContinue |
        Sort-Object { [version]($_.Name -replace '[^\d\.].*$', '') } -Descending |
        Select-Object -First 1
    if (-not $ver) { return $null }
    $hostDir = Join-Path $ver.FullName 'bin\Hostx64\x64'
    if (-not (Test-Path (Join-Path $hostDir 'link.exe'))) { return $null }
    return [pscustomobject]@{
        VsRoot   = $VsRoot
        Version  = $ver.Name
        BinDir   = $hostDir
        LibDir   = Join-Path $ver.FullName 'lib\x64'
        IncDir   = Join-Path $ver.FullName 'include'
        VcVars   = Join-Path $VsRoot 'VC\Auxiliary\Build\vcvars64.bat'
    }
}

function Get-WindowsSdk {
    # BARLINE_SDK_ROOT 指向 "...\Windows Kits\10" 这一层（版本号由脚本自动挑最新的）
    $kits = if ($env:BARLINE_SDK_ROOT) { $env:BARLINE_SDK_ROOT } else { Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10' }
    if (-not (Test-Path $kits)) { return $null }
    $ver = Get-ChildItem -Path (Join-Path $kits 'Lib') -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^10\.' } |
        Sort-Object { [version]$_.Name } -Descending |
        Select-Object -First 1
    if (-not $ver) { return $null }
    # 注意：Windows Kits 的目录层级是  <root>\Lib\<version>\um\x64 与 <root>\Include\<version>\um
    return [pscustomobject]@{
        Root    = $kits
        Version = $ver.Name
        LibUm   = Join-Path $kits ("Lib\{0}\um\x64"   -f $ver.Name)
        LibUcrt = Join-Path $kits ("Lib\{0}\ucrt\x64" -f $ver.Name)
        IncUm   = Join-Path $kits ("Include\{0}\um"   -f $ver.Name)
        IncUcrt = Join-Path $kits ("Include\{0}\ucrt" -f $ver.Name)
        IncShared = Join-Path $kits ("Include\{0}\shared" -f $ver.Name)
        IncWinrt  = Join-Path $kits ("Include\{0}\winrt"  -f $ver.Name)
    }
}

# ---------------------------------------------------------------- 主流程
Write-Step "探测 MSVC 工具链..."

$toolset = $null
foreach ($root in (Get-VsInstallRoots)) {
    $t = Get-MsvcToolset -VsRoot $root
    if ($t) { $toolset = $t; break }
}
if (-not $toolset) {
    throw "未找到 MSVC 工具链（link.exe）。请安装 Visual Studio BuildTools 的「使用 C++ 的桌面开发」工作负载，或设置 BARLINE_MSVC_ROOT 环境变量指向 VS 安装根目录。"
}
Write-Ok "MSVC $($toolset.Version)  ->  $($toolset.BinDir)"

$sdk = Get-WindowsSdk
if (-not $sdk) {
    throw "未找到 Windows 10/11 SDK（Windows Kits\10\Lib）。请安装 Windows SDK，或设置 BARLINE_SDK_ROOT 指向 ...\Windows Kits\10\<version>。"
}
Write-Ok "Windows SDK     ->  $($sdk.Root)"

# PATH：MSVC bin 必须排在最前，压过 Git 自带的 usr/bin/link.exe
$pathParts = @($toolset.BinDir, $sdk.LibUm, $sdk.LibUcrt) + ($env:PATH -split ';' | Where-Object { $_ })
$env:PATH = ($pathParts | Select-Object -Unique) -join ';'

# LIB：链接期搜索路径（x64 优先，顺序敏感）
$env:LIB = @($sdk.LibUm, $sdk.LibUcrt, $toolset.LibDir) -join ';'

# INCLUDE：编译期（C/C++ 依赖，Rust 自身不需要，但 cl.exe 可能被 build script 调用）
$env:INCLUDE = @($toolset.IncDir, $sdk.IncUm, $sdk.IncUcrt, $sdk.IncShared, $sdk.IncWinrt) -join ';'

Write-Ok "PATH / LIB / INCLUDE 已注入"

# 自检：确认解析到的 link.exe 就是 MSVC 的，而不是 GNU coreutils
$resolved = (Get-Command link.exe -ErrorAction SilentlyContinue).Source
if ($resolved -and $resolved -notlike "*Microsoft Visual Studio*") {
    Write-Warn2 "link.exe 解析到了 $resolved，可能仍被 GNU coreutils 抢占。"
} elseif ($resolved) {
    Write-Ok "link.exe -> $resolved"
}

if ($NoCargo) { Write-Step "已指定 -NoCargo，跳过 cargo 调用"; return }

# ---------------------------------------------------------------- cargo
$cargoArgs = @($Task)
switch ($Task) {
    'build'  { $cargoArgs += @('--profile', $Profile) }
    'run'    { $cargoArgs += @('--profile', $Profile) }
    'clippy' { $cargoArgs += @('--profile', $Profile, '--', '-D', 'warnings') }
    'test'   { $cargoArgs += @('--profile', $Profile) }
    default  { }
}

Write-Step "cargo $($cargoArgs -join ' ')"
& cargo @cargoArgs
if ($LASTEXITCODE -ne 0) { throw "cargo $Task 失败，退出码 $LASTEXITCODE" }

# 体积报告
if ($Task -in @('build', 'run') -and $Profile -eq 'release') {
    $exe = Join-Path $PSScriptRoot '..\target\release\barline.exe'
    if (Test-Path $exe) {
        $sizeKb = [math]::Round((Get-Item $exe).Length / 1KB, 1)
        $sizeMb = [math]::Round((Get-Item $exe).Length / 1MB, 2)
        $limitMb = 8.0
        $color = if ($sizeMb -le $limitMb) { 'Green' } else { 'Red' }
        Write-Host ("[ size] barline.exe = {0} KB ({1} MB) / 目标 {2} MB" -f $sizeKb, $sizeMb, $limitMb) -ForegroundColor $color
    }
}

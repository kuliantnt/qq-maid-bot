[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$repoDir = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$runtimeDir = Join-Path $repoDir "runtime"
$binary = Join-Path $repoDir "target\release\qq-maid-bot.exe"

if ($null -eq (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found; install the Rust MSVC toolchain first"
}

Push-Location $repoDir
try {
    & cargo build --release --workspace
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "release executable not found: $binary"
}

New-Item -ItemType Directory -Path $runtimeDir -Force | Out-Null
Copy-Item -LiteralPath $binary -Destination (Join-Path $runtimeDir "qq-maid-bot.exe") -Force

# Windows 原生安装器与 Release 包保持一致，不安装 Unix Shell 控制脚本。
foreach ($name in @(
    "qbot.ps1",
    "qbot.cmd",
    "botctl.ps1",
    "botctl.cmd",
    "windows-startup-example.bat"
)) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination (Join-Path $runtimeDir $name) -Force
}

# Remove obsolete Unix distribution files without touching private config or runtime data.
foreach ($name in @(
    "botctl.sh",
    "botmon.sh",
    "diagnose-network.sh",
    "validate-runtime.sh",
    "qq-maid-healthcheck.sh",
    "qq-maid-systemd.sh"
)) {
    Remove-Item -LiteralPath (Join-Path $runtimeDir $name) -Force -ErrorAction SilentlyContinue
}

Write-Output "Windows release build installed to: $runtimeDir"
Write-Output "Next: copy runtime\config\.env.example to runtime\config\.env, edit it, then run runtime\botctl.cmd start"

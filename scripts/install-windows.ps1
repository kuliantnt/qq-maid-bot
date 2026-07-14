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

# Windows 原生控制文件与 Release 包保持一致；Shell 脚本一并安装，方便 Git Bash 用户复用。
foreach ($name in @(
    "botctl.sh",
    "botctl.ps1",
    "botctl.cmd",
    "botmon.sh",
    "diagnose-network.sh",
    "validate-runtime.sh",
    "qq-maid-healthcheck.sh",
    "qq-maid-systemd.sh",
    "windows-startup-example.bat"
)) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $name) -Destination (Join-Path $runtimeDir $name) -Force
}

Write-Output "Windows release build installed to: $runtimeDir"
Write-Output "Next: copy runtime\config\.env.example to runtime\config\.env, edit it, then run runtime\botctl.cmd start"

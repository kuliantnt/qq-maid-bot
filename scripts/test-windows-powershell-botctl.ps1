param(
    [Parameter(Mandatory = $true)][string]$NativeBinary
)

$ErrorActionPreference = "Stop"
$repoDir = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$runtimeDir = Join-Path ([IO.Path]::GetTempPath()) ("qq-maid-powershell-" + [Guid]::NewGuid())
$ctl = Join-Path $runtimeDir "botctl.ps1"
$pidFile = Join-Path $runtimeDir "run\qq-maid-bot.pid"

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

New-Item -ItemType Directory -Path (Join-Path $runtimeDir "config") -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $repoDir "scripts\botctl.ps1") -Destination $ctl
Copy-Item -LiteralPath (Join-Path $repoDir "scripts\botctl.cmd") -Destination (Join-Path $runtimeDir "botctl.cmd")
Copy-Item -LiteralPath $NativeBinary -Destination (Join-Path $runtimeDir "qq-maid-bot.exe")

$oldRuntimeDir = $env:QQ_MAID_RUNTIME_DIR
try {
    $env:QQ_MAID_RUNTIME_DIR = $runtimeDir
    $startOutput = (& $ctl start) -join "`n"
    Assert-True ($startOutput.Contains("qq-maid-bot started")) "start did not report success"

    Assert-True (Test-Path -LiteralPath $pidFile) "pid file was not created"
    $botPid = [int](Get-Content -LiteralPath $pidFile -Raw).Trim()
    Assert-True ($null -ne (Get-Process -Id $botPid -ErrorAction SilentlyContinue)) "bot process is not running"

    $statusOutput = (& $ctl status) -join "`n"
    Assert-True ($statusOutput.Contains("qq-maid-bot is running, pid=$botPid")) "status did not find the bot"
    Assert-True ((Get-Content -LiteralPath (Join-Path $runtimeDir "logs\qq-maid-bot.stdout.log") -Raw).Contains("windows smoke started")) "stdout log is missing smoke output"

    $stopOutput = (& $ctl stop) -join "`n"
    Assert-True ($stopOutput.Contains("qq-maid-bot stopped")) "stop did not report success"
    Assert-True ($null -eq (Get-Process -Id $botPid -ErrorAction SilentlyContinue)) "bot process is still running"
    Assert-True (-not (Test-Path -LiteralPath $pidFile)) "pid file was not removed"

    $helpOutput = (& (Join-Path $runtimeDir "botctl.cmd") help) -join "`n"
    Assert-True ($helpOutput.Contains("Usage: botctl.cmd")) "cmd wrapper did not invoke PowerShell controller"
    Write-Output "PowerShell botctl smoke test passed"
} finally {
    if (Test-Path -LiteralPath $pidFile -ErrorAction SilentlyContinue) {
        & $ctl stop 2>$null | Out-Null
    }
    $env:QQ_MAID_RUNTIME_DIR = $oldRuntimeDir
    Remove-Item -LiteralPath $runtimeDir -Recurse -Force -ErrorAction SilentlyContinue
}

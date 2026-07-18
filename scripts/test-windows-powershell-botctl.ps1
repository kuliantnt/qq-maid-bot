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
$oldServerUrl = $env:LLM_SERVER_URL
$oldServerHost = $env:LLM_SERVER_HOST
$oldServerPort = $env:LLM_SERVER_PORT
$oldConsoleEnabled = $env:WEB_CONSOLE_ENABLED
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

    $secretsDir = Join-Path $runtimeDir "config\secrets"
    $tokenFile = Join-Path $secretsDir "bootstrap.token"
    $envFile = Join-Path $runtimeDir "config\.env"
    New-Item -ItemType Directory -Path $secretsDir -Force | Out-Null

    Set-Content -LiteralPath $envFile -Value @(
        "LLM_SERVER_PORT=9988",
        "WEB_CONSOLE_ENABLED=true",
        "LLM_MODEL=openai:legacy-model",
        " export TOOL_CALLING_ENABLED = true",
        "QWEATHER_API_KEY="
    ) -Encoding ASCII
    Set-Content -LiteralPath $tokenFile -Value "qq-maid-bootstrap-v1:1:initialize-secret" -Encoding ASCII
    $guideOutput = (& $ctl start) -join "`n"
    Assert-True ($guideOutput.Contains("--- 首次配置 ---")) "initialize guidance is missing"
    Assert-True ($guideOutput.Contains("http://127.0.0.1:9988/console/")) "custom console port was ignored"
    Assert-True (-not $guideOutput.Contains("initialize-secret")) "initialize token leaked"
    Assert-True (-not $guideOutput.Contains("legacy-model")) "obsolete config value leaked"
    $migratedEnv = Get-Content -LiteralPath $envFile -Raw
    Assert-True ($migratedEnv.Contains("QWEATHER_API_KEY=")) "empty weather key was removed"
    Assert-True (-not $migratedEnv.Contains("LLM_MODEL=")) "legacy model key was not removed"
    Assert-True (-not $migratedEnv.Contains("TOOL_CALLING_ENABLED")) "legacy tool key was not removed"
    $envBackups = @(Get-ChildItem -LiteralPath (Join-Path $runtimeDir "config") -Filter ".env.bak.v0.20.*")
    Assert-True ($envBackups.Count -eq 1) "pre-upgrade env backup was not created exactly once"
    Assert-True ((Get-Content -LiteralPath $envBackups[0].FullName -Raw).Contains("LLM_MODEL=openai:legacy-model")) "env backup lost legacy values"
    & $ctl stop | Out-Null

    Set-Content -LiteralPath $tokenFile -Value "qq-maid-password-reset-v1:2:reset-secret" -Encoding ASCII
    $guideOutput = (& $ctl start) -join "`n"
    Assert-True ($guideOutput.Contains("--- 密码重置待完成 ---")) "password reset guidance is missing"
    Assert-True (-not $guideOutput.Contains("--- 首次配置 ---")) "password reset used initialize guidance"
    Assert-True (-not $guideOutput.Contains("reset-secret")) "password reset token leaked"
    & $ctl stop | Out-Null

    Set-Content -LiteralPath $tokenFile -Value @(
        "qq-maid-bootstrap-v1:3:must-not-leak",
        "unexpected-extra-line"
    ) -Encoding ASCII
    $guideOutput = (& $ctl start) -join "`n"
    Assert-True (-not $guideOutput.Contains("首次配置")) "invalid token used initialize guidance"
    Assert-True (-not $guideOutput.Contains("密码重置")) "invalid token used reset guidance"
    Assert-True (-not $guideOutput.Contains("must-not-leak")) "invalid token content leaked"
    & $ctl stop | Out-Null

    Set-Content -LiteralPath $envFile -Value "WEB_CONSOLE_ENABLED=false" -Encoding ASCII
    Set-Content -LiteralPath $tokenFile -Value "qq-maid-bootstrap-v1:4:disabled-secret" -Encoding ASCII
    $guideOutput = (& $ctl start) -join "`n"
    Assert-True (-not $guideOutput.Contains("首次配置")) "disabled console printed initialize guidance"
    Assert-True (-not $guideOutput.Contains("/console/")) "disabled console printed a URL"
    Assert-True (-not $guideOutput.Contains("disabled-secret")) "disabled token leaked"
    & $ctl stop | Out-Null

    $helpOutput = (& (Join-Path $runtimeDir "botctl.cmd") help) -join "`n"
    Assert-True ($helpOutput.Contains("Usage: botctl.cmd")) "cmd wrapper did not invoke PowerShell controller"
    Write-Output "PowerShell botctl smoke test passed"
} finally {
    if (Test-Path -LiteralPath $pidFile -ErrorAction SilentlyContinue) {
        & $ctl stop 2>$null | Out-Null
    }
    $env:QQ_MAID_RUNTIME_DIR = $oldRuntimeDir
    $env:LLM_SERVER_URL = $oldServerUrl
    $env:LLM_SERVER_HOST = $oldServerHost
    $env:LLM_SERVER_PORT = $oldServerPort
    $env:WEB_CONSOLE_ENABLED = $oldConsoleEnabled
    Remove-Item -LiteralPath $runtimeDir -Recurse -Force -ErrorAction SilentlyContinue
}

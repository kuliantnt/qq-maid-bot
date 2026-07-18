[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [ValidateSet("start", "run", "stop", "restart", "status", "health", "console", "logs", "help")]
    [string]$Command = "help"
)

$ErrorActionPreference = "Stop"

function Get-EnvironmentOverride {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$DefaultValue
    )

    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        return $DefaultValue
    }
    return $value
}

function Resolve-ControlPath {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$BasePath
    )

    if ([IO.Path]::IsPathRooted($Path)) {
        return [IO.Path]::GetFullPath($Path)
    }
    return [IO.Path]::GetFullPath((Join-Path $BasePath $Path))
}

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
if ((Split-Path -Leaf $MyInvocation.MyCommand.Path) -eq "botctl.ps1" -and
    (Test-Path -LiteralPath (Join-Path $scriptDir "config") -PathType Container)) {
    $defaultRuntimeDir = $scriptDir
} else {
    $defaultRuntimeDir = [IO.Path]::GetFullPath((Join-Path $scriptDir "..\runtime"))
}

$runtimeOverride = Get-EnvironmentOverride -Name "QQ_MAID_RUNTIME_DIR" -DefaultValue $defaultRuntimeDir
$runtimeDir = [IO.Path]::GetFullPath($runtimeOverride)
$binary = Resolve-ControlPath `
    -Path (Get-EnvironmentOverride -Name "BOT_BINARY" -DefaultValue "qq-maid-bot.exe") `
    -BasePath $runtimeDir
$pidFile = Resolve-ControlPath `
    -Path (Get-EnvironmentOverride -Name "BOT_PID_FILE" -DefaultValue "run\qq-maid-bot.pid") `
    -BasePath $runtimeDir
$logFile = Resolve-ControlPath `
    -Path (Get-EnvironmentOverride -Name "BOT_LOG_FILE" -DefaultValue "logs\qq-maid-bot.log") `
    -BasePath $runtimeDir
$stdoutLogFile = Resolve-ControlPath `
    -Path (Get-EnvironmentOverride -Name "BOT_STDOUT_LOG_FILE" -DefaultValue "logs\qq-maid-bot.stdout.log") `
    -BasePath $runtimeDir
$stopTimeoutText = Get-EnvironmentOverride -Name "BOT_STOP_TIMEOUT_SECONDS" -DefaultValue "10"
$stopTimeoutSeconds = 0
if (-not [int]::TryParse($stopTimeoutText, [ref]$stopTimeoutSeconds) -or $stopTimeoutSeconds -lt 0) {
    throw "BOT_STOP_TIMEOUT_SECONDS must be a non-negative integer"
}

function Show-Usage {
    @"
Usage: botctl.cmd <command>
       powershell -ExecutionPolicy Bypass -File .\botctl.ps1 <command>

Commands:
  start     Start qq-maid-bot in the background
  run       Run qq-maid-bot in the foreground
  stop      Stop qq-maid-bot
  restart   Restart qq-maid-bot
  status    Show process status
  health    Request /healthz
  console   Show /console/ URL and HTTP status
  logs      Follow the main log file

Environment overrides:
  BOT_BINARY     Path to qq-maid-bot.exe
  BOT_ENV_FILE   Env file to load before starting
  BOT_PID_FILE   PID file path
  BOT_LOG_FILE   Main stderr log path
  BOT_STDOUT_LOG_FILE  Standard output log path
  BOT_STOP_TIMEOUT_SECONDS  Seconds before forced stop (default: 10)
  QQ_MAID_RUNTIME_DIR  Runtime directory containing binary/config/logs
  LINES          Number of log lines for logs command
"@
}

function Get-EnvFile {
    $override = [Environment]::GetEnvironmentVariable("BOT_ENV_FILE")
    if (-not [string]::IsNullOrWhiteSpace($override)) {
        return Resolve-ControlPath -Path $override -BasePath $runtimeDir
    }

    foreach ($candidate in @(
        (Join-Path $runtimeDir "config\.env"),
        (Join-Path $runtimeDir ".env")
    )) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return $candidate
        }
    }
    return $null
}

function Import-DotEnv {
    $envFile = Get-EnvFile
    if ($null -eq $envFile) {
        return
    }
    if (-not (Test-Path -LiteralPath $envFile -PathType Leaf)) {
        throw "env file not found: $envFile"
    }

    # 不使用 Invoke-Expression，避免配置值被当作 PowerShell 代码执行。
    foreach ($line in Get-Content -LiteralPath $envFile) {
        if ($line -notmatch '^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$') {
            continue
        }

        $name = $Matches[1]
        $value = $Matches[2].Trim()
        if ($value.Length -ge 2 -and $value[0] -eq "'" -and $value[$value.Length - 1] -eq "'") {
            $value = $value.Substring(1, $value.Length - 2)
        } elseif ($value.Length -ge 2 -and $value[0] -eq '"' -and $value[$value.Length - 1] -eq '"') {
            $value = $value.Substring(1, $value.Length - 2)
            $value = $value.Replace('\"', '"').Replace('\\', '\')
        } else {
            $value = $value -replace '\s+#.*$', ''
        }
        [Environment]::SetEnvironmentVariable($name, $value, "Process")
    }
}

function Read-BotPid {
    if (-not (Test-Path -LiteralPath $pidFile -PathType Leaf)) {
        return $null
    }
    $text = (Get-Content -LiteralPath $pidFile -Raw).Trim()
    $pidValue = 0
    if (-not [int]::TryParse($text, [ref]$pidValue) -or $pidValue -le 0) {
        return $null
    }
    return $pidValue
}

function Get-BotProcess {
    $pidValue = Read-BotPid
    if ($null -eq $pidValue) {
        return $null
    }

    $process = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
    if ($null -eq $process) {
        return $null
    }

    # PID 可能被系统复用，停止前必须确认它仍指向当前配置的机器人二进制。
    try {
        $processPath = [IO.Path]::GetFullPath($process.Path)
    } catch {
        return $null
    }
    if (-not $processPath.Equals($binary, [StringComparison]::OrdinalIgnoreCase)) {
        return $null
    }
    return $process
}

function Get-ServerUrl {
    $explicitUrl = [Environment]::GetEnvironmentVariable("LLM_SERVER_URL")
    if (-not [string]::IsNullOrWhiteSpace($explicitUrl)) {
        return $explicitUrl.TrimEnd('/')
    }
    $hostName = Get-EnvironmentOverride -Name "LLM_SERVER_HOST" -DefaultValue "127.0.0.1"
    $port = Get-EnvironmentOverride -Name "LLM_SERVER_PORT" -DefaultValue "8787"
    return "http://${hostName}:${port}"
}

function Ensure-ParentDirectory {
    param([Parameter(Mandatory = $true)][string]$Path)
    $parent = Split-Path -Parent $Path
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
    }
}

function Move-PreviousLog {
    param([Parameter(Mandatory = $true)][string]$Path)
    if (Test-Path -LiteralPath $Path -PathType Leaf) {
        Move-Item -LiteralPath $Path -Destination "${Path}.previous" -Force
    }
}

function Start-Bot {
    $running = Get-BotProcess
    if ($null -ne $running) {
        Write-Output "qq-maid-bot is already running, pid=$($running.Id)"
        return
    }
    Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue

    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
        throw "executable not found: $binary"
    }
    Ensure-ParentDirectory -Path $pidFile
    Ensure-ParentDirectory -Path $logFile
    Ensure-ParentDirectory -Path $stdoutLogFile
    Import-DotEnv
    if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable("RUST_LOG"))) {
        [Environment]::SetEnvironmentVariable(
            "RUST_LOG",
            "info,qq_maid_gateway_rs=debug,qq_maid_core=info,tower_http=info",
            "Process"
        )
    }

    # Start-Process 无法把 stdout/stderr 同时追加到一个文件，保留上一轮日志后分流写入。
    Move-PreviousLog -Path $logFile
    Move-PreviousLog -Path $stdoutLogFile
    $process = Start-Process `
        -FilePath $binary `
        -WorkingDirectory $runtimeDir `
        -RedirectStandardOutput $stdoutLogFile `
        -RedirectStandardError $logFile `
        -WindowStyle Hidden `
        -PassThru
    Set-Content -LiteralPath $pidFile -Value $process.Id -Encoding ASCII

    Start-Sleep -Seconds 1
    $process.Refresh()
    if ($process.HasExited) {
        Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
        $lastLines = Get-Content -LiteralPath $logFile -Tail 40 -ErrorAction SilentlyContinue
        if ($null -ne $lastLines) {
            [Console]::Error.WriteLine(($lastLines -join [Environment]::NewLine))
        }
        throw "qq-maid-bot failed to start; see $logFile"
    }
    Write-Output "qq-maid-bot started, pid=$($process.Id), log=$logFile"

    # v0.20+: 新安装或尚无部署管理员时，引导用户用网页完成配置。
    $bootstrapTokenFile = Join-Path $runtimeDir "config\secrets\bootstrap.token"
    if (Test-Path -LiteralPath $bootstrapTokenFile -PathType Leaf) {
        Write-Output ""
        Write-Output "--- 首次配置 ---"
        Write-Output "v0.20 起可以通过网页完成配置，不再必须编辑 config\.env："
        Write-Output "  1. 从启动日志中找到部署管理员 Bootstrap 令牌（仅在首次启动时输出一次）"
        Write-Output "  2. 浏览器打开 http://127.0.0.1:8787/console/"
        Write-Output "  3. 用令牌建立管理员，按向导保存 Provider、平台入口和功能开关"
        Write-Output ""
        Write-Output "如果你仍然习惯手写配置，可以直接编辑 config\.env。"
        Write-Output "更多：https://github.com/kuliantnt/qq-maid-bot/wiki/配置中心"
    }
}

function Run-Bot {
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
        throw "executable not found: $binary"
    }
    Import-DotEnv
    if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable("RUST_LOG"))) {
        [Environment]::SetEnvironmentVariable(
            "RUST_LOG",
            "info,qq_maid_gateway_rs=debug,qq_maid_core=info,tower_http=info",
            "Process"
        )
    }

    Push-Location $runtimeDir
    try {
        & $binary
        if ($LASTEXITCODE -ne 0) {
            throw "qq-maid-bot exited with code $LASTEXITCODE"
        }
    } finally {
        Pop-Location
    }
}

function Invoke-TaskKill {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [switch]$Force
    )

    $arguments = @("/PID", $ProcessId, "/T")
    if ($Force) {
        $arguments += "/F"
    }

    # Windows PowerShell 5.1 会把原生命令 stderr 转成 ErrorRecord；普通停止失败
    # 本来应进入超时后的强制停止，不能被全局 Stop 策略提前中断。
    $previousPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "SilentlyContinue"
        & taskkill.exe @arguments 2>$null | Out-Null
    } finally {
        $ErrorActionPreference = $previousPreference
    }
}

function Stop-Bot {
    $process = Get-BotProcess
    if ($null -eq $process) {
        Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
        Write-Output "qq-maid-bot is not running"
        return
    }

    # taskkill /T 同时处理机器人创建的子进程；超时后再使用 /F 强制结束。
    Invoke-TaskKill -ProcessId $process.Id
    $deadline = [DateTime]::UtcNow.AddSeconds($stopTimeoutSeconds)
    while (-not $process.HasExited -and [DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 200
        $process.Refresh()
    }
    if (-not $process.HasExited) {
        Invoke-TaskKill -ProcessId $process.Id -Force
        Start-Sleep -Milliseconds 300
        $process.Refresh()
    }
    if (-not $process.HasExited) {
        throw "qq-maid-bot is still running after forced stop, pid=$($process.Id)"
    }

    Remove-Item -LiteralPath $pidFile -Force -ErrorAction SilentlyContinue
    Write-Output "qq-maid-bot stopped"
}

function Show-Status {
    $process = Get-BotProcess
    if ($null -eq $process) {
        Write-Output "qq-maid-bot is stopped"
        return
    }
    Write-Output "qq-maid-bot is running, pid=$($process.Id)"
    Write-Output "health: $(Get-ServerUrl)/healthz"
}

function Test-Health {
    Import-DotEnv
    $response = Invoke-WebRequest -Uri "$(Get-ServerUrl)/healthz" -UseBasicParsing -TimeoutSec 15
    Write-Output $response.Content
}

function Test-Console {
    Import-DotEnv
    $url = "$(Get-ServerUrl)/console/"
    $response = Invoke-WebRequest -Uri $url -UseBasicParsing -TimeoutSec 15
    Write-Output "web console: $url -> HTTP $([int]$response.StatusCode)"
}

function Watch-Logs {
    Ensure-ParentDirectory -Path $logFile
    if (-not (Test-Path -LiteralPath $logFile -PathType Leaf)) {
        New-Item -ItemType File -Path $logFile -Force | Out-Null
    }
    $linesText = Get-EnvironmentOverride -Name "LINES" -DefaultValue "80"
    $lineCount = 0
    if (-not [int]::TryParse($linesText, [ref]$lineCount) -or $lineCount -lt 0) {
        throw "LINES must be a non-negative integer"
    }
    Get-Content -LiteralPath $logFile -Tail $lineCount -Wait
}

try {
    switch ($Command) {
        "start" { Start-Bot }
        "run" { Run-Bot }
        "stop" { Stop-Bot }
        "restart" { Stop-Bot; Start-Bot }
        "status" { Show-Status }
        "health" { Test-Health }
        "console" { Test-Console }
        "logs" { Watch-Logs }
        "help" { Show-Usage }
    }
} catch {
    [Console]::Error.WriteLine("error: $($_.Exception.Message)")
    exit 1
}

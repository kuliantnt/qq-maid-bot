$ErrorActionPreference = "Stop"
$repoDir = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ("qq-maid-qbot-" + [Guid]::NewGuid())
$appDir = Join-Path $testRoot "app"
$releaseDir = Join-Path $testRoot "release"
$oldAppDir = $env:QBOT_APP_DIR
$oldServerUrl = $env:LLM_SERVER_URL
$oldServerHost = $env:LLM_SERVER_HOST
$oldServerPort = $env:LLM_SERVER_PORT
$oldConsoleEnabled = $env:WEB_CONSOLE_ENABLED

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

try {
    $env:QBOT_APP_DIR = $appDir
    $env:LLM_SERVER_URL = $null
    $env:LLM_SERVER_HOST = $null
    $env:LLM_SERVER_PORT = $null
    $env:WEB_CONSOLE_ENABLED = $null
    . (Join-Path $repoDir "scripts\qbot.ps1")

    Assert-True (Test-SupportedWindowsArchitecture "AMD64") "Windows AMD64 should be supported"
    Assert-True (Test-SupportedWindowsArchitecture "x86_64") "Windows x86_64 should be supported"
    Assert-True (-not (Test-SupportedWindowsArchitecture "ARM64")) "Windows ARM64 should be rejected"
    Assert-True (Test-AgentConfigResetRequired -CurrentVersion "v0.20.1" -TargetVersion "v0.20.2") "v0.20.1 -> v0.20.2 should reset agent.toml"
    Assert-True (Test-AgentConfigResetRequired -CurrentVersion "v0.20.1" -TargetVersion "v0.21.0") "skipped upgrade should reset agent.toml"
    Assert-True (-not (Test-AgentConfigResetRequired -CurrentVersion "v0.20.2" -TargetVersion "v0.20.3")) "post-reset upgrade should preserve agent.toml"
    Assert-True (-not (Test-AgentConfigResetRequired -CurrentVersion "v0.20.3" -TargetVersion "v0.21.0")) "later upgrade should preserve agent.toml"
    $arm64Error = $null
    try {
        Assert-SupportedWindowsArchitecture "ARM64"
    } catch {
        $arm64Error = $_.Exception.Message
    }
    Assert-True ($null -ne $arm64Error) "Windows ARM64 rejection did not return an error"
    Assert-True ($arm64Error.Contains("Windows x86_64 Release")) "ARM64 error does not explain the available Release"
    Assert-True ($arm64Error.Contains("WSL") -and $arm64Error.Contains("Linux Release")) "ARM64 error does not suggest WSL"

    $wow64Architecture = Resolve-WindowsOperatingSystemArchitecture `
        -RuntimeArchitecture "" `
        -ProcessorArchitecture "x86" `
        -ProcessorArchitectureW6432 "AMD64"
    Assert-True ($wow64Architecture -eq "AMD64") "32-bit PowerShell did not resolve the x64 operating system architecture"

    New-Item -ItemType Directory -Path (Join-Path $releaseDir "config") -Force | Out-Null
    foreach ($name in @("qq-maid-bot.exe", "README.md", "VERSION", "windows-startup-example.bat")) {
        Set-Content -LiteralPath (Join-Path $releaseDir $name) -Value "release" -Encoding ASCII
    }
    foreach ($name in @("botctl.ps1", "botctl.cmd")) {
        Copy-Item -LiteralPath (Join-Path $repoDir "scripts\$name") -Destination (Join-Path $releaseDir $name)
    }
    Set-Content -LiteralPath (Join-Path $releaseDir "config\.env.example") -Value "EXAMPLE=1" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $releaseDir "config\agent.toml") -Value "release-agent" -Encoding ASCII

    $agentTestDir = Join-Path $testRoot "agent-migration"
    New-Item -ItemType Directory -Path $agentTestDir -Force | Out-Null
    $agentTemplate = Join-Path $agentTestDir "template.toml"
    Set-Content -LiteralPath $agentTemplate -Value "new-release-template" -Encoding ASCII

    $agentYes = Join-Path $agentTestDir "yes.toml"
    Set-Content -LiteralPath $agentYes -Value "custom-before-replacement" -Encoding ASCII
    $yesOutput = (Update-AgentConfigFromRelease -ConfigFile $agentYes -TemplateFile $agentTemplate) -join "`n"
    Assert-True ((Get-FileHash $agentYes).Hash -eq (Get-FileHash $agentTemplate).Hash) "upgrade did not install the Release agent template"
    Assert-True ((Get-Content -LiteralPath "${agentYes}.old" -Raw).Contains("custom-before-replacement")) "upgrade did not preserve the old agent config"
    Assert-True ($yesOutput.Contains("旧配置备份: ${agentYes}.old")) "upgrade did not report the agent backup path"
    Assert-True ($yesOutput.Contains("Provider、模型路线、Scene 和工具白名单")) "upgrade did not report required custom configuration work"

    $agentCollision = Join-Path $agentTestDir "collision.toml"
    Set-Content -LiteralPath $agentCollision -Value "current-old-config" -Encoding ASCII
    Set-Content -LiteralPath "${agentCollision}.old" -Value "earlier-backup" -Encoding ASCII
    Update-AgentConfigFromRelease -ConfigFile $agentCollision -TemplateFile $agentTemplate | Out-Null
    Assert-True ((Get-Content -LiteralPath "${agentCollision}.old" -Raw).Contains("earlier-backup")) "existing .old backup was overwritten"
    Assert-True ((Get-Content -LiteralPath "${agentCollision}.old.1" -Raw).Contains("current-old-config")) "replacement did not use the next backup suffix"

    $agentFailure = Join-Path $agentTestDir "failure.toml"
    Set-Content -LiteralPath $agentFailure -Value "original-must-survive" -Encoding ASCII
    $script:AgentMoveCalls = 0
    function Move-Item {
        param([string]$LiteralPath, [string]$Destination)
        $script:AgentMoveCalls++
        if ($script:AgentMoveCalls -eq 2) {
            throw "simulated template activation failure"
        }
        Microsoft.PowerShell.Management\Move-Item -LiteralPath $LiteralPath -Destination $Destination
    }
    $replacementError = $null
    try {
        Replace-AgentConfigFromRelease -ConfigFile $agentFailure -TemplateFile $agentTemplate
    } catch {
        $replacementError = $_.Exception.Message
    } finally {
        Remove-Item Function:\Move-Item -ErrorAction SilentlyContinue
    }
    Assert-True ($null -ne $replacementError -and $replacementError.Contains("original file was restored")) "replacement failure was not reported with rollback"
    Assert-True ((Get-Content -LiteralPath $agentFailure -Raw).Contains("original-must-survive")) "replacement failure did not restore agent.toml"
    Assert-True (-not (Test-Path -LiteralPath "${agentFailure}.old")) "replacement failure left a misleading backup"
    Assert-True (@(Get-ChildItem -LiteralPath $agentTestDir -Filter ".agent.toml.new.*").Count -eq 0) "replacement failure left a temporary file"

    $agentNonInteractive = Join-Path $agentTestDir "noninteractive.toml"
    Set-Content -LiteralPath $agentNonInteractive -Value "noninteractive-must-update" -Encoding ASCII
    $nonInteractiveOutput = (Update-AgentConfigFromRelease -ConfigFile $agentNonInteractive -TemplateFile $agentTemplate) -join "`n"
    Assert-True ((Get-FileHash $agentNonInteractive).Hash -eq (Get-FileHash $agentTemplate).Hash) "non-interactive upgrade did not replace agent.toml"
    Assert-True ((Get-Content -LiteralPath "${agentNonInteractive}.old" -Raw).Contains("noninteractive-must-update")) "non-interactive upgrade did not create a backup"
    Assert-True ($nonInteractiveOutput.Contains("自动备份并更新")) "non-interactive upgrade did not explain the automatic migration"

    $mixedMarker = Join-Path $appDir "config\.agent-config-v0.20.2"
    New-Item -ItemType Directory -Path (Split-Path -Parent $mixedMarker) -Force | Out-Null
    Complete-AgentConfigMigration -CurrentVersion "v0.20.1" -TargetVersion "v0.20.2"
    Assert-True (Test-Path -LiteralPath $mixedMarker -PathType Leaf) "successful updater migration did not create the shared marker"
    Assert-True (-not (Test-AgentConfigResetRequired -CurrentVersion "v0.20.1" -TargetVersion "v0.20.2" -MarkerFile $mixedMarker)) "remote migration marker did not prevent a second updater reset"
    Remove-Item -LiteralPath $mixedMarker -Force
    Complete-AgentConfigMigration -CurrentVersion "v0.20.3" -TargetVersion "v0.21.0"
    Assert-True (Test-Path -LiteralPath $mixedMarker -PathType Leaf) "installed v0.20.2+ did not get the shared marker"

    $failedMarker = Join-Path $appDir "config\failed\.agent-config-v0.20.2"
    $failedReplacementError = $null
    try {
        Replace-AgentConfigFromRelease -ConfigFile (Join-Path $appDir "config\missing.toml") -TemplateFile $agentTemplate
    } catch {
        $failedReplacementError = $_.Exception.Message
    }
    Assert-True ($null -ne $failedReplacementError) "failed agent migration did not return an error"
    Assert-True (-not (Test-Path -LiteralPath $failedMarker)) "failed agent migration left a marker"

    New-Item -ItemType Directory -Path (Join-Path $appDir "config") -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $appDir "data\storage") -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $appDir "logs") -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $appDir "config\.env") -Value @(
        "PRIVATE=keep",
        "LLM_MODEL=openai:legacy-model",
        " export TOOL_CALLING_ENABLED = true",
        "TODO_MODEL=legacy-todo-model",
        "QWEATHER_API_KEY="
    ) -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $appDir "config\agent.toml") -Value "custom-agent" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $appDir "data\storage\app.db") -Value "db" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $appDir "logs\qq-maid-bot.log") -Value "log" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $appDir "botctl.sh") -Value "obsolete" -Encoding ASCII

    Install-ReleasePayload -ReleaseDir $releaseDir -Version "v9.9.9"
    $migratedEnv = Get-Content -LiteralPath (Join-Path $appDir "config\.env") -Raw
    Assert-True ($migratedEnv.Contains("PRIVATE=keep")) "private config was overwritten"
    Assert-True ($migratedEnv.Contains("QWEATHER_API_KEY=")) "empty weather key was removed"
    Assert-True (-not $migratedEnv.Contains("LLM_MODEL=")) "legacy model key was not removed"
    Assert-True (-not $migratedEnv.Contains("TOOL_CALLING_ENABLED")) "legacy tool key was not removed"
    Assert-True (-not $migratedEnv.Contains("TODO_MODEL=")) "legacy todo key was not removed"
    $envBackups = @(Get-ChildItem -LiteralPath (Join-Path $appDir "config") -Filter ".env.bak.v0.20.*")
    Assert-True ($envBackups.Count -eq 1) "pre-upgrade env backup was not created exactly once"
    Assert-True ((Get-Content -LiteralPath $envBackups[0].FullName -Raw).Contains("LLM_MODEL=openai:legacy-model")) "env backup lost legacy values"
    Assert-True ((Get-Content -LiteralPath (Join-Path $appDir "data\storage\app.db") -Raw).Contains("db")) "database was overwritten"
    Assert-True ((Get-Content -LiteralPath (Join-Path $appDir "logs\qq-maid-bot.log") -Raw).Contains("log")) "log was overwritten"
    Assert-True (Test-Path -LiteralPath (Join-Path $appDir "config\agent.toml.release-v9.9.9")) "agent.toml update candidate is missing"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $appDir "botctl.sh"))) "Unix control script was not removed"
    Assert-True (Test-Path -LiteralPath (Join-Path $appDir "qbot.cmd")) "qbot.cmd was not installed"

    Invoke-ConfigCommand @("set", "OPENAI_API_KEY=secret-value", "WECHAT_SERVICE_ENCODING_AES_KEY=wechat-aes-key-value", "OPENAI_API_MODE=auto")
    $values = Read-ConfigValues
    Assert-True ($values["OPENAI_API_KEY"] -eq "secret-value") "API key config was not written"
    Assert-True ($values["OPENAI_API_MODE"] -eq "auto") "provider connection config was not written"
    $masked = (Show-Config @("OPENAI_API_KEY")) -join "`n"
    Assert-True (-not $masked.Contains("secret-value")) "config show leaked a secret"
    $maskedWechatKey = (Show-Config @("WECHAT_SERVICE_ENCODING_AES_KEY")) -join "`n"
    Assert-True (-not $maskedWechatKey.Contains("wechat-aes-key-value")) "config show leaked EncodingAESKey"

    $qbotScript = Join-Path $repoDir "scripts\qbot.ps1"
    & powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File $qbotScript config set "BINDING_TEST=ok"
    if ($LASTEXITCODE -ne 0) {
        throw "qbot.ps1 command-line argument binding failed"
    }
    $values = Read-ConfigValues
    Assert-True ($values["BINDING_TEST"] -eq "ok") "command-line config value was not written"

    & powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File $qbotScript config bot --app-id 123 --app-secret secret --enable
    if ($LASTEXITCODE -ne 0) {
        throw "qbot.ps1 config bot argument binding failed"
    }
    $values = Read-ConfigValues
    Assert-True ($values["QQ_BOT_APP_ID"] -eq "123") "config bot did not write app id"
    Assert-True ($values["QQ_CHANNEL_ENABLED"] -eq "true") "config bot did not enable the channel"

    Set-Content -LiteralPath (Join-Path $appDir "config\.env") -Value @(
        "LLM_SERVER_PORT=9988",
        "WEB_CONSOLE_ENABLED=true"
    ) -Encoding ASCII
    $consoleHint = (Write-ConsoleConfigHint) -join "`n"
    Assert-True ($consoleHint.Contains("http://127.0.0.1:9988/console/")) "qbot ignored custom console port"
    Set-Content -LiteralPath (Join-Path $appDir "config\.env") -Value "WEB_CONSOLE_ENABLED=false" -Encoding ASCII
    $consoleHint = (Write-ConsoleConfigHint) -join "`n"
    Assert-True ([string]::IsNullOrEmpty($consoleHint)) "qbot printed a hint while console was disabled"

    $archive = Join-Path $testRoot "fixture.zip"
    $checksum = "${archive}.sha256"
    Set-Content -LiteralPath $archive -Value "fixture" -Encoding ASCII
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath $checksum -Value "$hash  fixture.zip" -Encoding ASCII
    Test-ReleaseChecksum -Archive $archive -ChecksumFile $checksum

    Write-Output "PowerShell qbot regression tests passed"
} finally {
    $env:QBOT_APP_DIR = $oldAppDir
    $env:LLM_SERVER_URL = $oldServerUrl
    $env:LLM_SERVER_HOST = $oldServerHost
    $env:LLM_SERVER_PORT = $oldServerPort
    $env:WEB_CONSOLE_ENABLED = $oldConsoleEnabled
    Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}

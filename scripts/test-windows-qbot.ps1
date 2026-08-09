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
$oldInstallWeb = $env:QBOT_INSTALL_WEB_CONSOLE
$oldGitHubProxy = $env:QBOT_GITHUB_PROXY
$oldGitHubProxies = $env:QBOT_GITHUB_PROXIES

# 执行前后用户/系统环境变量与 git 全局配置必须保持不变（严禁写 User/Machine 作用域）。
$userProxyBefore = [Environment]::GetEnvironmentVariable("QBOT_GITHUB_PROXY", "User")
$userProxiesBefore = [Environment]::GetEnvironmentVariable("QBOT_GITHUB_PROXIES", "User")
$machineProxyBefore = [Environment]::GetEnvironmentVariable("QBOT_GITHUB_PROXY", "Machine")
$machineProxiesBefore = [Environment]::GetEnvironmentVariable("QBOT_GITHUB_PROXIES", "Machine")
$gitConfigPath = Join-Path $HOME ".gitconfig"
$gitConfigBefore = if (Test-Path -LiteralPath $gitConfigPath -PathType Leaf) {
    (Get-Content -LiteralPath $gitConfigPath -Raw)
} else {
    $null
}

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
    $env:QBOT_GITHUB_PROXY = $null
    $env:QBOT_GITHUB_PROXIES = $null
    . (Join-Path $repoDir "scripts\qbot.ps1")
    . (Join-Path $repoDir "scripts\lib\agent-config.ps1")
    $agentConfigMigrationMarker = Join-Path $appDir "config\.agent-config-v0.20.2"
    Remove-Item -LiteralPath $agentConfigMigrationMarker -Force -ErrorAction SilentlyContinue

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
    Set-Content -LiteralPath (Join-Path $releaseDir "config\agent.example.toml") -Value "release-agent" -Encoding ASCII

    $agentTestDir = Join-Path $testRoot "agent-migration"
    New-Item -ItemType Directory -Path $agentTestDir -Force | Out-Null
    $agentTemplate = Join-Path $agentTestDir "template.toml"
    Set-Content -LiteralPath $agentTemplate -Value "new-release-template" -Encoding ASCII

    $webSearchAgent = Join-Path $agentTestDir "web-search.toml"
    $utf8NoBom = New-Object Text.UTF8Encoding($false)
    # 用 UTF-8 无 BOM 写入含中文注释的旧格式，覆盖 Windows PowerShell 5.1 默认 ANSI 读文件路径。
    $legacyWebSearchLines = @(
        'version = 1',
        '# 旧版私聊联网搜索路由',
        '',
        '[search_routes.private_search]',
        'model = "gpt-search"'
    )
    [IO.File]::WriteAllLines($webSearchAgent, $legacyWebSearchLines, $utf8NoBom)
    $webSearchOutput = (Migrate-AgentWebSearchConfig -ConfigFile $webSearchAgent) -join "`n"
    $migratedWebSearch = [IO.File]::ReadAllText($webSearchAgent, $utf8NoBom)
    $webSearchBackup = [IO.File]::ReadAllText("${webSearchAgent}.old", $utf8NoBom)
    Assert-True ($migratedWebSearch.Contains('[tools.web_search]')) "web-search defaults were not added"
    Assert-True ($migratedWebSearch.Contains('[tools.web_search.routes.private_search]')) "legacy web-search route was not migrated"
    Assert-True (-not $migratedWebSearch.Contains('[search_routes.private_search]')) "legacy web-search route remained"
    Assert-True ($migratedWebSearch.Contains('# 旧版私聊联网搜索路由')) "web-search migration corrupted UTF-8 comments"
    Assert-True ($webSearchBackup.Contains('[search_routes.private_search]')) "web-search migration backup lost the legacy route"
    Assert-True ($webSearchBackup.Contains('# 旧版私聊联网搜索路由')) "web-search migration backup corrupted UTF-8 comments"
    Assert-True ($webSearchOutput.Contains("backup: ${webSearchAgent}.old")) "web-search migration did not report its backup"
    Migrate-AgentWebSearchConfig -ConfigFile $webSearchAgent
    Assert-True (-not (Test-Path -LiteralPath "${webSearchAgent}.old.1")) "idempotent web-search migration created another backup"

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

    # 从 Install-OrUpdate 真实入口触发模板替换失败，确认异常会中断后续安装与完成标记。
    $failedMarker = $agentConfigMigrationMarker
    Remove-Item -LiteralPath $failedMarker -Force -ErrorAction SilentlyContinue
    Set-Content -LiteralPath (Join-Path $appDir "VERSION") -Value "v0.20.1" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $appDir "config\agent.toml") -Value "must-survive-full-chain" -Encoding ASCII
    function Resolve-Version { return "v0.20.2" }
    function Save-ReleaseChain {
        param([string]$Version, [string]$ArchiveName, [string]$ArchivePath, [string]$ChecksumPath)
        return $true
    }
    function Expand-Archive {
        param([string]$LiteralPath, [string]$DestinationPath, [switch]$Force)
        $packageDir = Join-Path $DestinationPath "qq-maid-bot-v0.20.2-windows-x86_64"
        New-Item -ItemType Directory -Path (Join-Path $packageDir "config") -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $packageDir "config\agent.example.toml") -Value "new-release-template" -Encoding ASCII
    }
    $script:FullChainMoveCalls = 0
    function Move-Item {
        param([string]$LiteralPath, [string]$Destination)
        $script:FullChainMoveCalls++
        if ($script:FullChainMoveCalls -eq 2) {
            throw "simulated full-chain template activation failure"
        }
        Microsoft.PowerShell.Management\Move-Item -LiteralPath $LiteralPath -Destination $Destination
    }
    $failedReplacementError = $null
    try {
        Install-OrUpdate -Mode "update" -RequestedVersion "v0.20.2"
    } catch {
        $failedReplacementError = $_.Exception.Message
    } finally {
        Remove-Item Function:\Resolve-Version -ErrorAction SilentlyContinue
        Remove-Item Function:\Save-ReleaseChain -ErrorAction SilentlyContinue
        Remove-Item Function:\Expand-Archive -ErrorAction SilentlyContinue
        Remove-Item Function:\Move-Item -ErrorAction SilentlyContinue
        . (Join-Path $repoDir "scripts\qbot.ps1")
    }
    Assert-True ($null -ne $failedReplacementError) "failed agent migration did not return an error"
    Assert-True ((Get-Content -LiteralPath (Join-Path $appDir "config\agent.toml") -Raw).Contains("must-survive-full-chain")) "full-chain replacement failure did not restore agent.toml"
    Assert-True ((Get-Content -LiteralPath (Join-Path $appDir "VERSION") -Raw).Contains("v0.20.1")) "failed migration continued into release installation"
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
    Set-InstallWebConsoleChoice -RequestedWeb "false" -ConfigExisted $false
    $webChoiceValues = Read-ConfigValues
    Assert-True ($webChoiceValues["WEB_CONSOLE_ENABLED"] -eq "false") "install Web opt-out was not persisted"
    Set-InstallWebConsoleChoice -RequestedWeb "true" -ConfigExisted $true
    $webChoiceValues = Read-ConfigValues
    Assert-True ($webChoiceValues["WEB_CONSOLE_ENABLED"] -eq "true") "explicit reinstall Web choice was not persisted"
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
    Assert-True ((Get-Content -LiteralPath (Join-Path $appDir "config\agent.toml") -Raw).Contains("custom-agent")) "local agent.toml was overwritten"
    Assert-True ((Get-Content -LiteralPath (Join-Path $appDir "config\agent.example.toml") -Raw).Contains("release-agent")) "agent.example.toml was not installed"
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

    # —— GitHub 下载候选与失败回退回归 ——
    # 用 mock 的 Invoke-WebRequest 模拟网络：请求 URL 写入日志；按前缀规则返回失败、
    # 空文件、损坏 ZIP 或错误校验和；正常请求从 mock-server 目录取 fixture 文件。
    $mockServerDir = Join-Path $testRoot "mock-server"
    $mockPackageDir = Join-Path $mockServerDir "qq-maid-bot-v9.9.9-windows-x86_64"
    New-Item -ItemType Directory -Path (Join-Path $mockPackageDir "config") -Force | Out-Null
    foreach ($mockFile in @("qq-maid-bot.exe", "README.md", "VERSION")) {
        Set-Content -LiteralPath (Join-Path $mockPackageDir $mockFile) -Value "release" -Encoding ASCII
    }
    Set-Content -LiteralPath (Join-Path $mockPackageDir "config\.env.example") -Value "EXAMPLE=1" -Encoding ASCII
    $mockZip = Join-Path $mockServerDir "qq-maid-bot-v9.9.9-windows-x86_64.zip"
    # 用 .NET ZipFile 打包并显式 includeBaseDirectory，确保 ZIP 顶层是包目录本身
    # （与真实 Release 的 zip -rq 布局一致；Compress-Archive 与默认 2 参 CreateFromDirectory
    # 都不含顶层目录名，不能用于 mock）。
    [IO.Compression.ZipFile]::CreateFromDirectory($mockPackageDir, $mockZip, [IO.Compression.CompressionLevel]::Optimal, $true)
    $mockHash = (Get-FileHash -LiteralPath $mockZip -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "${mockZip}.sha256" -Value "$mockHash  qq-maid-bot-v9.9.9-windows-x86_64.zip" -Encoding ASCII
    # 结构损坏产物：ZIP 容器有效但缺少预期的顶层包目录，用于验证深度校验回退。
    $mockStructureDir = Join-Path $testRoot "structure-broken"
    New-Item -ItemType Directory -Path $mockStructureDir -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $mockStructureDir "README.md") -Value "broken" -Encoding ASCII
    $mockStructureZip = Join-Path $mockServerDir "structure-broken.zip"
    Compress-Archive -Path (Join-Path $mockStructureDir "*") -DestinationPath $mockStructureZip -Force
    $mockStructureHash = (Get-FileHash -LiteralPath $mockStructureZip -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath "${mockStructureZip}.sha256" -Value "$mockStructureHash  qq-maid-bot-v9.9.9-windows-x86_64.zip" -Encoding ASCII

    $script:MockRequestLog = New-Object System.Collections.Generic.List[string]
    $script:MockFailPatterns = New-Object System.Collections.Generic.List[string]
    $script:MockEmptyPatterns = New-Object System.Collections.Generic.List[string]
    $script:MockCorruptPatterns = New-Object System.Collections.Generic.List[string]
    $script:MockStructurePatterns = New-Object System.Collections.Generic.List[string]
    $script:MockWrongHashPatterns = New-Object System.Collections.Generic.List[string]

    function Invoke-WebRequest {
        param([string]$Uri, [string]$OutFile, [switch]$UseBasicParsing, [int]$TimeoutSec, [string]$ErrorAction)
        $script:MockRequestLog.Add($Uri) | Out-Null
        foreach ($pattern in $script:MockFailPatterns) {
            if ($Uri.StartsWith($pattern, [StringComparison]::OrdinalIgnoreCase)) {
                throw "simulated HTTP/network failure for $Uri"
            }
        }
        foreach ($pattern in $script:MockEmptyPatterns) {
            if ($Uri.StartsWith($pattern, [StringComparison]::OrdinalIgnoreCase)) {
                Set-Content -LiteralPath $OutFile -Value "" -Encoding ASCII
                return $null
            }
        }
        foreach ($pattern in $script:MockCorruptPatterns) {
            if ($Uri.StartsWith($pattern, [StringComparison]::OrdinalIgnoreCase)) {
                Set-Content -LiteralPath $OutFile -Value "not-a-zip" -Encoding ASCII
                return $null
            }
        }
        foreach ($pattern in $script:MockStructurePatterns) {
            if ($Uri.StartsWith($pattern, [StringComparison]::OrdinalIgnoreCase)) {
                if ($Uri.EndsWith(".sha256")) {
                    Copy-Item -LiteralPath "${mockStructureZip}.sha256" -Destination $OutFile -Force
                } else {
                    Copy-Item -LiteralPath $mockStructureZip -Destination $OutFile -Force
                }
                return $null
            }
        }
        foreach ($pattern in $script:MockWrongHashPatterns) {
            if ($Uri.StartsWith($pattern, [StringComparison]::OrdinalIgnoreCase) -and $Uri.EndsWith(".sha256")) {
                Set-Content -LiteralPath $OutFile -Value ("0" * 64) -Encoding ASCII
                return $null
            }
        }
        $fileName = [IO.Path]::GetFileName($Uri)
        Write-Host ("mock served: " + $Uri)
        Copy-Item -LiteralPath (Join-Path $mockServerDir $fileName) -Destination $OutFile -Force
        return $null
    }

    function Invoke-TestDownloadChain {
        # 跑一次真实链路，返回干净的布尔结果，并把诊断信息打到宿主输出（不进成功流）。
        param([string]$Version, [string]$ArchiveName, [string]$ArchivePath, [string]$ChecksumPath)
        Write-Host ("mock fail patterns: " + ($script:MockFailPatterns -join " | "))
        Write-Host ("mock request log: " + ($script:MockRequestLog -join " | "))
        $result = Save-ReleaseChain -Version $Version -ArchiveName $ArchiveName -ArchivePath $ArchivePath -ChecksumPath $ChecksumPath
        Write-Host ("chain result: " + $result)
        Write-Host ("mock request log after: " + ($script:MockRequestLog -join " | "))
        return $result
    }

    $officialPrefix = "https://github.com/$($script:RepoSlug)"
    $archiveName = "qq-maid-bot-v9.9.9-windows-x86_64.zip"
    $downloadDir = Join-Path $testRoot "download"
    New-Item -ItemType Directory -Path $downloadDir -Force | Out-Null
    $archivePath = Join-Path $downloadDir $archiveName

    # 1) 未配置代理时只访问 GitHub 官方源，且下载成功。
    $script:MockRequestLog.Clear()
    $script:MockFailPatterns.Clear()
    $script:MockEmptyPatterns.Clear()
    $script:MockCorruptPatterns.Clear()
    $script:MockWrongHashPatterns.Clear()
    $env:QBOT_GITHUB_PROXY = $null
    $env:QBOT_GITHUB_PROXIES = $null
    $prefixes = Get-GitHubProxyPrefixes
    Assert-True ($prefixes.Count -eq 1 -and $prefixes[0] -eq "") "no proxy should yield only the official source"
    $ok = Invoke-TestDownloadChain -Version "v9.9.9" -ArchiveName $archiveName -ArchivePath $archivePath -ChecksumPath "${archivePath}.sha256"
    Assert-True $ok "official-only download should succeed"
    foreach ($requestUrl in $script:MockRequestLog) {
        Assert-True ($requestUrl.StartsWith("$officialPrefix/releases/download/v9.9.9/", [StringComparison]::OrdinalIgnoreCase)) "no-proxy mode requested a non-official URL: $requestUrl"
    }

    # 2) 官方源失败时回退到单个代理源。
    $script:MockRequestLog.Clear()
    $script:MockFailPatterns.Add($officialPrefix)
    $env:QBOT_GITHUB_PROXY = "https://proxy-a.example.com/"
    $env:QBOT_GITHUB_PROXIES = $null
    $prefixes = Get-GitHubProxyPrefixes
    Assert-True ($prefixes.Count -eq 2 -and $prefixes[1] -eq "https://proxy-a.example.com") "QBOT_GITHUB_PROXY did not add a proxy candidate"
    $ok = Invoke-TestDownloadChain -Version "v9.9.9" -ArchiveName $archiveName -ArchivePath $archivePath -ChecksumPath "${archivePath}.sha256"
    Assert-True $ok "official failure should fall back to the single proxy"
    $proxyRequested = $false
    foreach ($requestUrl in $script:MockRequestLog) {
        if ($requestUrl.StartsWith("https://proxy-a.example.com/", [StringComparison]::OrdinalIgnoreCase)) {
            $proxyRequested = $true
            break
        }
    }
    Assert-True $proxyRequested "proxy URL was not requested after official failure"

    # 3) 第一个代理失败时继续尝试后续代理。
    $script:MockRequestLog.Clear()
    $script:MockFailPatterns.Clear()
    $script:MockFailPatterns.Add($officialPrefix)
    $script:MockFailPatterns.Add("https://proxy-bad.example.com")
    $env:QBOT_GITHUB_PROXY = "https://proxy-bad.example.com"
    $env:QBOT_GITHUB_PROXIES = "https://proxy-good.example.com"
    $ok = Invoke-TestDownloadChain -Version "v9.9.9" -ArchiveName $archiveName -ArchivePath $archivePath -ChecksumPath "${archivePath}.sha256"
    Assert-True $ok "later proxy should succeed after the first proxy failed"
    $badIndex = -1
    $goodIndex = -1
    for ($i = 0; $i -lt $script:MockRequestLog.Count; $i++) {
        if ($script:MockRequestLog[$i].StartsWith("https://proxy-bad.example.com/")) { $badIndex = $i; break }
    }
    for ($i = 0; $i -lt $script:MockRequestLog.Count; $i++) {
        if ($script:MockRequestLog[$i].StartsWith("https://proxy-good.example.com/")) { $goodIndex = $i; break }
    }
    Assert-True ($badIndex -ge 0 -and $goodIndex -gt $badIndex) "later proxy was not tried after the first proxy failed"

    # 4) HTTP 成功但文件为空 / ZIP 损坏 / checksum 无效时继续回退。
    $script:MockRequestLog.Clear()
    $script:MockFailPatterns.Clear()
    $script:MockEmptyPatterns.Add($officialPrefix)
    $env:QBOT_GITHUB_PROXY = "https://proxy-a.example.com"
    $env:QBOT_GITHUB_PROXIES = $null
    $ok = Invoke-TestDownloadChain -Version "v9.9.9" -ArchiveName $archiveName -ArchivePath $archivePath -ChecksumPath "${archivePath}.sha256"
    Assert-True $ok "empty official file should fall back to proxy"

    $script:MockEmptyPatterns.Clear()
    $script:MockCorruptPatterns.Add($officialPrefix)
    $ok = Invoke-TestDownloadChain -Version "v9.9.9" -ArchiveName $archiveName -ArchivePath $archivePath -ChecksumPath "${archivePath}.sha256"
    Assert-True $ok "corrupt official zip should fall back to proxy"

    $script:MockCorruptPatterns.Clear()
    $script:MockStructurePatterns.Add($officialPrefix)
    $ok = Invoke-TestDownloadChain -Version "v9.9.9" -ArchiveName $archiveName -ArchivePath $archivePath -ChecksumPath "${archivePath}.sha256"
    Assert-True $ok "structure-broken official zip should fall back to proxy"
    $script:MockStructurePatterns.Clear()

    $script:MockCorruptPatterns.Clear()
    $script:MockWrongHashPatterns.Add($officialPrefix)
    $ok = Invoke-TestDownloadChain -Version "v9.9.9" -ArchiveName $archiveName -ArchivePath $archivePath -ChecksumPath "${archivePath}.sha256"
    Assert-True $ok "invalid official checksum should fall back to proxy"

    # 5) 重复代理（含尾部斜杠差异）不产生重复请求。
    $script:MockRequestLog.Clear()
    $script:MockWrongHashPatterns.Clear()
    $script:MockFailPatterns.Add($officialPrefix)
    $env:QBOT_GITHUB_PROXY = "https://proxy-a.example.com/"
    $env:QBOT_GITHUB_PROXIES = "https://proxy-a.example.com https://proxy-a.example.com/"
    $ok = Invoke-TestDownloadChain -Version "v9.9.9" -ArchiveName $archiveName -ArchivePath $archivePath -ChecksumPath "${archivePath}.sha256"
    Assert-True $ok "deduplicated proxy download should succeed"
    $archiveUrl = "https://proxy-a.example.com/$officialPrefix/releases/download/v9.9.9/$archiveName"
    $archiveCount = @($script:MockRequestLog | Where-Object { $_ -eq $archiveUrl }).Count
    Assert-True ($archiveCount -eq 1) "duplicate proxies produced duplicate requests"

    # 6) 所有来源失败时返回非零，且不覆盖任何程序文件。
    $script:MockRequestLog.Clear()
    $script:MockFailPatterns.Clear()
    $script:MockFailPatterns.Add($officialPrefix)
    $env:QBOT_GITHUB_PROXY = $null
    $env:QBOT_GITHUB_PROXIES = $null
    $ok = Invoke-TestDownloadChain -Version "v9.9.9" -ArchiveName $archiveName -ArchivePath $archivePath -ChecksumPath "${archivePath}.sha256"
    Assert-True (-not $ok) "all-sources-failed chain should return false"
    $installFailApp = Join-Path $testRoot "install-fail"
    New-Item -ItemType Directory -Path (Join-Path $installFailApp "config") -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $installFailApp "VERSION") -Value "v0.0.0-old" -Encoding ASCII
    $oldScriptAppDir = $script:AppDir
    $script:AppDir = $installFailApp
    $installFailError = $null
    try {
        Install-OrUpdate -Mode "install" -RequestedVersion "v9.9.9"
    } catch {
        $installFailError = $_.Exception.Message
    } finally {
        $script:AppDir = $oldScriptAppDir
    }
    Assert-True ($null -ne $installFailError) "all-sources-failed install did not return an error"
    Assert-True ((Get-Content -LiteralPath (Join-Path $installFailApp "VERSION") -Raw).Contains("v0.0.0-old")) "failed install overwrote program files"
    Assert-True ($installFailError.Contains("QBOT_GITHUB_PROXY")) "all-sources-failed error lacks proxy guidance"
    Remove-Item Function:\Invoke-WebRequest -ErrorAction SilentlyContinue

    # 7) 候选列表规范化与去重（与 Linux 端语义一致：官方 → 单代理 → 多代理，按序去重）。
    $env:QBOT_GITHUB_PROXY = "https://proxy-a.example.com/"
    $env:QBOT_GITHUB_PROXIES = "https://proxy-b.example.com https://proxy-a.example.com bad-addr http:// http:///path https://?query"
    $prefixes = Get-GitHubProxyPrefixes
    Assert-True ($prefixes.Count -eq 3) "expected official plus two deduplicated proxies"
    Assert-True ($prefixes[0] -eq "" -and $prefixes[1] -eq "https://proxy-a.example.com" -and $prefixes[2] -eq "https://proxy-b.example.com") "proxy prefix order/normalization mismatch"
    $env:QBOT_GITHUB_PROXY = $null
    $env:QBOT_GITHUB_PROXIES = $null

    # —— qbot.cmd 参数透传与退出码 ——
    $cmdDir = Join-Path $testRoot "cmd with spaces"
    New-Item -ItemType Directory -Path $cmdDir -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $repoDir "scripts\qbot.ps1") -Destination (Join-Path $cmdDir "qbot.ps1")
    Copy-Item -LiteralPath (Join-Path $repoDir "scripts\qbot.cmd") -Destination (Join-Path $cmdDir "qbot.cmd")
    & cmd.exe /d /c "`"$cmdDir\qbot.cmd`" config set CMD_BINDING_TEST=ok"
    if ($LASTEXITCODE -ne 0) {
        throw "qbot.cmd argument pass-through failed"
    }
    $values = Read-ConfigValues
    Assert-True ($values["CMD_BINDING_TEST"] -eq "ok") "qbot.cmd did not forward config arguments"
    & cmd.exe /d /c "`"$cmdDir\qbot.cmd`" not-a-command"
    if ($LASTEXITCODE -eq 0) {
        throw "qbot.cmd swallowed the PowerShell non-zero exit code"
    }
    # 上一条 cmd.exe 故意以非零退出；清掉 LASTEXITCODE，避免 powershell -Command
    # 把“最后一条原生命令”的退出码当成整个测试进程的退出码，误报失败。
    $LASTEXITCODE = 0

    # 执行前后用户/系统环境变量与 git 全局配置必须保持不变。
    $userProxyAfter = [Environment]::GetEnvironmentVariable("QBOT_GITHUB_PROXY", "User")
    $userProxiesAfter = [Environment]::GetEnvironmentVariable("QBOT_GITHUB_PROXIES", "User")
    $machineProxyAfter = [Environment]::GetEnvironmentVariable("QBOT_GITHUB_PROXY", "Machine")
    $machineProxiesAfter = [Environment]::GetEnvironmentVariable("QBOT_GITHUB_PROXIES", "Machine")
    Assert-True ($userProxyAfter -eq $userProxyBefore -and $userProxiesAfter -eq $userProxiesBefore) "User-scope proxy environment changed during the test"
    Assert-True ($machineProxyAfter -eq $machineProxyBefore -and $machineProxiesAfter -eq $machineProxiesBefore) "Machine-scope proxy environment changed during the test"
    $gitConfigAfter = if (Test-Path -LiteralPath $gitConfigPath -PathType Leaf) {
        (Get-Content -LiteralPath $gitConfigPath -Raw)
    } else {
        $null
    }
    Assert-True ($gitConfigAfter -eq $gitConfigBefore) "git global config changed during the test"

    Write-Output "PowerShell qbot regression tests passed"
    # 测试中曾以非零退出运行 cmd.exe（校验 qbot.cmd 退出码透传），
    # powershell -Command 会沿用最后一条原生命令/LASTEXITCODE 作为进程退出码；
    # 全部断言通过后显式 exit 0，避免误报失败（断言失败会先抛出，不会走到这里）。
    exit 0
} finally {
    $env:QBOT_APP_DIR = $oldAppDir
    $env:LLM_SERVER_URL = $oldServerUrl
    $env:LLM_SERVER_HOST = $oldServerHost
    $env:LLM_SERVER_PORT = $oldServerPort
    $env:WEB_CONSOLE_ENABLED = $oldConsoleEnabled
    $env:QBOT_INSTALL_WEB_CONSOLE = $oldInstallWeb
    $env:QBOT_GITHUB_PROXY = $oldGitHubProxy
    $env:QBOT_GITHUB_PROXIES = $oldGitHubProxies
    Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}

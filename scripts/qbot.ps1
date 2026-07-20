[CmdletBinding(PositionalBinding = $false)]
param(
    [Parameter(Position = 0)][string]$Command = "help",
    [Parameter(Position = 1, ValueFromRemainingArguments = $true)][string[]]$CommandArgs
)

$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

function Get-EnvironmentValue {
    param([string]$Name, [string]$DefaultValue)
    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        return $DefaultValue
    }
    return $value
}

function Resolve-WindowsOperatingSystemArchitecture {
    param(
        [string]$RuntimeArchitecture,
        [string]$ProcessorArchitecture,
        [string]$ProcessorArchitectureW6432
    )
    # WOW64 exposes the native OS architecture through PROCESSOR_ARCHITEW6432.
    foreach ($candidate in @($RuntimeArchitecture, $ProcessorArchitectureW6432, $ProcessorArchitecture)) {
        if (-not [string]::IsNullOrWhiteSpace($candidate)) {
            $normalized = $candidate.Trim()
            return $normalized.ToUpperInvariant()
        }
    }
    return "UNKNOWN"
}

function Get-WindowsOperatingSystemArchitecture {
    $runtimeArchitecture = $null
    try {
        $runtimeInformationType = "System.Runtime.InteropServices.RuntimeInformation" -as [type]
        if ($null -ne $runtimeInformationType) {
            $property = $runtimeInformationType.GetProperty("OSArchitecture")
            if ($null -ne $property) {
                $runtimeArchitecture = [string]($property.GetValue($null, $null))
            }
        }
    } catch {
        $runtimeArchitecture = $null
    }

    return Resolve-WindowsOperatingSystemArchitecture `
        -RuntimeArchitecture $runtimeArchitecture `
        -ProcessorArchitecture $env:PROCESSOR_ARCHITECTURE `
        -ProcessorArchitectureW6432 $env:PROCESSOR_ARCHITEW6432
}

function Test-SupportedWindowsArchitecture {
    param([string]$OperatingSystemArchitecture)
    $normalized = Resolve-WindowsOperatingSystemArchitecture $OperatingSystemArchitecture "" ""
    return $normalized -in @("AMD64", "X64", "X86_64")
}

function Assert-SupportedWindowsArchitecture {
    param([string]$OperatingSystemArchitecture)
    if (Test-SupportedWindowsArchitecture $OperatingSystemArchitecture) {
        return
    }

    throw "Only a Windows x86_64 Release is currently available.`r`nARM64 users can install the Linux Release through WSL. Detected OS architecture: $OperatingSystemArchitecture"
}

$script:AppDir = [IO.Path]::GetFullPath((Get-EnvironmentValue "QBOT_APP_DIR" (Join-Path $HOME "qq-maid-bot")))
$script:InstallerPath = [IO.Path]::GetFullPath($MyInvocation.MyCommand.Path)
$script:RepoSlug = Get-EnvironmentValue "QBOT_REPO_SLUG" "kuliantnt/qq-maid-bot"
$script:ReleasesUrl = "https://github.com/$($script:RepoSlug)/releases"
$script:LatestApiUrl = "https://api.github.com/repos/$($script:RepoSlug)/releases/latest"
$script:ObsoleteEnvKeys = @(
    "LLM_PROVIDER", "OPENAI_MODEL", "LLM_MODEL", "PRIVATE_LLM_MODEL", "GROUP_LLM_MODEL",
    "OPENAI_SEARCH_MODEL", "PRIVATE_OPENAI_SEARCH_MODEL", "GROUP_OPENAI_SEARCH_MODEL",
    "TITLE_MODEL", "MEMORY_MODEL", "COMPACT_MODEL", "TRANSLATION_MODEL",
    "DEEPSEEK_MODEL", "BIGMODEL_MODEL", "GEMINI_MODEL", "LLM_MAX_OUTPUT_TOKENS",
    "TOOL_CALLING_ENABLED", "TOOL_CALLING_GROUP_ENABLED", "TOOL_CALLING_MAX_ROUNDS",
    "TODO_MODEL", "MEMBER_ID_MAPPING_FILE"
)
$script:AgentConfigMigrationVersion = [Version]"0.20.2"
$script:AgentConfigMigrationMarkerName = ".agent-config-v0.20.2"

function Show-QbotUsage {
    @"
Usage: qbot.cmd <command>
       powershell.exe -ExecutionPolicy Bypass -File .\qbot.ps1 <command>

Commands:
  install [version]       Download and install the Windows x86_64 Release
  update [version]        Update while preserving config and runtime data
  version                 Show installed and latest versions
  start|stop|restart      Manage the installed bot
  status|logs             Show status or follow logs
  health|console          Check the local service
  config path             Create and print config\.env
  config show [KEY...]    Show configuration with secrets masked
  config get KEY          Print one configuration value
  config set KEY=VALUE    Set one or more configuration values
  config bot <options>    Configure QQ Bot values
  config ai <options>     Configure AI provider values

Environment overrides:
  QBOT_APP_DIR            Install directory (default: %USERPROFILE%\qq-maid-bot)
  QBOT_REPO_SLUG          GitHub repository (default: kuliantnt/qq-maid-bot)
  QBOT_GITHUB_PROXY       Optional trusted download URL prefix
"@
}

function Normalize-Version {
    param([string]$Version)
    if ([string]::IsNullOrWhiteSpace($Version) -or $Version -eq "latest") {
        return "latest"
    }
    if ($Version.StartsWith("v")) {
        return $Version
    }
    return "v$Version"
}

function ConvertTo-AgentConfigVersion {
    param([AllowEmptyString()][string]$Version)
    if ([string]::IsNullOrWhiteSpace($Version)) {
        return $null
    }
    $normalized = $Version.Trim()
    if ($normalized.StartsWith("v")) {
        $normalized = $normalized.Substring(1)
    }
    $normalized = ($normalized -split '[-+]', 2)[0]
    $parsed = $null
    if (-not [Version]::TryParse($normalized, [ref]$parsed)) {
        return $null
    }
    return $parsed
}

function Test-AgentConfigResetRequired {
    param(
        [AllowEmptyString()][string]$CurrentVersion,
        [Parameter(Mandatory = $true)][string]$TargetVersion,
        [AllowEmptyString()][string]$MarkerFile
    )
    if (-not [string]::IsNullOrWhiteSpace($MarkerFile) -and (Test-Path -LiteralPath $MarkerFile)) {
        return $false
    }
    $target = ConvertTo-AgentConfigVersion $TargetVersion
    if ($null -eq $target -or $target -lt $script:AgentConfigMigrationVersion) {
        return $false
    }
    $current = ConvertTo-AgentConfigVersion $CurrentVersion
    return $null -eq $current -or $current -lt $script:AgentConfigMigrationVersion
}

function Complete-AgentConfigMigration {
    param(
        [AllowEmptyString()][string]$CurrentVersion,
        [Parameter(Mandatory = $true)][string]$TargetVersion
    )
    $current = ConvertTo-AgentConfigVersion $CurrentVersion
    $target = ConvertTo-AgentConfigVersion $TargetVersion
    if (($null -eq $current -or $current -lt $script:AgentConfigMigrationVersion) -and
        ($null -eq $target -or $target -lt $script:AgentConfigMigrationVersion)) {
        return
    }
    $marker = Join-Path $script:AppDir "config\$($script:AgentConfigMigrationMarkerName)"
    New-Item -ItemType File -Path $marker -Force | Out-Null
}

function Get-LatestVersion {
    $headers = @{ "User-Agent" = "qq-maid-bot-windows-installer" }
    $release = Invoke-RestMethod -Uri $script:LatestApiUrl -Headers $headers -UseBasicParsing
    if ($null -eq $release -or [string]::IsNullOrWhiteSpace([string]$release.tag_name)) {
        throw "unable to resolve the latest Release version"
    }
    return [string]$release.tag_name
}

function Resolve-Version {
    param([string]$RequestedVersion)
    $normalized = Normalize-Version $RequestedVersion
    if ($normalized -eq "latest") {
        return Get-LatestVersion
    }
    return $normalized
}

function Get-DownloadUrl {
    param([string]$RawUrl)
    $prefix = [Environment]::GetEnvironmentVariable("QBOT_GITHUB_PROXY")
    if ([string]::IsNullOrWhiteSpace($prefix)) {
        return $RawUrl
    }
    return "$($prefix.TrimEnd('/'))/$RawUrl"
}

function Save-ReleaseFile {
    param([string]$Url, [string]$Destination)
    $downloadUrl = Get-DownloadUrl $Url
    Invoke-WebRequest -Uri $downloadUrl -OutFile $Destination -UseBasicParsing
    if (-not (Test-Path -LiteralPath $Destination -PathType Leaf) -or
        (Get-Item -LiteralPath $Destination).Length -eq 0) {
        throw "download returned an empty file: $Url"
    }
}

function Test-ReleaseChecksum {
    param([string]$Archive, [string]$ChecksumFile)
    $checksumText = (Get-Content -LiteralPath $ChecksumFile -Raw).Trim()
    $expected = ($checksumText -split '\s+')[0]
    if ($expected -notmatch '^[0-9a-fA-F]{64}$') {
        throw "invalid SHA-256 file: $ChecksumFile"
    }
    $actual = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash
    if (-not $actual.Equals($expected, [StringComparison]::OrdinalIgnoreCase)) {
        throw "SHA-256 mismatch for $(Split-Path -Leaf $Archive)"
    }
}

function Get-LocalVersion {
    $versionFile = Join-Path $script:AppDir "VERSION"
    if (-not (Test-Path -LiteralPath $versionFile -PathType Leaf)) {
        return $null
    }
    return (Get-Content -LiteralPath $versionFile -Raw).Trim()
}

function Test-InstalledBotRunning {
    $pidFile = Join-Path $script:AppDir "run\qq-maid-bot.pid"
    $binary = Join-Path $script:AppDir "qq-maid-bot.exe"
    if (-not (Test-Path -LiteralPath $pidFile -PathType Leaf)) {
        return $false
    }
    $pidValue = 0
    if (-not [int]::TryParse((Get-Content -LiteralPath $pidFile -Raw).Trim(), [ref]$pidValue)) {
        return $false
    }
    $process = Get-Process -Id $pidValue -ErrorAction SilentlyContinue
    if ($null -eq $process) {
        return $false
    }
    try {
        return ([IO.Path]::GetFullPath($process.Path)).Equals(
            [IO.Path]::GetFullPath($binary),
            [StringComparison]::OrdinalIgnoreCase
        )
    } catch {
        return $false
    }
}

function Invoke-BotControl {
    param([string]$ControlCommand)
    $controller = Join-Path $script:AppDir "botctl.ps1"
    if (-not (Test-Path -LiteralPath $controller -PathType Leaf)) {
        throw "botctl.ps1 not found in $($script:AppDir); run qbot install first"
    }
    $oldRuntimeDir = $env:QQ_MAID_RUNTIME_DIR
    try {
        $env:QQ_MAID_RUNTIME_DIR = $script:AppDir
        & $controller $ControlCommand
    } finally {
        $env:QQ_MAID_RUNTIME_DIR = $oldRuntimeDir
    }
}

function Copy-ReleaseConfig {
    param([string]$SourceDir, [string]$Version)
    if (-not (Test-Path -LiteralPath $SourceDir -PathType Container)) {
        return
    }
    $destinationRoot = Join-Path $script:AppDir "config"
    New-Item -ItemType Directory -Path $destinationRoot -Force | Out-Null
    $sourcePrefix = [IO.Path]::GetFullPath($SourceDir).TrimEnd('\') + '\'

    foreach ($sourceFile in Get-ChildItem -LiteralPath $SourceDir -File -Recurse) {
        $relative = $sourceFile.FullName.Substring($sourcePrefix.Length)
        $destination = Join-Path $destinationRoot $relative
        New-Item -ItemType Directory -Path (Split-Path -Parent $destination) -Force | Out-Null

        if ($relative -eq "agent.toml") {
            if (Test-Path -LiteralPath $destination -PathType Leaf) {
                $sourceHash = (Get-FileHash -LiteralPath $sourceFile.FullName -Algorithm SHA256).Hash
                $destinationHash = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash
                if ($sourceHash -ne $destinationHash) {
                    Copy-Item -LiteralPath $sourceFile.FullName -Destination "${destination}.release-${Version}" -Force
                }
            } else {
                Copy-Item -LiteralPath $sourceFile.FullName -Destination $destination
            }
        } elseif ($sourceFile.Name -match '\.example(?:\.|$)') {
            Copy-Item -LiteralPath $sourceFile.FullName -Destination $destination -Force
        } elseif (-not (Test-Path -LiteralPath $destination)) {
            Copy-Item -LiteralPath $sourceFile.FullName -Destination $destination
        }
    }
}

function Install-ReleasePayload {
    param([string]$ReleaseDir, [string]$Version)
    foreach ($required in @(
        "qq-maid-bot.exe", "botctl.ps1", "botctl.cmd",
        "config\.env.example", "config\agent.toml", "README.md", "VERSION"
    )) {
        if (-not (Test-Path -LiteralPath (Join-Path $ReleaseDir $required) -PathType Leaf)) {
            throw "Release package is missing $required"
        }
    }

    New-Item -ItemType Directory -Path $script:AppDir -Force | Out-Null
    foreach ($name in @(
        "qq-maid-bot.exe", "botctl.ps1", "botctl.cmd", "qbot.ps1", "qbot.cmd",
        "windows-startup-example.bat", "README.md", "VERSION"
    )) {
        $source = Join-Path $ReleaseDir $name
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Copy-Item -LiteralPath $source -Destination (Join-Path $script:AppDir $name) -Force
        }
    }

    # Bootstrap against an older Windows Release that predates qbot.ps1/qbot.cmd.
    $installedQbot = Join-Path $script:AppDir "qbot.ps1"
    $releaseQbot = Join-Path $ReleaseDir "qbot.ps1"
    if (-not (Test-Path -LiteralPath $releaseQbot -PathType Leaf) -and
        -not $script:InstallerPath.Equals($installedQbot, [StringComparison]::OrdinalIgnoreCase)) {
        Copy-Item -LiteralPath $script:InstallerPath -Destination $installedQbot -Force
    }
    $installedWrapper = Join-Path $script:AppDir "qbot.cmd"
    if (-not (Test-Path -LiteralPath (Join-Path $ReleaseDir "qbot.cmd") -PathType Leaf)) {
        Write-Utf8Lines -Path $installedWrapper -Lines @(
            "@echo off",
            "setlocal",
            'powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0qbot.ps1" %*',
            "exit /b %errorlevel%"
        )
    }
    Copy-ReleaseConfig -SourceDir (Join-Path $ReleaseDir "config") -Version $Version

    foreach ($directory in @("data\storage", "logs", "run")) {
        New-Item -ItemType Directory -Path (Join-Path $script:AppDir $directory) -Force | Out-Null
    }
    $configFile = Join-Path $script:AppDir "config\.env"
    if (-not (Test-Path -LiteralPath $configFile -PathType Leaf)) {
        Copy-Item -LiteralPath (Join-Path $script:AppDir "config\.env.example") -Destination $configFile
        Write-Output "created config template: $configFile"
    }
    Remove-ObsoleteEnvConfig -ConfigFile $configFile
    if (Get-Command Migrate-AgentWebSearchConfig -CommandType Function -ErrorAction SilentlyContinue) {
        Migrate-AgentWebSearchConfig -ConfigFile (Join-Path $script:AppDir "config\agent.toml")
    }

    # Remove obsolete distribution files only; private config and runtime data stay untouched.
    foreach ($obsolete in @(
        "botctl.sh", "botmon.sh", "diagnose-network.sh", "validate-runtime.sh",
        "qq-maid-healthcheck.sh", "qq-maid-systemd.sh", ".env.example"
    )) {
        Remove-Item -LiteralPath (Join-Path $script:AppDir $obsolete) -Force -ErrorAction SilentlyContinue
    }
}

function Remove-ObsoleteEnvConfig {
    param([Parameter(Mandatory = $true)][string]$ConfigFile)
    if (-not (Test-Path -LiteralPath $ConfigFile -PathType Leaf)) {
        return
    }
    if ((Get-Item -LiteralPath $ConfigFile -Force).LinkType) {
        [Console]::Error.WriteLine("warning: skip obsolete env migration for symbolic link: $ConfigFile")
        return
    }

    $lines = @(Get-Content -LiteralPath $ConfigFile)
    $removed = New-Object Collections.Generic.List[string]
    $filtered = New-Object Collections.Generic.List[string]
    foreach ($line in $lines) {
        if ($line -match '^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=' -and
            $script:ObsoleteEnvKeys -contains $Matches[1]) {
            if (-not $removed.Contains($Matches[1])) {
                $removed.Add($Matches[1])
            }
            continue
        }
        $filtered.Add($line)
    }
    if ($removed.Count -eq 0) {
        return
    }

    $stamp = Get-Date -Format "yyyyMMdd_HHmmss"
    $backup = "${ConfigFile}.bak.v0.20.${stamp}.$PID"
    Copy-Item -LiteralPath $ConfigFile -Destination $backup
    $tempFile = "${ConfigFile}.tmp.$PID"
    Write-Utf8Lines -Path $tempFile -Lines $filtered.ToArray()
    Move-Item -LiteralPath $tempFile -Destination $ConfigFile -Force
    Write-Output "removed obsolete config keys from config\.env: $($removed -join ', ')"
    Write-Output "pre-upgrade config backup: $backup"
    Write-Output "Remove the same keys manually if systemd, Docker, or the host environment still injects them."
}

function Get-NextAgentConfigBackupPath {
    param([Parameter(Mandatory = $true)][string]$ConfigFile)
    $candidate = "${ConfigFile}.old"
    $suffix = 0
    while (Test-Path -LiteralPath $candidate) {
        $suffix++
        $candidate = "${ConfigFile}.old.${suffix}"
    }
    return $candidate
}

function Replace-AgentConfigFromRelease {
    param(
        [Parameter(Mandatory = $true)][string]$ConfigFile,
        [Parameter(Mandatory = $true)][string]$TemplateFile
    )
    if (-not (Test-Path -LiteralPath $ConfigFile -PathType Leaf)) {
        throw "Agent config replacement failed: existing file not found: $ConfigFile"
    }
    if ((Get-Item -LiteralPath $ConfigFile -Force).LinkType) {
        throw "Agent config replacement failed: symbolic links are not replaced automatically: $ConfigFile"
    }
    if (-not (Test-Path -LiteralPath $TemplateFile -PathType Leaf)) {
        throw "Agent config replacement failed: Release template not found: $TemplateFile"
    }

    $directory = Split-Path -Parent $ConfigFile
    $tempFile = Join-Path $directory (".agent.toml.new." + [Guid]::NewGuid().ToString("N"))
    $backup = Get-NextAgentConfigBackupPath -ConfigFile $ConfigFile
    $backupCreated = $false
    try {
        Copy-Item -LiteralPath $TemplateFile -Destination $tempFile
        $templateHash = (Get-FileHash -LiteralPath $TemplateFile -Algorithm SHA256).Hash
        $tempHash = (Get-FileHash -LiteralPath $tempFile -Algorithm SHA256).Hash
        if ($templateHash -ne $tempHash) {
            throw "the new template could not be written completely"
        }

        Move-Item -LiteralPath $ConfigFile -Destination $backup
        $backupCreated = $true
        Move-Item -LiteralPath $tempFile -Destination $ConfigFile
    } catch {
        $reason = $_.Exception.Message
        if ($backupCreated -and -not (Test-Path -LiteralPath $ConfigFile)) {
            try {
                Move-Item -LiteralPath $backup -Destination $ConfigFile
                $backupCreated = $false
                throw "Agent config replacement failed: $reason; the original file was restored"
            } catch {
                if ($backupCreated) {
                    throw "Agent config replacement failed: $reason; automatic restore failed and the original file remains at $backup"
                }
                throw
            }
        }
        throw "Agent config replacement failed: $reason; the original file was not modified"
    } finally {
        Remove-Item -LiteralPath $tempFile -Force -ErrorAction SilentlyContinue
    }

    Write-Output "已使用当前 Release 的新版默认配置替换 agent.toml"
    Write-Output "旧配置备份: $backup"
    Write-Output "请参考备份重新填写 Provider、模型路线、Scene 和工具白名单等自定义配置。"
}

function Update-AgentConfigFromRelease {
    param(
        [Parameter(Mandatory = $true)][string]$ConfigFile,
        [Parameter(Mandatory = $true)][string]$TemplateFile
    )
    if (-not (Test-Path -LiteralPath $ConfigFile)) {
        return
    }
    Write-Output "检测到跨版本升级，自动备份并更新 agent.toml。"
    Replace-AgentConfigFromRelease -ConfigFile $ConfigFile -TemplateFile $TemplateFile
}

function Install-OrUpdate {
    param([string]$Mode, [string]$RequestedVersion)
    Assert-SupportedWindowsArchitecture (Get-WindowsOperatingSystemArchitecture)
    $version = Resolve-Version $RequestedVersion
    $current = Get-LocalVersion
    $package = "qq-maid-bot-${version}-windows-x86_64"
    $archiveName = "${package}.zip"
    $tempDir = Join-Path ([IO.Path]::GetTempPath()) ("qbot-install-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Path $tempDir | Out-Null
    try {
        $archive = Join-Path $tempDir $archiveName
        $checksum = "${archive}.sha256"
        $rawUrl = "$($script:ReleasesUrl)/download/${version}/${archiveName}"
        Write-Output "downloading Release: $version (windows-x86_64)"
        Save-ReleaseFile -Url $rawUrl -Destination $archive
        Save-ReleaseFile -Url "${rawUrl}.sha256" -Destination $checksum
        Test-ReleaseChecksum -Archive $archive -ChecksumFile $checksum
        Expand-Archive -LiteralPath $archive -DestinationPath $tempDir -Force
        $releaseDir = Join-Path $tempDir $package
        $agentConfigModule = Join-Path $releaseDir "lib\agent-config.ps1"
        if (Test-Path -LiteralPath $agentConfigModule -PathType Leaf) {
            . $agentConfigModule
        }

        if ($Mode -eq "update" -and $null -ne $current -and (Normalize-Version $current) -eq $version) {
            Remove-ObsoleteEnvConfig -ConfigFile (Join-Path $script:AppDir "config\.env")
            if (Get-Command Migrate-AgentWebSearchConfig -CommandType Function -ErrorAction SilentlyContinue) {
                Migrate-AgentWebSearchConfig -ConfigFile (Join-Path $script:AppDir "config\agent.toml")
            }
            Complete-AgentConfigMigration -CurrentVersion $current -TargetVersion $version
            Write-Output "already installed: $current"
            return
        }

        # v0.20.2 完成一次结构升级；跨过门槛后只靠字段默认值兼容，不再覆盖用户策略。
        $agentConfigMarker = Join-Path $script:AppDir "config\$($script:AgentConfigMigrationMarkerName)"
        if ($Mode -eq "update" -and (Test-AgentConfigResetRequired -CurrentVersion $current -TargetVersion $version -MarkerFile $agentConfigMarker)) {
            Update-AgentConfigFromRelease `
                -ConfigFile (Join-Path $script:AppDir "config\agent.toml") `
                -TemplateFile (Join-Path $releaseDir "config\agent.toml")
        }

        $wasRunning = Test-InstalledBotRunning
        if ($wasRunning) {
            Write-Output "stopping the running bot before updating"
            Invoke-BotControl "stop"
        }
        Install-ReleasePayload -ReleaseDir $releaseDir -Version $version
        Complete-AgentConfigMigration -CurrentVersion $current -TargetVersion $version
        Write-Output "qbot $Mode completed: $version"
        Write-Output "directory: $($script:AppDir)"
        Write-Output "config: $(Join-Path $script:AppDir 'config\.env')"
        if (-not $wasRunning) { Write-ConsoleConfigHint -NextStart }
        if ($wasRunning) {
            Invoke-BotControl "start"
        }
    } finally {
        Remove-Item -LiteralPath $tempDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Get-ConfigFile {
    $configDir = Join-Path $script:AppDir "config"
    $configFile = Join-Path $configDir ".env"
    New-Item -ItemType Directory -Path $configDir -Force | Out-Null
    if (-not (Test-Path -LiteralPath $configFile -PathType Leaf)) {
        $example = Join-Path $configDir ".env.example"
        if (-not (Test-Path -LiteralPath $example -PathType Leaf)) {
            throw "config template not found; run qbot install first"
        }
        Copy-Item -LiteralPath $example -Destination $configFile
    }
    return $configFile
}

function ConvertFrom-DotEnvValue {
    param([string]$RawValue)
    $value = $RawValue.Trim()
    if ($value.Length -ge 2 -and $value[0] -eq "'" -and $value[$value.Length - 1] -eq "'") {
        return $value.Substring(1, $value.Length - 2)
    }
    if ($value.Length -ge 2 -and $value[0] -eq '"' -and $value[$value.Length - 1] -eq '"') {
        return $value.Substring(1, $value.Length - 2).Replace('\"', '"').Replace('\\', '\')
    }
    return ($value -replace '\s+#.*$', '')
}

function Read-ConfigValues {
    $values = [ordered]@{}
    foreach ($line in Get-Content -LiteralPath (Get-ConfigFile)) {
        if ($line -match '^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$') {
            $values[$Matches[1]] = ConvertFrom-DotEnvValue $Matches[2]
        }
    }
    return $values
}

function Get-ConfiguredValue {
    param([string]$Name, [string]$DefaultValue = "")
    $environmentValue = [Environment]::GetEnvironmentVariable($Name)
    if (-not [string]::IsNullOrWhiteSpace($environmentValue)) {
        return $environmentValue
    }
    $values = Read-ConfigValues
    if ($values.Contains($Name) -and -not [string]::IsNullOrWhiteSpace([string]$values[$Name])) {
        return [string]$values[$Name]
    }
    return $DefaultValue
}

function Get-ConfiguredConsoleUrl {
    $explicitUrl = Get-ConfiguredValue "LLM_SERVER_URL"
    if (-not [string]::IsNullOrWhiteSpace($explicitUrl)) {
        return $explicitUrl.TrimEnd('/')
    }
    $hostName = Get-ConfiguredValue "LLM_SERVER_HOST" "127.0.0.1"
    $port = Get-ConfiguredValue "LLM_SERVER_PORT" "8787"
    return "http://${hostName}:${port}"
}

function Write-ConsoleConfigHint {
    param([switch]$NextStart)
    $enabled = Get-ConfiguredValue "WEB_CONSOLE_ENABLED" "true"
    if ($enabled.Equals("false", [StringComparison]::OrdinalIgnoreCase)) {
        return
    }
    $url = Get-ConfiguredConsoleUrl
    $parsed = $null
    if ([Uri]::TryCreate($url, [UriKind]::Absolute, [ref]$parsed) -and
        $parsed.Host -in @("0.0.0.0", "::")) {
        Write-Output "v0.20 起可通过控制台完成配置；当前监听通配地址，请使用实际服务器地址或反向代理地址访问 /console/"
        return
    }
    $when = if ($NextStart) { "在下次 qbot start 后，" } else { "" }
    Write-Output "v0.20 起推荐${when}通过 ${url}/console/ 网页完成配置"
}

function Write-ConfigDoneHint {
    Write-Output "配置已写入: $(Get-ConfigFile)"
    Write-Output "提示: 下次 qbot start 时生效"
    Write-ConsoleConfigHint -NextStart
}

function Write-Utf8Lines {
    param([string]$Path, [string[]]$Lines)
    $encoding = New-Object Text.UTF8Encoding($false)
    [IO.File]::WriteAllLines($Path, $Lines, $encoding)
}

function Set-ConfigValue {
    param([string]$Name, [string]$Value)
    if ($Name -notmatch '^[A-Za-z_][A-Za-z0-9_]*$') {
        throw "invalid environment variable name: $Name"
    }
    if ($Value.Contains("`r") -or $Value.Contains("`n")) {
        throw "configuration values cannot contain newlines"
    }
    if ($script:ObsoleteEnvKeys -contains $Name) {
        throw "$Name was removed; edit config/agent.toml for Agent policy"
    }
    $escaped = $Value.Replace('\', '\\').Replace('"', '\"')
    $replacement = "$Name=`"$escaped`""
    $configFile = Get-ConfigFile
    $pattern = '^\s*(?:export\s+)?' + [Regex]::Escape($Name) + '\s*='
    $result = New-Object Collections.Generic.List[string]
    $replaced = $false
    foreach ($line in Get-Content -LiteralPath $configFile) {
        if ($line -match $pattern) {
            if (-not $replaced) {
                $result.Add($replacement)
                $replaced = $true
            }
        } else {
            $result.Add($line)
        }
    }
    if (-not $replaced) {
        $result.Add($replacement)
    }
    Write-Utf8Lines -Path $configFile -Lines $result.ToArray()
}

function Show-Config {
    param([string[]]$Names)
    $values = Read-ConfigValues
    $selectedNames = $Names
    if ($null -eq $selectedNames -or $selectedNames.Count -eq 0) {
        $selectedNames = @($values.Keys)
    }
    foreach ($name in $selectedNames) {
        if (-not $values.Contains($name)) {
            continue
        }
        $value = [string]$values[$name]
        if ($name -match '(?i)(SECRET|TOKEN|PASSWORD|API_KEY|APP_ID|_KEY$)') {
            if ($value.Length -gt 6) {
                $value = $value.Substring(0, 2) + "***" + $value.Substring($value.Length - 2)
            } elseif ($value.Length -gt 0) {
                $value = "***"
            }
        }
        Write-Output "$name=$value"
    }
}

function Parse-Options {
    param([string[]]$Arguments)
    $options = @{}
    for ($index = 0; $index -lt $Arguments.Count; $index++) {
        $name = $Arguments[$index]
        if ($name -in @("--enable", "--disable", "--unbind")) {
            $options[$name] = $true
            continue
        }
        if (-not $name.StartsWith("--") -or $index + 1 -ge $Arguments.Count) {
            throw "invalid or missing option value: $name"
        }
        $index++
        $options[$name] = $Arguments[$index]
    }
    return $options
}

function Configure-Bot {
    param([string[]]$Arguments)
    $options = Parse-Options $Arguments
    $modes = @("--enable", "--disable", "--unbind") | Where-Object { $options.ContainsKey($_) }
    if ($modes.Count -gt 1) {
        throw "--enable, --disable and --unbind are mutually exclusive"
    }
    $mapping = @{
        "--app-id" = "QQ_BOT_APP_ID"; "--app-secret" = "QQ_BOT_APP_SECRET";
        "--sandbox" = "QQ_BOT_SANDBOX"; "--group-mode" = "QQ_MAID_GROUP_RESPONSE_MODE";
        "--active-keywords" = "QQ_MAID_GROUP_ACTIVE_KEYWORDS"; "--mention-ids" = "QQ_MAID_BOT_MENTION_IDS"
    }
    foreach ($option in $mapping.Keys) {
        if ($options.ContainsKey($option)) {
            Set-ConfigValue $mapping[$option] ([string]$options[$option])
        }
    }
    if ($options.ContainsKey("--enable")) { Set-ConfigValue "QQ_CHANNEL_ENABLED" "true" }
    if ($options.ContainsKey("--disable")) { Set-ConfigValue "QQ_CHANNEL_ENABLED" "false" }
    if ($options.ContainsKey("--unbind")) {
        Set-ConfigValue "QQ_BOT_APP_ID" ""
        Set-ConfigValue "QQ_BOT_APP_SECRET" ""
        Set-ConfigValue "QQ_CHANNEL_ENABLED" "false"
    }
}

function Configure-Ai {
    param([string[]]$Arguments)
    $options = Parse-Options $Arguments
    $provider = "openai"
    if ($options.ContainsKey("--provider")) { $provider = [string]$options["--provider"] }
    $prefix = switch ($provider) {
        "deepseek" { "DEEPSEEK" }
        "bigmodel" { "GLM" }
        "mimo" { "MIMO" }
        default { "OPENAI" }
    }
    if ($options.ContainsKey("--api-key")) { Set-ConfigValue "${prefix}_API_KEY" ([string]$options["--api-key"]) }
    if ($options.ContainsKey("--base-url")) { Set-ConfigValue "${prefix}_BASE_URL" ([string]$options["--base-url"]) }
    foreach ($removedOption in @("--model", "--private-model", "--group-model", "--search-model")) {
        if ($options.ContainsKey($removedOption)) {
            throw "$removedOption was removed; edit config/agent.toml for Agent policy"
        }
    }
    if ($options.ContainsKey("--api-mode")) { Set-ConfigValue "OPENAI_API_MODE" ([string]$options["--api-mode"]) }
}

function Invoke-ConfigCommand {
    param([string[]]$Arguments)
    if ($null -eq $Arguments -or $Arguments.Count -eq 0) {
        throw "config requires path, show, get, set, bot or ai"
    }
    $subcommand = $Arguments[0]
    $remaining = @($Arguments | Select-Object -Skip 1)
    switch ($subcommand) {
        "path" { Write-Output (Get-ConfigFile) }
        "show" { Show-Config $remaining }
        "get" {
            if ($remaining.Count -ne 1) { throw "usage: qbot config get KEY" }
            $values = Read-ConfigValues
            if (-not $values.Contains($remaining[0])) { throw "configuration key not found: $($remaining[0])" }
            Write-Output $values[$remaining[0]]
        }
        "set" {
            if ($remaining.Count -eq 0) { throw "usage: qbot config set KEY=VALUE" }
            foreach ($assignment in $remaining) {
                $separator = $assignment.IndexOf('=')
                if ($separator -le 0) { throw "invalid assignment: $assignment" }
                Set-ConfigValue $assignment.Substring(0, $separator) $assignment.Substring($separator + 1)
            }
            Write-ConfigDoneHint
        }
        "bot" { Configure-Bot $remaining; Write-ConfigDoneHint }
        "ai" { Configure-Ai $remaining; Write-ConfigDoneHint }
        default { throw "unknown config command: $subcommand" }
    }
}

function Invoke-Qbot {
    param([string]$QbotCommand, [string[]]$Arguments)
    $requestedVersion = "latest"
    if ($null -ne $Arguments -and $Arguments.Count -gt 0) { $requestedVersion = $Arguments[0] }
    switch ($QbotCommand) {
        "install" { Install-OrUpdate "install" $requestedVersion }
        { $_ -in @("update", "upgrade", "patch") } { Install-OrUpdate "update" $requestedVersion }
        "version" {
            $localVersion = Get-LocalVersion
            if ($null -eq $localVersion) { $localVersion = "not installed" }
            Write-Output "installed version: $localVersion"
            Write-Output "latest version: $(Get-LatestVersion)"
        }
        { $_ -in @("start", "stop", "restart", "status", "health", "console") } { Invoke-BotControl $QbotCommand }
        { $_ -in @("log", "logs") } { Invoke-BotControl "logs" }
        "config" { Invoke-ConfigCommand $Arguments }
        { $_ -in @("help", "-h", "--help") } { Show-QbotUsage }
        default { throw "unknown command: $QbotCommand" }
    }
}

# Dot-sourced regression tests load functions without dispatching a command.
if ($MyInvocation.InvocationName -ne '.') {
    try {
        Invoke-Qbot -QbotCommand $Command -Arguments $CommandArgs
    } catch {
        [Console]::Error.WriteLine("error: $($_.Exception.Message)")
        exit 1
    }
}

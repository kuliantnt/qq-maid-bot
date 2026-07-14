$ErrorActionPreference = "Stop"
$repoDir = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ("qq-maid-qbot-" + [Guid]::NewGuid())
$appDir = Join-Path $testRoot "app"
$releaseDir = Join-Path $testRoot "release"
$oldAppDir = $env:QBOT_APP_DIR

function Assert-True {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

try {
    $env:QBOT_APP_DIR = $appDir
    . (Join-Path $repoDir "scripts\qbot.ps1")

    New-Item -ItemType Directory -Path (Join-Path $releaseDir "config") -Force | Out-Null
    foreach ($name in @("qq-maid-bot.exe", "README.md", "VERSION", "windows-startup-example.bat")) {
        Set-Content -LiteralPath (Join-Path $releaseDir $name) -Value "release" -Encoding ASCII
    }
    foreach ($name in @("botctl.ps1", "botctl.cmd")) {
        Copy-Item -LiteralPath (Join-Path $repoDir "scripts\$name") -Destination (Join-Path $releaseDir $name)
    }
    Set-Content -LiteralPath (Join-Path $releaseDir "config\.env.example") -Value "EXAMPLE=1" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $releaseDir "config\agent.toml") -Value "release-agent" -Encoding ASCII

    New-Item -ItemType Directory -Path (Join-Path $appDir "config") -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $appDir "data\storage") -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $appDir "logs") -Force | Out-Null
    Set-Content -LiteralPath (Join-Path $appDir "config\.env") -Value "PRIVATE=keep" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $appDir "config\agent.toml") -Value "custom-agent" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $appDir "data\storage\app.db") -Value "db" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $appDir "logs\qq-maid-bot.log") -Value "log" -Encoding ASCII
    Set-Content -LiteralPath (Join-Path $appDir "botctl.sh") -Value "obsolete" -Encoding ASCII

    Install-ReleasePayload -ReleaseDir $releaseDir -Version "v9.9.9"
    Assert-True ((Get-Content -LiteralPath (Join-Path $appDir "config\.env") -Raw).Contains("PRIVATE=keep")) "private config was overwritten"
    Assert-True ((Get-Content -LiteralPath (Join-Path $appDir "data\storage\app.db") -Raw).Contains("db")) "database was overwritten"
    Assert-True ((Get-Content -LiteralPath (Join-Path $appDir "logs\qq-maid-bot.log") -Raw).Contains("log")) "log was overwritten"
    Assert-True (Test-Path -LiteralPath (Join-Path $appDir "config\agent.toml.release-v9.9.9")) "agent.toml update candidate is missing"
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $appDir "botctl.sh"))) "Unix control script was not removed"
    Assert-True (Test-Path -LiteralPath (Join-Path $appDir "qbot.cmd")) "qbot.cmd was not installed"

    Invoke-ConfigCommand @("set", "OPENAI_API_KEY=secret-value", "LLM_MODEL=openai:test")
    $values = Read-ConfigValues
    Assert-True ($values["OPENAI_API_KEY"] -eq "secret-value") "API key config was not written"
    Assert-True ($values["LLM_MODEL"] -eq "openai:test") "model config was not written"
    $masked = (Show-Config @("OPENAI_API_KEY")) -join "`n"
    Assert-True (-not $masked.Contains("secret-value")) "config show leaked a secret"

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

    $archive = Join-Path $testRoot "fixture.zip"
    $checksum = "${archive}.sha256"
    Set-Content -LiteralPath $archive -Value "fixture" -Encoding ASCII
    $hash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath $checksum -Value "$hash  fixture.zip" -Encoding ASCII
    Test-ReleaseChecksum -Archive $archive -ChecksumFile $checksum

    Write-Output "PowerShell qbot regression tests passed"
} finally {
    $env:QBOT_APP_DIR = $oldAppDir
    Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}

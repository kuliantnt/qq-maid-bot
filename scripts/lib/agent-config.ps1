# Loaded by qbot.ps1 from the extracted release payload; not a standalone command.

function Migrate-AgentWebSearchConfig {
    param([Parameter(Mandatory = $true)][string]$ConfigFile)
    if (-not (Test-Path -LiteralPath $ConfigFile -PathType Leaf)) {
        return
    }
    if ((Get-Item -LiteralPath $ConfigFile -Force).LinkType) {
        Write-Warning "skip symbolic-link Agent config web-search migration: $ConfigFile"
        return
    }

    # Windows PowerShell 5.1 默认按系统 ANSI 读文件；agent.toml 是 UTF-8 无 BOM，必须显式指定。
    $utf8 = New-Object Text.UTF8Encoding($false)
    $lines = [IO.File]::ReadAllLines($ConfigFile, $utf8)
    $legacyPattern = '^\s*\[search_routes\.[A-Za-z0-9_-]+\]\s*(#.*)?$'
    $newPattern = '^\s*\[tools\.web_search\.routes\.[A-Za-z0-9_-]+\]\s*(#.*)?$'
    if (-not ($lines | Where-Object { $_ -match $legacyPattern })) {
        return
    }
    if ($lines | Where-Object { $_ -match $newPattern }) {
        throw "Agent config contains both legacy and unified web-search routes; merge it manually: $ConfigFile"
    }

    $hasTools = [bool]($lines | Where-Object { $_ -match '^\s*\[tools\.web_search\]\s*(#.*)?$' })
    $migrated = New-Object Collections.Generic.List[string]
    $inserted = $false
    foreach ($line in $lines) {
        if (-not $hasTools -and -not $inserted -and $line -match $legacyPattern) {
            @(
                '[tools.web_search]',
                'backend = "provider_native"',
                'max_results = 5',
                'search_depth = "basic"',
                'topic = "general"',
                'connect_timeout_seconds = 10',
                'first_response_timeout_seconds = 30',
                'total_timeout_seconds = 60',
                ''
            ) | ForEach-Object { $migrated.Add($_) }
            $inserted = $true
        }
        if ($line -match $legacyPattern) {
            $migrated.Add([regex]::Replace($line, '\[search_routes\.', '[tools.web_search.routes.', 1))
        } else {
            $migrated.Add($line)
        }
    }

    $backup = Get-NextAgentConfigBackupPath -ConfigFile $ConfigFile
    Copy-Item -LiteralPath $ConfigFile -Destination $backup
    $tempFile = Join-Path (Split-Path -Parent $ConfigFile) (".agent.toml.web-search." + [Guid]::NewGuid().ToString("N"))
    try {
        Write-Utf8Lines -Path $tempFile -Lines $migrated.ToArray()
        Move-Item -LiteralPath $tempFile -Destination $ConfigFile -Force
    } catch {
        Remove-Item -LiteralPath $tempFile -Force -ErrorAction SilentlyContinue
        throw "Agent web-search config migration failed; original config remains at $ConfigFile and backup is $backup"
    }
    Write-Output "Migrated legacy web-search routes to tools.web_search; backup: $backup"
}

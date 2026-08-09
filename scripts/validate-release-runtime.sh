#!/usr/bin/env bash
set -euo pipefail

RUNTIME_DIR="${1:-$(pwd)}"
PAYLOAD_PROFILE="${2:-all}"

die() {
    echo "error: $*" >&2
    exit 1
}

require_file() {
    [[ -f "${RUNTIME_DIR}/$1" ]] || die "missing $1"
}

require_executable() {
    require_file "$1"
    [[ -x "${RUNTIME_DIR}/$1" ]] || die "$1 is not executable"
}

require_any_executable() {
    local candidate
    for candidate in "$@"; do
        if [[ -f "${RUNTIME_DIR}/${candidate}" && -x "${RUNTIME_DIR}/${candidate}" ]]; then
            return 0
        fi
    done
    die "missing executable: $*"
}

# 这里只校验待发布 runtime 目录的离线结构是否完整，以及是否混入敏感/运行产物。
# 服务状态、/healthz、上游调用和 /console 等在线检查由 scripts/validate-runtime.sh 负责。
require_any_executable qq-maid-bot qq-maid-bot.exe

case "${PAYLOAD_PROFILE}" in
    windows)
        require_file lib/agent-config.ps1
        require_file qbot.ps1
        require_file qbot.cmd
        require_file botctl.ps1
        require_file botctl.cmd
        require_file windows-startup-example.bat
        ;;
    unix)
        require_file lib/agent-config.sh
        require_executable botctl.sh
        require_executable validate-runtime.sh
        require_executable diagnose-network.sh
        require_executable qq-maid-healthcheck.sh
        require_executable botmon.sh
        require_executable qq-maid-systemd.sh
        ;;
    all)
        require_file lib/agent-config.sh
        require_file lib/agent-config.ps1
        require_file qbot.ps1
        require_file qbot.cmd
        require_executable botctl.sh
        require_file botctl.ps1
        require_file botctl.cmd
        require_executable validate-runtime.sh
        require_executable diagnose-network.sh
        require_executable qq-maid-healthcheck.sh
        require_executable botmon.sh
        require_executable qq-maid-systemd.sh
        require_file windows-startup-example.bat
        ;;
    *)
        die "unsupported payload profile: ${PAYLOAD_PROFILE}"
        ;;
esac

require_file config/.env.example
require_file config/agent.example.toml
require_file config/ops.example.toml
require_file config/runtime.example.toml
require_file README.md

if find "${RUNTIME_DIR}" -path '*/logs/*' -o -path '*/run/*.pid' -o -path '*/config/secrets/*' -o -name '.env' -o -name 'runtime.toml' -o -name 'master.key' -o -name '*.db' -o -name '*.bak' | grep -q .; then
    die "runtime contains forbidden private or generated files"
fi

echo "runtime payload validation ok: ${RUNTIME_DIR}"

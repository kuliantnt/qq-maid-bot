#!/usr/bin/env bash
set -euo pipefail

RUNTIME_DIR="${1:-$(pwd)}"

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
require_executable botctl.sh
require_executable validate-runtime.sh
require_executable diagnose-network.sh
require_executable qq-maid-healthcheck.sh
require_executable botmon.sh
require_executable qq-maid-systemd.sh
require_file windows-startup-example.bat
require_file .env.example
require_file config/agent.toml
require_file README.md
require_file static/index.html

if find "${RUNTIME_DIR}" -path '*/logs/*' -o -path '*/run/*.pid' -o -name '.env' -o -name '*.db' -o -name '*.bak' | grep -q .; then
    die "runtime contains forbidden private or generated files"
fi

echo "runtime payload validation ok: ${RUNTIME_DIR}"

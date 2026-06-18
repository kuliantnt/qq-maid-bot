#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_NAME="$(basename -- "${BASH_SOURCE[0]}")"

if [[ "${SCRIPT_NAME}" == "llmctl.sh" && -d "${SCRIPT_DIR}/config" ]]; then
    REPO_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
    DEFAULT_RUNTIME_DIR="${SCRIPT_DIR}"
else
    REPO_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
    DEFAULT_RUNTIME_DIR="${REPO_DIR}/runtime"
fi
# 默认运行目录只放部署产物和运行配置。脚本位于 runtime 根目录时，直接使用脚本所在目录；
# 位于源码 scripts/ 目录时，继续使用仓库 runtime/，以兼容本地 make 和远端部署两种形态。
RUNTIME_DIR="${QQ_MAID_RUNTIME_DIR:-${DEFAULT_RUNTIME_DIR}}"

DEFAULT_BINARY="${RUNTIME_DIR}/qq-maid-llm"
BINARY="${LLM_BINARY:-${DEFAULT_BINARY}}"
PID_FILE="${LLM_PID_FILE:-${RUNTIME_DIR}/run/qq-maid-llm.pid}"
LOG_FILE="${LLM_LOG_FILE:-${RUNTIME_DIR}/logs/qq-maid-llm.log}"

usage() {
    cat <<'EOF'
Usage: llmctl.sh <command>

Commands:
  start     Start qq-maid-llm in the background
  stop      Stop qq-maid-llm
  restart   Restart qq-maid-llm
  status    Show process status
  health    Request /healthz
  logs      Tail the log file

Environment overrides:
  LLM_BINARY    Path to qq-maid-llm executable
  LLM_ENV_FILE  Env file to load before starting
  LLM_PID_FILE  PID file path
  LLM_LOG_FILE  Log file path
  QQ_MAID_RUNTIME_DIR  Runtime directory containing binaries/config/logs
  LINES         Number of log lines for logs command
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

resolve_env_file() {
    if [[ -n "${LLM_ENV_FILE:-}" ]]; then
        echo "${LLM_ENV_FILE}"
        return 0
    fi

    local candidate
    for candidate in \
        "${RUNTIME_DIR}/config/.env" \
        "${RUNTIME_DIR}/.env"
    do
        if [[ -f "${candidate}" ]]; then
            echo "${candidate}"
            return 0
        fi
    done

    return 1
}

load_env() {
    local env_file
    if ! env_file="$(resolve_env_file)"; then
        return 0
    fi
    [[ -f "${env_file}" ]] || die "env file not found: ${env_file}"

    set -a
    set +u
    # shellcheck source=/dev/null
    . "${env_file}"
    set -u
    set +a
}

read_pid() {
    [[ -f "${PID_FILE}" ]] || return 1
    local pid
    pid="$(tr -d '[:space:]' < "${PID_FILE}")"
    [[ "${pid}" =~ ^[0-9]+$ ]] || return 1
    echo "${pid}"
}

is_running() {
    local pid
    pid="$(read_pid)" || return 1
    kill -0 "${pid}" 2>/dev/null
}

server_url() {
    local host port
    host="${LLM_SERVER_HOST:-127.0.0.1}"
    port="${LLM_SERVER_PORT:-8787}"
    echo "${LLM_SERVER_URL:-http://${host}:${port}}"
}

start() {
    if is_running; then
        echo "qq-maid-llm is already running, pid=$(read_pid)"
        return 0
    fi

    [[ -f "${BINARY}" ]] || die "executable not found: ${BINARY}"
    if [[ ! -x "${BINARY}" ]]; then
        chmod +x "${BINARY}"
    fi

    mkdir -p "$(dirname -- "${PID_FILE}")" "$(dirname -- "${LOG_FILE}")"
    load_env
    export RUST_LOG="${RUST_LOG:-qq_maid_llm=info,tower_http=info}"

    (
        cd "${RUNTIME_DIR}"
        nohup "${BINARY}" >> "${LOG_FILE}" 2>&1 &
        echo "$!" > "${PID_FILE}"
    )

    sleep 1
    if ! is_running; then
        echo "qq-maid-llm failed to start. Last log lines:" >&2
        tail -n 40 "${LOG_FILE}" >&2 || true
        exit 1
    fi

    echo "qq-maid-llm started, pid=$(read_pid), log=${LOG_FILE}"
}

stop() {
    local pid
    if ! pid="$(read_pid)"; then
        echo "qq-maid-llm is not running"
        rm -f "${PID_FILE}"
        return 0
    fi

    if ! kill -0 "${pid}" 2>/dev/null; then
        echo "qq-maid-llm is not running"
        rm -f "${PID_FILE}"
        return 0
    fi

    kill "${pid}"
    local waited=0
    while kill -0 "${pid}" 2>/dev/null; do
        if (( waited >= 10 )); then
            kill -9 "${pid}" 2>/dev/null || true
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done

    rm -f "${PID_FILE}"
    echo "qq-maid-llm stopped"
}

status() {
    if is_running; then
        echo "qq-maid-llm is running, pid=$(read_pid)"
        echo "health: $(server_url | sed 's:/*$::')/healthz"
    else
        echo "qq-maid-llm is stopped"
    fi
}

health() {
    load_env
    command -v curl >/dev/null 2>&1 || die "curl is required for health"
    local url
    url="$(server_url)"
    curl -fsS "${url%/}/healthz"
    echo
}

logs() {
    mkdir -p "$(dirname -- "${LOG_FILE}")"
    touch "${LOG_FILE}"
    tail -n "${LINES:-80}" -f "${LOG_FILE}"
}

command="${1:-}"
case "${command}" in
    start)
        start
        ;;
    stop)
        stop
        ;;
    restart)
        stop
        start
        ;;
    status)
        status
        ;;
    health)
        health
        ;;
    logs)
        logs
        ;;
    -h|--help|help|"")
        usage
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
RUNTIME_DIR="${QQ_MAID_RUNTIME_DIR:-${SCRIPT_DIR}}"
BINARY="${BOT_BINARY:-${RUNTIME_DIR}/qq-maid-bot}"
PID_FILE="${BOT_PID_FILE:-${RUNTIME_DIR}/run/qq-maid-bot.pid}"
LOG_FILE="${BOT_LOG_FILE:-${RUNTIME_DIR}/logs/qq-maid-bot.log}"

usage() {
    cat <<'EOF'
Usage: botctl.sh <command>

Commands:
  start     Start qq-maid-bot in the background
  stop      Stop qq-maid-bot
  restart   Restart qq-maid-bot
  status    Show process status
  health    Request /healthz
  console   Check web console route and print its URL
  logs      Tail the log file

Environment overrides:
  BOT_BINARY      Executable path, default: runtime/qq-maid-bot
  BOT_ENV_FILE    Env file to load before starting
  BOT_PID_FILE    PID file path
  BOT_LOG_FILE    Log file path
  LLM_SERVER_URL   LLM base URL, default: http://127.0.0.1:8787
  LLM_SERVER_HOST  LLM host when LLM_SERVER_URL is unset
  LLM_SERVER_PORT  LLM port when LLM_SERVER_URL is unset
  LINES            Number of log lines for logs command
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

resolve_env_file() {
    if [[ -n "${BOT_ENV_FILE:-}" ]]; then
        echo "${BOT_ENV_FILE}"
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

start() {
    if is_running; then
        echo "qq-maid-bot is already running, pid=$(read_pid)"
        return 0
    fi

    [[ -f "${BINARY}" ]] || die "executable not found: ${BINARY}"
    if [[ ! -x "${BINARY}" ]]; then
        chmod +x "${BINARY}"
    fi

    mkdir -p "$(dirname -- "${PID_FILE}")" "$(dirname -- "${LOG_FILE}")"
    load_env
    export RUST_LOG="${RUST_LOG:-info,qq_maid_gateway_rs=debug,qq_maid_core=info,tower_http=info}"

    (
        cd "${RUNTIME_DIR}"
        nohup "${BINARY}" >> "${LOG_FILE}" 2>&1 &
        echo "$!" > "${PID_FILE}"
    )

    sleep 1
    if ! is_running; then
        echo "qq-maid-bot failed to start. Last log lines:" >&2
        tail -n 40 "${LOG_FILE}" >&2 || true
        exit 1
    fi

    echo "qq-maid-bot started, pid=$(read_pid), log=${LOG_FILE}"
}

stop() {
    local pid
    if ! pid="$(read_pid)"; then
        echo "qq-maid-bot is not running"
        rm -f "${PID_FILE}"
        return 0
    fi

    if ! kill -0 "${pid}" 2>/dev/null; then
        echo "qq-maid-bot is not running"
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
    echo "qq-maid-bot stopped"
}

restart() {
    stop
    start
}

status() {
    if is_running; then
        echo "qq-maid-bot is running, pid=$(read_pid)"
        echo "health: $(server_url | sed 's:/*$::')/healthz"
    else
        echo "qq-maid-bot is stopped"
    fi
}

server_url() {
    local host port
    host="${LLM_SERVER_HOST:-127.0.0.1}"
    port="${LLM_SERVER_PORT:-8787}"
    echo "${LLM_SERVER_URL:-http://${host}:${port}}"
}

health() {
    load_env
    command -v curl >/dev/null 2>&1 || {
        echo "error: curl is required for health" >&2
        exit 1
    }
    local url
    url="$(server_url)"
    curl -fsS "${url%/}/healthz"
    echo
}

console() {
    load_env
    command -v curl >/dev/null 2>&1 || {
        echo "error: curl is required for console" >&2
        exit 1
    }

    local url status
    url="$(server_url)"
    url="${url%/}/console/"
    status="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 15 "${url}")"
    echo "web console: ${url} -> HTTP ${status}"
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
        restart
        ;;
    status)
        status
        ;;
    health)
        health
        ;;
    console)
        console
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

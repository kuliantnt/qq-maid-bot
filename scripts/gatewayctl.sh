#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
RUNTIME_DIR="${QQ_MAID_RUNTIME_DIR:-${REPO_DIR}/llm}"

DEFAULT_BINARY="${RUNTIME_DIR}/qq-maid-gateway-rs"
BINARY="${GATEWAY_BINARY:-${DEFAULT_BINARY}}"
PID_FILE="${GATEWAY_PID_FILE:-${RUNTIME_DIR}/run/qq-maid-gateway-rs.pid}"
LOG_FILE="${GATEWAY_LOG_FILE:-${RUNTIME_DIR}/logs/qq-maid-gateway-rs.log}"

usage() {
    cat <<'EOF'
Usage: gatewayctl.sh <command>

Commands:
  start     Start qq-maid-gateway-rs in the background
  stop      Stop qq-maid-gateway-rs
  restart   Restart qq-maid-gateway-rs
  status    Show process status
  logs      Tail the log file

Environment overrides:
  GATEWAY_BINARY    Path to qq-maid-gateway-rs executable
  GATEWAY_ENV_FILE  Env file to load before starting
  GATEWAY_PID_FILE  PID file path
  GATEWAY_LOG_FILE  Log file path
  QQ_MAID_RUNTIME_DIR  Runtime directory containing binaries/config/logs
  LINES             Number of log lines for logs command
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

resolve_env_file() {
    if [[ -n "${GATEWAY_ENV_FILE:-}" ]]; then
        echo "${GATEWAY_ENV_FILE}"
        return 0
    fi

    local candidate
    for candidate in \
        "${REPO_DIR}/.env" \
        "${RUNTIME_DIR}/.env" \
        "${RUNTIME_DIR}/config/.env" \
        "${REPO_DIR}/qq-maid-gateway-rs/config/.env"
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
        echo "qq-maid-gateway-rs is already running, pid=$(read_pid)"
        return 0
    fi

    [[ -f "${BINARY}" ]] || die "executable not found: ${BINARY}"
    if [[ ! -x "${BINARY}" ]]; then
        chmod +x "${BINARY}"
    fi

    mkdir -p "$(dirname -- "${PID_FILE}")" "$(dirname -- "${LOG_FILE}")"
    load_env
    export RUST_LOG="${RUST_LOG:-info,qq_maid_gateway_rs=debug}"

    (
        cd "${RUNTIME_DIR}"
        nohup "${BINARY}" >> "${LOG_FILE}" 2>&1 &
        echo "$!" > "${PID_FILE}"
    )

    sleep 1
    if ! is_running; then
        echo "qq-maid-gateway-rs failed to start. Last log lines:" >&2
        tail -n 40 "${LOG_FILE}" >&2 || true
        exit 1
    fi

    echo "qq-maid-gateway-rs started, pid=$(read_pid), log=${LOG_FILE}"
}

stop() {
    local pid
    if ! pid="$(read_pid)"; then
        echo "qq-maid-gateway-rs is not running"
        rm -f "${PID_FILE}"
        return 0
    fi

    if ! kill -0 "${pid}" 2>/dev/null; then
        echo "qq-maid-gateway-rs is not running"
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
    echo "qq-maid-gateway-rs stopped"
}

status() {
    if is_running; then
        echo "qq-maid-gateway-rs is running, pid=$(read_pid)"
    else
        echo "qq-maid-gateway-rs is stopped"
    fi
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

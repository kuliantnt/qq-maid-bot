#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
LLM_CTL="${SCRIPT_DIR}/llmctl.sh"
GATEWAY_CTL="${SCRIPT_DIR}/gatewayctl.sh"

usage() {
    cat <<'EOF'
Usage: botctl.sh <command>

Commands:
  start     Start LLM then gateway
  stop      Stop gateway then LLM
  restart   Restart both services
  status    Show both service statuses
  health    Check LLM health and show gateway status
  console   Check web console route and print its URL
  logs      Tail both log files

Environment overrides:
  LLM_SERVER_URL   LLM base URL, default: http://127.0.0.1:8787
  LLM_SERVER_HOST  LLM host when LLM_SERVER_URL is unset
  LLM_SERVER_PORT  LLM port when LLM_SERVER_URL is unset
  LINES            Number of log lines for logs command
EOF
}

run_ctl() {
    local ctl="$1"
    local command="$2"

    [[ -f "${ctl}" ]] || {
        echo "error: control script not found: ${ctl}" >&2
        exit 1
    }
    if [[ ! -x "${ctl}" ]]; then
        chmod +x "${ctl}"
    fi

    "${ctl}" "${command}"
}

start() {
    run_ctl "${LLM_CTL}" start
    run_ctl "${GATEWAY_CTL}" start
}

stop() {
    run_ctl "${GATEWAY_CTL}" stop
    run_ctl "${LLM_CTL}" stop
}

restart() {
    stop
    start
}

status() {
    run_ctl "${LLM_CTL}" status
    run_ctl "${GATEWAY_CTL}" status
}

server_url() {
    local host port
    host="${LLM_SERVER_HOST:-127.0.0.1}"
    port="${LLM_SERVER_PORT:-8787}"
    echo "${LLM_SERVER_URL:-http://${host}:${port}}"
}

health() {
    run_ctl "${LLM_CTL}" health
    run_ctl "${GATEWAY_CTL}" status
}

console() {
    command -v curl >/dev/null 2>&1 || {
        echo "error: curl is required for console" >&2
        exit 1
    }

    local url status
    url="$(server_url)"
    url="${url%/}/console/"
    status="$(curl -fsS -o /dev/null -w '%{http_code}' --max-time 15 "${url}")"
    echo "web console: ${url} -> HTTP ${status}"
}

logs() {
    local lines="${LINES:-80}"
    local llm_log="${LLM_LOG_FILE:-${SCRIPT_DIR}/logs/qq-maid-llm.log}"
    local gateway_log="${GATEWAY_LOG_FILE:-${SCRIPT_DIR}/logs/qq-maid-gateway-rs.log}"

    mkdir -p "$(dirname -- "${llm_log}")" "$(dirname -- "${gateway_log}")"
    touch "${llm_log}" "${gateway_log}"
    tail -n "${lines}" -f "${llm_log}" "${gateway_log}"
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

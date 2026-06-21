#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_NAME="$(basename -- "${BASH_SOURCE[0]}")"

if [[ "${SCRIPT_NAME}" == "botctl.sh" && -d "${SCRIPT_DIR}/config" ]]; then
    DEFAULT_RUNTIME_DIR="${SCRIPT_DIR}"
else
    DEFAULT_RUNTIME_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/../runtime" && pwd)"
fi
RUNTIME_DIR="${QQ_MAID_RUNTIME_DIR:-${DEFAULT_RUNTIME_DIR}}"

usage() {
    cat <<'EOF'
Usage: botctl.sh <command>

Commands:
  start     Start LLM and Gateway
  stop      Stop Gateway and LLM
  restart   Restart LLM and Gateway
  status    Show both service statuses
  logs      Tail both service logs if tmux/multitail is unavailable, LLM log first
  health    Request LLM /healthz
  console   Show LLM /console/ URL and HTTP status
EOF
}

run_ctl() {
    local script="$1"
    shift
    "${RUNTIME_DIR}/${script}" "$@"
}

command="${1:-}"
case "${command}" in
    start)
        run_ctl llmctl.sh start
        run_ctl gatewayctl.sh start
        ;;
    stop)
        run_ctl gatewayctl.sh stop
        run_ctl llmctl.sh stop
        ;;
    restart)
        run_ctl llmctl.sh restart
        run_ctl gatewayctl.sh restart
        ;;
    status)
        run_ctl llmctl.sh status
        run_ctl gatewayctl.sh status
        ;;
    logs)
        echo "==> LLM logs"
        LINES="${LINES:-80}" run_ctl llmctl.sh logs
        ;;
    health)
        run_ctl llmctl.sh health
        ;;
    console)
        run_ctl llmctl.sh console
        ;;
    -h|--help|help|"")
        usage
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

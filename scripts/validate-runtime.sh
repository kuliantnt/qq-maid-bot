#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_NAME="$(basename -- "${BASH_SOURCE[0]}")"

if [[ "${SCRIPT_NAME}" == "validate-runtime.sh" && -d "${SCRIPT_DIR}/config" ]]; then
    REPO_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
    DEFAULT_RUNTIME_DIR="${SCRIPT_DIR}"
else
    REPO_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
    DEFAULT_RUNTIME_DIR="${REPO_DIR}/runtime"
fi

RUNTIME_DIR="${QQ_MAID_RUNTIME_DIR:-${DEFAULT_RUNTIME_DIR}}"
BOT_CTL="${RUNTIME_DIR}/botctl.sh"
LLM_URL="${LLM_SERVER_URL:-http://127.0.0.1:${LLM_SERVER_PORT:-8787}}"
HEALTH_URL="${LLM_URL%/}/healthz"
RESPOND_URL="${LLM_URL%/}/v1/respond"
CONSOLE_URL="${LLM_URL%/}/console/"
SOURCE_BOT_BINARY="${SOURCE_BOT_BINARY:-${REPO_DIR}/target/debug/qq-maid-bot}"
SOURCE_BOT_PID_FILE="${SOURCE_BOT_PID_FILE:-${RUNTIME_DIR}/run/qq-maid-bot-source.pid}"
SOURCE_BOT_LOG_FILE="${SOURCE_BOT_LOG_FILE:-${RUNTIME_DIR}/logs/qq-maid-bot-source.log}"
BOT_LOG_FILE_DEFAULT="${BOT_LOG_FILE:-${RUNTIME_DIR}/logs/qq-maid-bot.log}"

usage() {
    cat <<'EOF'
Usage: validate-runtime.sh <command>

Commands:
  check             Check service status, LLM health, GLM upstream, console, and bot logs
  glm              Run only the GLM/OpenAI-compatible upstream diagnostic
  console          Check only the web console route
  logs             Show recent bot logs
  restart          Restart deployed qq-maid-bot, then run check
  restart-source   Restart source-built debug qq-maid-bot, then run check

Environment overrides:
  QQ_MAID_RUNTIME_DIR       Runtime directory, default: runtime/
  LLM_SERVER_URL            LLM base URL, default: http://127.0.0.1:8787
  LINES                     Log lines to show, default: 80
  SOURCE_BOT_BINARY         Debug/source bot binary for restart-source
EOF
}

require_file() {
    local path="$1"
    [[ -f "${path}" ]] || {
        echo "error: required file not found: ${path}" >&2
        exit 1
    }
}

curl_json() {
    local url="$1"
    shift
    curl -fsS --max-time 60 "$@" "${url}"
}

print_heading() {
    printf '\n== %s ==\n' "$1"
}

bot_status() {
    print_heading "service status"
    require_file "${BOT_CTL}"
    "${BOT_CTL}" status || true
    if [[ -f "${SOURCE_BOT_PID_FILE}" ]]; then
        BOT_PID_FILE="${SOURCE_BOT_PID_FILE}" \
        BOT_LOG_FILE="${SOURCE_BOT_LOG_FILE}" \
        BOT_BINARY="${SOURCE_BOT_BINARY}" \
            "${BOT_CTL}" status || true
    fi
}

health_check() {
    print_heading "LLM health"
    curl_json "${HEALTH_URL}"
    printf '\n'
}

glm_check() {
    print_heading "GLM/OpenAI-compatible upstream check"
    curl_json "${RESPOND_URL}" \
        -X POST \
        -H 'Content-Type: application/json' \
        -d '{"diagnostic":"upstream_check","scope_key":"diagnostic:validate-runtime","content":"ping","platform":"local","event_type":"diagnostic"}'
    printf '\n'
    curl_json "${HEALTH_URL}"
    printf '\n'
}

console_check() {
    print_heading "web console"
    local status
    status="$(curl -fsS -o /dev/null -w '%{http_code}' --max-time 15 "${CONSOLE_URL}")"
    printf '%s -> HTTP %s\n' "${CONSOLE_URL}" "${status}"
}

bot_log_check() {
    print_heading "bot logs"
    local log_file="${SOURCE_BOT_LOG_FILE}"
    if [[ ! -f "${log_file}" ]]; then
        log_file="${BOT_LOG_FILE_DEFAULT}"
    fi
    if [[ -f "${log_file}" ]]; then
        tail -n "${LINES:-80}" "${log_file}"
    else
        printf 'bot log missing: %s\n' "${log_file}"
    fi
}

check_all() {
    bot_status
    health_check
    glm_check
    console_check
    bot_log_check
}

restart_deployed() {
    require_file "${BOT_CTL}"
    "${BOT_CTL}" restart
    check_all
}

restart_source() {
    require_file "${BOT_CTL}"
    require_file "${SOURCE_BOT_BINARY}"

    "${BOT_CTL}" stop || true
    BOT_PID_FILE="${SOURCE_BOT_PID_FILE}" \
    BOT_LOG_FILE="${SOURCE_BOT_LOG_FILE}" \
    BOT_BINARY="${SOURCE_BOT_BINARY}" \
        "${BOT_CTL}" stop || true
    BOT_PID_FILE="${SOURCE_BOT_PID_FILE}" \
    BOT_LOG_FILE="${SOURCE_BOT_LOG_FILE}" \
    BOT_BINARY="${SOURCE_BOT_BINARY}" \
        "${BOT_CTL}" start
    check_all
}

command="${1:-check}"
case "${command}" in
    check)
        check_all
        ;;
    glm)
        glm_check
        ;;
    console)
        console_check
        ;;
    logs)
        bot_log_check
        ;;
    restart)
        restart_deployed
        ;;
    restart-source)
        restart_source
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

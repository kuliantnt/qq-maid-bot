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
LLM_CTL="${RUNTIME_DIR}/llmctl.sh"
GATEWAY_CTL="${RUNTIME_DIR}/gatewayctl.sh"
LLM_URL="${LLM_SERVER_URL:-http://127.0.0.1:${LLM_SERVER_PORT:-8787}}"
HEALTH_URL="${LLM_URL%/}/healthz"
RESPOND_URL="${LLM_URL%/}/v1/respond"
CONSOLE_URL="${LLM_URL%/}/console/"
SOURCE_GATEWAY_BINARY="${SOURCE_GATEWAY_BINARY:-${REPO_DIR}/target/debug/qq-maid-gateway-rs}"
SOURCE_GATEWAY_PID_FILE="${SOURCE_GATEWAY_PID_FILE:-${RUNTIME_DIR}/run/qq-maid-gateway-rs-source.pid}"
SOURCE_GATEWAY_LOG_FILE="${SOURCE_GATEWAY_LOG_FILE:-${RUNTIME_DIR}/logs/qq-maid-gateway-rs-source.log}"
GATEWAY_LOG_FILE_DEFAULT="${GATEWAY_LOG_FILE:-${RUNTIME_DIR}/logs/qq-maid-gateway-rs.log}"

usage() {
    cat <<'EOF'
Usage: validate-runtime.sh <command>

Commands:
  check             Check service status, LLM health, GLM upstream, console, and gateway logs
  glm              Run only the GLM/OpenAI-compatible upstream diagnostic
  console          Check only the web console route
  logs             Show recent gateway and LLM logs
  restart          Restart deployed LLM and gateway, then run check
  restart-source   Restart LLM and run source-built debug gateway, then run check

Environment overrides:
  QQ_MAID_RUNTIME_DIR       Runtime directory, default: runtime/
  LLM_SERVER_URL            LLM base URL, default: http://127.0.0.1:8787
  LINES                     Log lines to show, default: 80
  SOURCE_GATEWAY_BINARY     Debug/source gateway binary for restart-source
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

llm_status() {
    print_heading "service status"
    require_file "${LLM_CTL}"
    require_file "${GATEWAY_CTL}"
    "${LLM_CTL}" status
    "${GATEWAY_CTL}" status || true
    if [[ -f "${SOURCE_GATEWAY_PID_FILE}" ]]; then
        GATEWAY_PID_FILE="${SOURCE_GATEWAY_PID_FILE}" \
        GATEWAY_LOG_FILE="${SOURCE_GATEWAY_LOG_FILE}" \
        GATEWAY_BINARY="${SOURCE_GATEWAY_BINARY}" \
            "${GATEWAY_CTL}" status || true
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

gateway_log_check() {
    print_heading "gateway logs"
    local log_file="${SOURCE_GATEWAY_LOG_FILE}"
    if [[ ! -f "${log_file}" ]]; then
        log_file="${GATEWAY_LOG_FILE_DEFAULT}"
    fi
    if [[ -f "${log_file}" ]]; then
        tail -n "${LINES:-80}" "${log_file}"
    else
        printf 'gateway log missing: %s\n' "${log_file}"
    fi

    print_heading "LLM logs"
    local llm_log="${LLM_LOG_FILE:-${RUNTIME_DIR}/logs/qq-maid-llm.log}"
    if [[ -f "${llm_log}" ]]; then
        tail -n "${LINES:-80}" "${llm_log}"
    else
        printf 'LLM log missing: %s\n' "${llm_log}"
    fi
}

check_all() {
    llm_status
    health_check
    glm_check
    console_check
    gateway_log_check
}

restart_deployed() {
    require_file "${LLM_CTL}"
    require_file "${GATEWAY_CTL}"
    "${GATEWAY_CTL}" stop || true
    "${LLM_CTL}" restart
    "${GATEWAY_CTL}" start
    check_all
}

restart_source() {
    require_file "${LLM_CTL}"
    require_file "${GATEWAY_CTL}"
    require_file "${SOURCE_GATEWAY_BINARY}"

    "${GATEWAY_CTL}" stop || true
    GATEWAY_PID_FILE="${SOURCE_GATEWAY_PID_FILE}" \
    GATEWAY_LOG_FILE="${SOURCE_GATEWAY_LOG_FILE}" \
    GATEWAY_BINARY="${SOURCE_GATEWAY_BINARY}" \
        "${GATEWAY_CTL}" stop || true
    "${LLM_CTL}" restart
    GATEWAY_PID_FILE="${SOURCE_GATEWAY_PID_FILE}" \
    GATEWAY_LOG_FILE="${SOURCE_GATEWAY_LOG_FILE}" \
    GATEWAY_BINARY="${SOURCE_GATEWAY_BINARY}" \
        "${GATEWAY_CTL}" start
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
        gateway_log_check
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

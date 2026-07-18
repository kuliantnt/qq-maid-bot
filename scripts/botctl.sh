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

DEFAULT_BINARY="${RUNTIME_DIR}/qq-maid-bot"
if [[ ! -f "${DEFAULT_BINARY}" && -f "${DEFAULT_BINARY}.exe" ]]; then
    # Windows Release 在 Git Bash/MSYS2/Cygwin 下复用本控制脚本。
    DEFAULT_BINARY="${DEFAULT_BINARY}.exe"
fi
BINARY="${BOT_BINARY:-${DEFAULT_BINARY}}"
PID_FILE="${BOT_PID_FILE:-${RUNTIME_DIR}/run/qq-maid-bot.pid}"
LOG_FILE="${BOT_LOG_FILE:-${RUNTIME_DIR}/logs/qq-maid-bot.log}"
STOP_TIMEOUT_SECONDS="${BOT_STOP_TIMEOUT_SECONDS:-10}"
OBSOLETE_ENV_KEYS=(
    LLM_PROVIDER OPENAI_MODEL LLM_MODEL PRIVATE_LLM_MODEL GROUP_LLM_MODEL
    OPENAI_SEARCH_MODEL PRIVATE_OPENAI_SEARCH_MODEL GROUP_OPENAI_SEARCH_MODEL
    TITLE_MODEL MEMORY_MODEL COMPACT_MODEL TRANSLATION_MODEL
    DEEPSEEK_MODEL BIGMODEL_MODEL GEMINI_MODEL LLM_MAX_OUTPUT_TOKENS
    TOOL_CALLING_ENABLED TOOL_CALLING_GROUP_ENABLED TOOL_CALLING_MAX_ROUNDS
    TODO_MODEL MEMBER_ID_MAPPING_FILE
)

usage() {
    cat <<'EOF'
Usage: botctl.sh <command>

Commands:
  start     Start qq-maid-bot in the background
  run       Run qq-maid-bot in the foreground
  stop      Stop qq-maid-bot
  restart   Restart qq-maid-bot
  status    Show process status
  health    Request /healthz
  console   Show /console/ URL and HTTP status
  logs      Tail the log file

Environment overrides:
  BOT_BINARY     Path to qq-maid-bot executable
  BOT_ENV_FILE   Env file to load before starting
  BOT_PID_FILE   PID file path
  BOT_LOG_FILE   Log file path
  BOT_STOP_TIMEOUT_SECONDS  Seconds before forced stop (default: 10)
  QQ_MAID_RUNTIME_DIR  Runtime directory containing binary/config/logs
  LINES          Number of log lines for logs command
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

migrate_obsolete_env_config() {
    local env_file key tmp owner group mode backup joined
    local found=()
    env_file="$(resolve_env_file)" || return 0
    [[ -f "${env_file}" ]] || return 0
    if [[ -L "${env_file}" ]]; then
        echo "warning: skip obsolete env migration for symbolic link: ${env_file}" >&2
        return 0
    fi

    for key in "${OBSOLETE_ENV_KEYS[@]}"; do
        if grep -Eq "^[[:space:]]*(export[[:space:]]+)?${key}[[:space:]]*=" "${env_file}"; then
            found+=("${key}")
        fi
    done
    ((${#found[@]} > 0)) || return 0

    backup="${env_file}.bak.v0.20.$(date +%Y%m%d_%H%M%S).$$"
    cp -a -- "${env_file}" "${backup}"
    tmp="${env_file}.tmp.$$"
    owner="$(stat -c '%u' "${env_file}" 2>/dev/null || echo "")"
    group="$(stat -c '%g' "${env_file}" 2>/dev/null || echo "")"
    mode="$(stat -c '%a' "${env_file}" 2>/dev/null || echo "")"
    joined="$(IFS=:; echo "${OBSOLETE_ENV_KEYS[*]}")"
    awk -v removed_keys="${joined}" '
        BEGIN {
            count = split(removed_keys, values, ":")
            for (i = 1; i <= count; i++) removed[values[i]] = 1
        }
        {
            candidate = $0
            sub(/^[[:space:]]*/, "", candidate)
            sub(/^export[[:space:]]+/, "", candidate)
            if (match(candidate, /^[A-Za-z_][A-Za-z0-9_]*/)) {
                key = substr(candidate, RSTART, RLENGTH)
                rest = substr(candidate, RLENGTH + 1)
                if (removed[key] && rest ~ /^[[:space:]]*=/) next
            }
            print
        }
    ' "${env_file}" > "${tmp}"
    [[ -n "${owner}" && -n "${group}" ]] && chown "${owner}:${group}" "${tmp}" 2>/dev/null || true
    [[ -n "${mode}" ]] && chmod "${mode}" "${tmp}" 2>/dev/null || true
    mv -- "${tmp}" "${env_file}"

    echo "removed obsolete config keys: ${found[*]}"
    echo "pre-upgrade env backup: ${backup}"
    echo "Remove the same keys manually if systemd, Docker, or the host environment still injects them."
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

web_console_enabled() {
    local enabled
    enabled="$(printf '%s' "${WEB_CONSOLE_ENABLED:-true}" | tr '[:upper:]' '[:lower:]')"
    [[ "${enabled}" != "false" ]]
}

console_access_hint() {
    local url authority
    url="$(server_url)"
    authority="${url#*://}"
    authority="${authority%%/*}"
    case "${authority}" in
        0.0.0.0|0.0.0.0:*|'[::]'|'[::]':*|::* )
            echo "控制台监听于通配地址，请使用实际服务器地址或反向代理地址访问 /console/"
            ;;
        *)
            echo "浏览器打开 ${url%/}/console/"
            ;;
    esac
}

bootstrap_token_purpose() {
    local token_file="$1"
    [[ -f "${token_file}" && ! -L "${token_file}" ]] || return 1
    LC_ALL=C awk -F: '
        NR == 1 && NF == 3 && $2 ~ /^[0-9]+$/ && length($3) > 0 &&
        ($1 == "qq-maid-bootstrap-v1" || $1 == "qq-maid-password-reset-v1") {
            purpose = $1
            valid = 1
        }
        NR > 1 { valid = 0 }
        END { if (valid && NR == 1) print purpose }
    ' "${token_file}" 2>/dev/null
}

show_bootstrap_guidance() {
    local token_file="$1" purpose access_hint
    web_console_enabled || return 0
    # 没有 bootstrap token 是正常的稳定运行状态，不能让解析失败影响 start 的退出码。
    purpose="$(bootstrap_token_purpose "${token_file}")" || return 0
    [[ -n "${purpose}" ]] || return 0
    access_hint="$(console_access_hint)"

    echo ""
    if [[ "${purpose}" == "qq-maid-bootstrap-v1" ]]; then
        echo "--- 首次配置 ---"
        echo "v0.20 起可以通过网页完成配置，不再必须编辑 config/.env："
        echo "  1. 读取服务器本地 config/secrets/bootstrap.token 中的一次性令牌"
        echo "  2. ${access_hint}"
        echo "  3. 用令牌建立部署管理员，按向导保存 Provider、平台入口和功能开关"
    else
        echo "--- 密码重置待完成 ---"
        echo "部署管理员密码重置令牌已经生成："
        echo "  1. 读取服务器本地 config/secrets/bootstrap.token 中的一次性令牌"
        echo "  2. ${access_hint}"
        echo "  3. 在登录页完成密码重置；完成后旧管理员会话将失效"
    fi
    echo ""
    echo "请勿输出、转发或长期保留令牌。"
    echo "更多：https://github.com/kuliantnt/qq-maid-bot/wiki/配置中心"
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
    migrate_obsolete_env_config
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

    # 只解析 token purpose，不把令牌原文带入命令输出。
    local bootstrap_token_file="${RUNTIME_DIR}/config/secrets/bootstrap.token"
    show_bootstrap_guidance "${bootstrap_token_file}"
}

run_foreground() {
    [[ -f "${BINARY}" ]] || die "executable not found: ${BINARY}"
    if [[ ! -x "${BINARY}" ]]; then
        chmod +x "${BINARY}"
    fi

    mkdir -p "$(dirname -- "${PID_FILE}")" "$(dirname -- "${LOG_FILE}")"
    migrate_obsolete_env_config
    load_env
    export RUST_LOG="${RUST_LOG:-info,qq_maid_gateway_rs=debug,qq_maid_core=info,tower_http=info}"

    # systemd 等外部进程管理器需要前台进程；后台启动仍走 start。
    cd "${RUNTIME_DIR}"
    exec "${BINARY}"
}

stop() {
    local pid
    [[ "${STOP_TIMEOUT_SECONDS}" =~ ^[0-9]+$ ]] || die "BOT_STOP_TIMEOUT_SECONDS must be a non-negative integer"
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

    kill "${pid}" || die "failed to stop qq-maid-bot, pid=${pid}"
    local waited=0
    while kill -0 "${pid}" 2>/dev/null; do
        if (( waited >= STOP_TIMEOUT_SECONDS )); then
            kill -9 "${pid}" || die "failed to force stop qq-maid-bot, pid=${pid}"
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done

    # Windows 原生进程被 TerminateProcess 后，MSYS 的进程表可能短暂仍可见，等待其完成清理。
    local force_waited=0
    while kill -0 "${pid}" 2>/dev/null && (( force_waited < 5 )); do
        sleep 1
        force_waited=$((force_waited + 1))
    done
    if kill -0 "${pid}" 2>/dev/null; then
        die "qq-maid-bot is still running after forced stop, pid=${pid}"
    fi

    rm -f "${PID_FILE}"
    echo "qq-maid-bot stopped"
}

status() {
    if is_running; then
        echo "qq-maid-bot is running, pid=$(read_pid)"
        echo "health: $(server_url | sed 's:/*$::')/healthz"
    else
        echo "qq-maid-bot is stopped"
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

console() {
    load_env
    command -v curl >/dev/null 2>&1 || die "curl is required for console"
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
    run)
        run_foreground
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
    health)
        health
        ;;
    console)
        console
        ;;
    -h|--help|help|"")
        usage
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

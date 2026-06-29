#!/usr/bin/env bash
# qq-maid-healthcheck.sh —— qq-maid-bot 进程级健康与资源占用诊断脚本
#
# 用途：
#   定位正在运行的 qq-maid-bot 进程，输出其基本信息、内存、状态、IO、
#   文件描述符、连接情况和资源上限，便于上线巡检和排障。
#
# 与 botctl.sh / diagnose-network.sh 共用运行目录语义：
#   * 源码仓库中脚本位于 scripts/，默认运行目录为仓库下的 runtime/；
#   * release 包中脚本位于运行目录根部，默认运行目录即为脚本所在目录。
#
# 不会读取或打印真实 .env 内容、QQ 事件、openid、Authorization 等敏感信息；
# 连接信息仅展示本地/远端地址与端口、协议方向。

set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_NAME="$(basename -- "${BASH_SOURCE[0]}")"

# 反射式运行目录解析：脚本被改名安装到运行目录根部时，优先认定该目录为运行目录。
if [[ "${SCRIPT_NAME}" == "qq-maid-healthcheck.sh" && -f "${SCRIPT_DIR}/qq-maid-bot" ]]; then
    DEFAULT_RUNTIME_DIR="${SCRIPT_DIR}"
else
    DEFAULT_RUNTIME_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/../runtime" && pwd)"
fi
RUNTIME_DIR="${QQ_MAID_RUNTIME_DIR:-${DEFAULT_RUNTIME_DIR}}"

# 二进制名：用于 PID 文件回退与进程匹配。允许通过 BOT_BINARY 覆盖完整路径。
DEFAULT_BINARY="${RUNTIME_DIR}/qq-maid-bot"
BINARY="${BOT_BINARY:-${DEFAULT_BINARY}}"
# 进程匹配模式：默认按二进制名匹配，避免误伤其它同名进程时可改成完整命令行片段。
PROC_MATCH="${HEALTHCHECK_PROC_MATCH:-$(basename -- "${BINARY}")}"

# PID 文件：优先复用 botctl.sh 写入的 PID 文件，缺失时回退到 pgrep 查找。
PID_FILE="${BOT_PID_FILE:-${RUNTIME_DIR}/run/qq-maid-bot.pid}"

# 健康端点：与 botctl.sh 一致，兼容 LLM_SERVER_URL / HOST / PORT 三个变量。
SERVER_HOST="${LLM_SERVER_HOST:-127.0.0.1}"
SERVER_PORT="${LLM_SERVER_PORT:-8787}"
SERVER_URL="${LLM_SERVER_URL:-http://${SERVER_HOST}:${SERVER_PORT}}"

# 连接查看命令：root 直接 ss，非 root 优先尝试 sudo ss，再降级普通 ss / netstat。
USE_SUDO_FOR_SS=0
if [[ "$(id -u)" -ne 0 ]]; then
    if command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
        USE_SUDO_FOR_SS=1
    fi
fi

usage() {
    cat <<'EOF'
Usage: qq-maid-healthcheck.sh [options]

选项：
  -h, --help         显示帮助
  --health           仅查询 HTTP 健康端点 (/healthz) 并打印状态码
  --no-proc          跳过进程资源采集，仅做基础进程定位与健康检查
  --pid <PID>        显式指定目标进程 PID，绕过 PID 文件与 pgrep 查找

环境变量覆盖：
  BOT_BINARY          二进制路径（默认 runtime/qq-maid-bot）
  BOT_PID_FILE        PID 文件路径（默认 runtime/run/qq-maid-bot.pid）
  HEALTHCHECK_PROC_MATCH  pgrep 进程匹配模式（默认二进制名）
  QQ_MAID_RUNTIME_DIR 运行目录
  LLM_SERVER_URL      健康端点基址
  LLM_SERVER_HOST     健康端点主机（默认 127.0.0.1）
  LLM_SERVER_PORT     健康端点端口（默认 8787）
EOF
}

# 参数解析
OPT_HEALTH_ONLY=0
OPT_NO_PROC=0
OPT_PID=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        --health)
            OPT_HEALTH_ONLY=1
            ;;
        --no-proc)
            OPT_NO_PROC=1
            ;;
        --pid)
            shift
            [[ $# -gt 0 ]] || { echo "error: --pid 需要参数" >&2; exit 2; }
            OPT_PID="$1"
            ;;
        *)
            echo "error: 未知参数: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

# 解析目标 PID：显式指定 > PID 文件 > pgrep 匹配。
# PID 文件可能存在残留（进程已退出），需配合存活校验。
resolve_pid() {
    local pid

    if [[ -n "${OPT_PID}" ]]; then
        if [[ "${OPT_PID}" =~ ^[0-9]+$ ]] && kill -0 "${OPT_PID}" 2>/dev/null; then
            printf '%s\n' "${OPT_PID}"
            return 0
        fi
        echo "warning: --pid ${OPT_PID} 对应进程不存在" >&2
        return 1
    fi

    if [[ -f "${PID_FILE}" ]]; then
        pid="$(tr -d '[:space:]' < "${PID_FILE}")"
        if [[ "${pid}" =~ ^[0-9]+$ ]] && kill -0 "${pid}" 2>/dev/null; then
            printf '%s\n' "${pid}"
            return 0
        fi
        # PID 文件残留但进程不在，继续回退到 pgrep，而不是直接判定未运行。
        echo "warning: PID 文件 ${PID_FILE} 残留(PID=${pid:-空})但进程不存在，回退到 pgrep" >&2
    fi

    # pgrep 匹配进程可执行名；-n 取最新启动的一个，避免多实例时输出不确定。
    if pid="$(pgrep -n -f "${PROC_MATCH}" 2>/dev/null)" && [[ -n "${pid}" ]]; then
        printf '%s\n' "${pid}"
        return 0
    fi

    return 1
}

# 健康端点查询：复用 curl，缺失时回退 wget，均无则跳过。
check_health() {
    local url status
    url="${SERVER_URL%/}/healthz"

    if command -v curl >/dev/null 2>&1; then
        status="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 8 "${url}" 2>/dev/null || printf 'ERR')"
        printf '  %s -> HTTP %s\n' "${url}" "${status}"
        if [[ "${status}" == "200" ]]; then
            return 0
        fi
        return 1
    fi

    if command -v wget >/dev/null 2>&1; then
        if wget -qO- --timeout=8 --tries=1 "${url}" >/dev/null 2>&1; then
            printf '  %s -> HTTP 200\n' "${url}"
            return 0
        fi
        printf '  %s -> HTTP ERR\n' "${url}"
        return 1
    fi

    printf '  %s -> SKIPPED (curl/wget 均不可用)\n' "${url}"
    return 1
}

# 连接查看：root 用 ss，非 root 在能免密 sudo 时用 sudo ss，否则降级 ss / netstat。
list_connections() {
    local pid="$1"
    local out

    if command -v ss >/dev/null 2>&1; then
        if (( USE_SUDO_FOR_SS == 1 )); then
            out="$(sudo -n ss -tpn 2>/dev/null | grep "pid=${pid}," || true)"
        else
            out="$(ss -tpn 2>/dev/null | grep "pid=${pid}," || true)"
        fi
    elif command -v netstat >/dev/null 2>&1; then
        # netstat 无进程列，只能按进程已建立的 fd-inode 粗略对应，这里仅展示监听与已建立。
        out="$(netstat -tpn 2>/dev/null | grep -E "(ESTABLISHED|LISTEN)" || true)"
    else
        printf '  (ss/netstat 均不可用)\n'
        return
    fi

    if [[ -z "${out}" ]]; then
        printf '  (无连接记录)\n'
        return
    fi

    # 只输出关键列，避免打印完整 raw（netstat 仍含部分字段，但不涉及 QQ 事件/openid）。
    printf '%s\n' "${out}" | awk '{ printf "  %s %s %s\n", $1, $5, $6 }'
}

main() {
    printf 'QQ Maid healthcheck\n'
    printf '  runtime_dir: %s\n' "${RUNTIME_DIR}"
    printf '  binary:      %s\n' "${BINARY}"
    printf '  pid_file:    %s\n\n' "${PID_FILE}"

    local pid
    if ! pid="$(resolve_pid)"; then
        printf '===== STATUS =====\n'
        printf 'qq-maid-bot 未运行（无 PID 文件或匹配进程）\n\n'
        printf '===== HEALTH =====\n'
        check_health || true
        exit 0
    fi

    printf '===== STATUS =====\n'
    printf 'qq-maid-bot is running, pid=%s\n' "${pid}"
    if [[ -f "${BINARY}" ]]; then
        printf 'binary present: %s\n' "${BINARY}"
    else
        printf 'binary missing (可能使用其它路径启动): %s\n' "${BINARY}" >&2
    fi
    printf '\n'

    printf '===== HEALTH =====\n'
    check_health || true
    printf '\n'

    if (( OPT_HEALTH_ONLY == 1 )) || (( OPT_NO_PROC == 1 )); then
        exit 0
    fi

    # 以下为进程级资源采集，依赖 /proc，仅适用于 Linux。
    if [[ ! -d "/proc/${pid}" ]]; then
        printf '进程 /proc/%s 不可读（非 Linux 或权限不足），跳过资源采集\n' "${pid}"
        exit 0
    fi

    printf '===== BASIC =====\n'
    ps -p "${pid}" -o pid,ppid,user,stat,lstart,etime,%cpu,%mem,rss,vsz,nlwp,cmd || true
    printf '\n'

    printf '===== MEMORY =====\n'
    cat "/proc/${pid}/smaps_rollup" 2>/dev/null \
        | grep -E 'Rss:|Pss:|Private_|Anonymous:|Swap:' || true
    printf '\n'

    printf '===== STATUS =====\n'
    grep -E 'State|Threads|VmPeak|VmSize|VmRSS|RssAnon|RssFile|VmSwap' "/proc/${pid}/status" 2>/dev/null || true
    printf '\n'

    printf '===== IO =====\n'
    cat "/proc/${pid}/io" 2>/dev/null || true
    printf '\n'

    printf '===== FILE DESCRIPTORS =====\n'
    printf 'FD count: %s\n' "$(find "/proc/${pid}/fd" -maxdepth 1 -type l 2>/dev/null | wc -l)"
    printf '\n'

    printf '===== CONNECTIONS =====\n'
    list_connections "${pid}"
    printf '\n'

    printf '===== LIMITS =====\n'
    grep -E 'Max open files|Max processes|Max address space' "/proc/${pid}/limits" 2>/dev/null || true
}

main
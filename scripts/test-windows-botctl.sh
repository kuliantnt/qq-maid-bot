#!/usr/bin/env bash
set -euo pipefail

native_binary="${1:?usage: test-windows-botctl.sh /path/to/qq-maid-bot.exe}"
repo_dir="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
runtime_dir="$(mktemp -d)"
trap '[[ -f "${runtime_dir}/run/qq-maid-bot.pid" ]] && QQ_MAID_RUNTIME_DIR="${runtime_dir}" bash "${runtime_dir}/botctl.sh" stop >/dev/null 2>&1 || true; rm -rf "${runtime_dir}"' EXIT

mkdir -p "${runtime_dir}/config"
cp "${repo_dir}/scripts/botctl.sh" "${runtime_dir}/botctl.sh"
cp "${native_binary}" "${runtime_dir}/qq-maid-bot.exe"

assert_stopped() {
    local pid="$1"
    if kill -0 "${pid}" 2>/dev/null; then
        echo "process is still running: ${pid}" >&2
        return 1
    fi
    [[ ! -e "${runtime_dir}/run/qq-maid-bot.pid" ]]
}

start_output="$(QQ_MAID_RUNTIME_DIR="${runtime_dir}" bash "${runtime_dir}/botctl.sh" start)"
[[ "${start_output}" == *"qq-maid-bot started"* ]]
pid="$(tr -d '[:space:]' < "${runtime_dir}/run/qq-maid-bot.pid")"
[[ "${pid}" =~ ^[0-9]+$ ]]
kill -0 "${pid}"
status_output="$(QQ_MAID_RUNTIME_DIR="${runtime_dir}" bash "${runtime_dir}/botctl.sh" status)"
[[ "${status_output}" == *"qq-maid-bot is running, pid=${pid}"* ]]
grep -F "windows smoke started" "${runtime_dir}/logs/qq-maid-bot.log"
QQ_MAID_RUNTIME_DIR="${runtime_dir}" bash "${runtime_dir}/botctl.sh" stop
assert_stopped "${pid}"

# 保留真实 Windows 进程，但拦截第一次普通停止信号，确定性覆盖超时后的 kill -9 分支。
QQ_MAID_RUNTIME_DIR="${runtime_dir}" bash "${runtime_dir}/botctl.sh" start
pid="$(tr -d '[:space:]' < "${runtime_dir}/run/qq-maid-bot.pid")"
kill() {
    if [[ "${1:-}" == "-0" ]]; then
        builtin kill "$@"
    elif [[ "${1:-}" == "-9" ]]; then
        printf 'forced\n' > "${runtime_dir}/forced-stop"
        builtin kill "$@"
    else
        return 0
    fi
}
export -f kill
export runtime_dir
BOT_STOP_TIMEOUT_SECONDS=1 QQ_MAID_RUNTIME_DIR="${runtime_dir}" bash "${runtime_dir}/botctl.sh" stop
[[ -f "${runtime_dir}/forced-stop" ]]
assert_stopped "${pid}"

echo "Windows botctl smoke test passed"

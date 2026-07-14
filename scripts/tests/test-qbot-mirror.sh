#!/usr/bin/env bash
# =============================================================================
# test-qbot-mirror.sh — qbot.sh GitHub 镜像探测逻辑单元测试
#
# 覆盖范围（PR #430 收窄后）：
#   - github_url_for_prefix        prefix 拼接（proxy 型）
#   - github_prefix_label          直连 / 前缀标签
#   - github_accel_prefixes        去重与尾部斜杠规范化
#   - check_github_direct          官方直连判断（基于 probe 结果）
#   - bootstrap_github_network     短路与 opt-in 分支：
#       1) 已显式配 GITHUB_ACCEL_PROXY        → 跳过
#       2) QBOT_SKIP_MIRROR_AUTO=1            → 跳过
#       3) QBOT_ENABLE_MIRROR_AUTO 未设       → 默认跳过（不改动）
#       4) opt-in + 官方直连可用              → 不设置 proxy
#       5) opt-in + 直连失败 + 有可用候选      → 仅当前进程设 GITHUB_ACCEL_PROXY
#       6) opt-in + 直连失败 + 无可用候选      → 不设置、不崩溃
#
# 设计：
#   - 通过 source qbot.sh 加载函数；qbot.sh 已有 source guard，不会触发命令分发。
#   - 网络相关函数（probe_github_prefix_ms）被本测试用同名的 mock 覆盖，
#     避免真实联网、保证确定性。
#   - 不引入外部测试框架（bats/shunit2），自包含断言，符合 bash -n 即可运行。
#
# 用法：
#   bash scripts/tests/test-qbot-mirror.sh
#   （可选）VERBOSE=1 时打印每条用例的执行细节
# =============================================================================
set -uo pipefail

# ---------- 定位被测脚本 ----------
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
QBOT_SH="$(cd -- "${SCRIPT_DIR}/../.." && pwd)/qbot.sh"
[[ -f "${QBOT_SH}" ]] || { echo "找不到 qbot.sh: ${QBOT_SH}" >&2; exit 1; }

# ---------- 加载被测代码（仅函数，触发 source guard） ----------
# shellcheck disable=SC1090
source "${QBOT_SH}"

# qbot.sh 内部会 set -euo pipefail；source 后在当前 shell 生效。
# 测试需要收集所有断言结果而非遇错即停，这里关闭 errexit（保留 nounset/pipefail）。
set +e

# ui_* 依赖 UI_* 颜色变量（由 init_ui 在正常启动时设置）；source 模式下未初始化，
# 这里兜底为空，确保 ui_note/ui_warn 在测试中不报 unbound variable。
UI_DIM="" UI_RESET="" UI_YELLOW="" UI_RED="" UI_GREEN="" UI_BLUE="" UI_CYAN=""

# ---------- 极简测试框架 ----------
PASS=0
FAIL=0
FAILED_NAMES=()

assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [[ "${expected}" == "${actual}" ]]; then
        PASS=$((PASS + 1))
        [[ "${VERBOSE:-0}" == "1" ]] && echo "  ✓ ${desc}"
    else
        FAIL=$((FAIL + 1))
        FAILED_NAMES+=("${desc}")
        echo "  ✗ ${desc}"
        echo "      expected: [${expected}]"
        echo "      actual:   [${actual}]"
    fi
}

assert_true() {
    local desc="$1" val="$2"
    if [[ "${val}" == "0" || "${val}" == "true" ]]; then
        assert_eq "${desc}" "0" "0"
    else
        assert_eq "${desc}" "0" "non-zero(${val})"
    fi
}

assert_false() {
    local desc="$1" val="$2"
    if [[ -z "${val}" || "${val}" == "1" || "${val}" == "false" ]]; then
        assert_eq "${desc}" "0" "0"
    else
        assert_eq "${desc}" "0" "non-zero(${val})"
    fi
}

# 每个用例前重置会被 bootstrap 改动的全局状态（注意：只清空，不要 unset，
# 否则 set -u 下 qbot.sh 内部引用 GITHUB_ACCEL_PROXY 会报 unbound variable）
reset_state() {
    GITHUB_ACCEL_PROXY=""
    GITHUB_ACCEL_PROXIES=""
    QBOT_SKIP_MIRROR_AUTO=0
    QBOT_ENABLE_MIRROR_AUTO=0
}

# mock 网络探测：通过 MAP 指定 "domain=>延迟ms"，未列出=不可用(999999)
setup_probe_mock() {
    # $1 为 "domain:ms domain:ms ..." 形式；空表示全部不可用
    PROBE_MAP="$1"
    probe_github_prefix_ms() {
        local prefix="$1"
        if [[ -z "${prefix}" ]]; then
            # 官方直连：MAP 里 special key "__DIRECT__"
            local d
            d="${PROBE_MAP##*__DIRECT__:}"
            if [[ "${PROBE_MAP}" == *"__DIRECT__:0"* ]]; then
                echo 0
            else
                echo 999999
            fi
            return
        fi
        # 从 prefix 提取域名：https://<domain>/
        local domain="${prefix#https://}"
        domain="${domain%%/*}"
        local token
        token="$(echo " ${PROBE_MAP} " | sed -nE "s/.* ${domain}:([0-9]+) .*/\1/p")"
        if [[ -n "${token}" ]]; then
            echo "${token}"
        else
            echo 999999
        fi
    }
}

echo "== test-qbot-mirror: qbot.sh GitHub 镜像探测逻辑 =="

# ---------------------------------------------------------------------------
echo "[1] github_url_for_prefix"
assert_eq "直连 prefix 空返回原 URL" \
    "https://github.com/kuliantnt/qq-maid-bot/releases" \
    "$(github_url_for_prefix "" "https://github.com/kuliantnt/qq-maid-bot/releases")"
assert_eq "proxy prefix 拼接正确" \
    "https://ghproxy.net/https://github.com/kuliantnt/qq-maid-bot/releases" \
    "$(github_url_for_prefix "https://ghproxy.net/" "https://github.com/kuliantnt/qq-maid-bot/releases")"

# ---------------------------------------------------------------------------
echo "[2] github_prefix_label"
assert_eq "空 prefix 显示直连" "直连 GitHub" "$(github_prefix_label "")"
assert_eq "prefix 显示域名" "https://ghproxy.net/" "$(github_prefix_label "https://ghproxy.net/")"

# ---------------------------------------------------------------------------
echo "[3] github_accel_prefixes 规范化与去重"
GITHUB_ACCEL_PROXY="https://ghproxy.net"
GITHUB_ACCEL_PROXIES=""
out="$(github_accel_prefixes | grep -v '^$' | tr '\n' ',')"
assert_eq "单 proxy 规范化尾部斜杠" "https://ghproxy.net/," "${out}"
GITHUB_ACCEL_PROXY=""
GITHUB_ACCEL_PROXIES="https://a.com/ https://a.com"
out="$(github_accel_prefixes | grep -v '^$' | tr '\n' ',')"
assert_eq "多候选去重并规范斜杠" "https://a.com/," "${out}"
GITHUB_ACCEL_PROXIES=""

# check_github_direct 通过退出码表达真假，这里包一层取 $?
direct_result() {
    if check_github_direct "https://github.com/x/releases"; then echo 0; else echo 1; fi
}

# ---------------------------------------------------------------------------
echo "[4] check_github_direct"
reset_state
setup_probe_mock "__DIRECT__:0"
assert_eq "官方直连可用返回 true" "0" "$(direct_result)"
setup_probe_mock "__DIRECT__:999999"
assert_eq "官方直连不可用返回 false" "1" "$(direct_result)"

# ---------------------------------------------------------------------------
echo "[5] bootstrap_github_network 短路分支"
# 5.1 已显式配置 proxy → 跳过，不探测不修改
reset_state
setup_probe_mock "__DIRECT__:999999 ghproxy.net:50"
GITHUB_ACCEL_PROXY="https://ghproxy.net/"
QBOT_ENABLE_MIRROR_AUTO=1
bootstrap_github_network
assert_eq "已配 proxy 时 GITHUB_ACCEL_PROXY 不被覆盖" "https://ghproxy.net/" "${GITHUB_ACCEL_PROXY}"

# 5.2 QBOT_SKIP_MIRROR_AUTO=1 → 跳过
reset_state
setup_probe_mock "__DIRECT__:999999 ghproxy.net:50"
QBOT_SKIP_MIRROR_AUTO=1
QBOT_ENABLE_MIRROR_AUTO=1
bootstrap_github_network
assert_eq "SKIP 时未设置 proxy" "" "${GITHUB_ACCEL_PROXY:-}"

# 5.3 默认未启用 → 即使直连失败也不自动设置（opt-in 要求）
reset_state
setup_probe_mock "__DIRECT__:999999 ghproxy.net:50"
bootstrap_github_network
assert_eq "默认未 opt-in 时不设置 proxy" "" "${GITHUB_ACCEL_PROXY:-}"

# 5.4 opt-in + 官方直连可用 → 不设置 proxy
reset_state
setup_probe_mock "__DIRECT__:0 ghproxy.net:50"
QBOT_ENABLE_MIRROR_AUTO=1
bootstrap_github_network
assert_eq "opt-in 但直连可用时不设置 proxy" "" "${GITHUB_ACCEL_PROXY:-}"

# 5.5 opt-in + 直连失败 + 有可用候选 → 设 GITHUB_ACCEL_PROXY
reset_state
setup_probe_mock "__DIRECT__:999999 ghproxy.net:50 gh-proxy.com:20"
QBOT_ENABLE_MIRROR_AUTO=1
bootstrap_github_network
assert_eq "opt-in 且有候选时设置 proxy(选最快)" "https://gh-proxy.com/" "${GITHUB_ACCEL_PROXY:-}"

# 5.6 opt-in + 直连失败 + 无可用候选 → 不设置、不崩溃
reset_state
setup_probe_mock "__DIRECT__:999999"
QBOT_ENABLE_MIRROR_AUTO=1
bootstrap_github_network; rc=$?
assert_eq "无候选时不崩溃(rc=0)" "0" "${rc}"
assert_eq "无候选时不设置 proxy" "" "${GITHUB_ACCEL_PROXY:-}"

# ---------------------------------------------------------------------------
echo ""
echo "== 结果: PASS=${PASS} FAIL=${FAIL} =="
if [[ "${FAIL}" -gt 0 ]]; then
    echo "失败用例:" >&2
    if [[ ${#FAILED_NAMES[@]} -gt 0 ]]; then
        printf '  - %s\n' "${FAILED_NAMES[@]}" >&2
    fi
    exit 1
fi
echo "全部通过"

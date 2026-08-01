#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "${REPO_DIR}/scripts/qbot.sh"
# 安装器在解压 Release 后加载同一内部模块；测试直接加载以覆盖迁移函数。
source "${REPO_DIR}/scripts/lib/agent-config.sh"

assert_target() {
    local system="$1"
    local fixture_arch="$2"
    local expected="$3"
    uname() {
        [[ "${1:-}" == "-s" ]] && echo "${system}" || echo "${fixture_arch}"
    }
    local actual
    actual="$(detect_target)"
    [[ "${actual}" == "${expected}" ]] || {
        echo "target mismatch: ${system}/${fixture_arch}: expected ${expected}, got ${actual}" >&2
        return 1
    }
}

assert_target Linux x86_64 linux-x86_64
assert_target Linux aarch64 linux-aarch64
assert_target Darwin x86_64 macos-x86_64
assert_target Darwin arm64 macos-aarch64

version_marker="${TMPDIR:-/tmp}/qbot-agent-version-marker-$$"
agent_config_reset_required v0.20.1 v0.20.2 "${version_marker}"
agent_config_reset_required v0.20.1 v0.21.0 "${version_marker}"
! agent_config_reset_required v0.20.2 v0.20.3 "${version_marker}"
! agent_config_reset_required v0.20.3 v0.21.0 "${version_marker}"

# Unix 安装器不得再包含 Windows target、ZIP 或原生 Windows 二进制分支。
if rg -n 'MINGW|MSYS|CYGWIN|windows-(x86_64|aarch64)|\.zip|qq-maid-bot\.exe' \
    "${REPO_DIR}/scripts/qbot.sh" >/dev/null; then
    echo "scripts/qbot.sh unexpectedly contains Windows-specific logic" >&2
    exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

# 执行前后用户 shell、Git 与系统全局配置必须保持不变（严禁改 .bashrc/.zshrc/.profile/.gitconfig 等）。
config_snapshot="${tmp_dir}/config-snapshot"
mkdir -p "${config_snapshot}"
for snapshot_file in .bashrc .zshrc .profile .gitconfig; do
    if [[ -f "${HOME}/${snapshot_file}" ]]; then
        cp "${HOME}/${snapshot_file}" "${config_snapshot}/${snapshot_file}"
    else
        : > "${config_snapshot}/${snapshot_file}.absent"
    fi
done
git config --global --list > "${config_snapshot}/git-global-config" 2>/dev/null || true

APP_DIR="${tmp_dir}/web-choice"
mkdir -p "${APP_DIR}/config"
printf '%s\n' 'WEB_CONSOLE_ENABLED=true' > "${APP_DIR}/config/.env"
configure_install_web_console false 0 >/dev/null
[[ "$(get_real_env_var WEB_CONSOLE_ENABLED)" == "false" ]]
# 显式参数允许重复安装时调整；未显式选择则保留已有配置。
configure_install_web_console true 1 >/dev/null
[[ "$(get_real_env_var WEB_CONSOLE_ENABLED)" == "true" ]]
configure_install_web_console "" 1 >/dev/null
[[ "$(get_real_env_var WEB_CONSOLE_ENABLED)" == "true" ]]

APP_DIR="${tmp_dir}/web-choice-noninteractive"
mkdir -p "${APP_DIR}/config"
printf '%s\n' 'WEB_CONSOLE_ENABLED=true' > "${APP_DIR}/config/.env"
QBOT_INSTALL_WEB_CONSOLE=false configure_install_web_console "" 0 >/dev/null
[[ "$(get_real_env_var WEB_CONSOLE_ENABLED)" == "false" ]]

APP_DIR="${tmp_dir}/web-search-migration"
mkdir -p "${APP_DIR}/config"
web_search_agent="${APP_DIR}/config/agent.toml"
printf '%s\n' \
    'version = 1' \
    '' \
    '[search_routes.private_search]' \
    'model = "gpt-search"' > "${web_search_agent}"
output="$(migrate_agent_web_search_config)"
grep -Fqx '[tools.web_search]' "${web_search_agent}"
grep -Fqx '[tools.web_search.routes.private_search]' "${web_search_agent}"
! grep -Fqx '[search_routes.private_search]' "${web_search_agent}"
grep -Fqx '[search_routes.private_search]' "${web_search_agent}.old"
[[ "${output}" == *"旧配置备份: ${web_search_agent}.old"* ]]
migrate_agent_web_search_config
[[ ! -e "${web_search_agent}.old.1" ]]

agent_template="${tmp_dir}/agent-template.toml"
printf '%s\n' 'version = 1' '[scenes.private]' 'enabled_tools = ["new_tool"]' > "${agent_template}"

agent_yes="${tmp_dir}/agent-yes.toml"
printf '%s\n' 'version = 1' 'custom = "keep-before-replacement"' > "${agent_yes}"
output="$(upgrade_agent_config_from_release "${agent_yes}" "${agent_template}")"
cmp -s "${agent_yes}" "${agent_template}"
grep -Fqx 'custom = "keep-before-replacement"' "${agent_yes}.old"
[[ "${output}" == *"旧配置备份: ${agent_yes}.old"* ]]
[[ "${output}" == *"Provider、模型路线、Scene 和工具白名单"* ]]

agent_collision="${tmp_dir}/agent-collision.toml"
printf '%s\n' 'current-old-config' > "${agent_collision}"
printf '%s\n' 'earlier-backup' > "${agent_collision}.old"
upgrade_agent_config_from_release "${agent_collision}" "${agent_template}" >/dev/null
grep -Fqx 'earlier-backup' "${agent_collision}.old"
grep -Fqx 'current-old-config' "${agent_collision}.old.1"
cmp -s "${agent_collision}" "${agent_template}"

agent_failure="${tmp_dir}/agent-failure.toml"
printf '%s\n' 'original-must-survive' > "${agent_failure}"
mv_calls=0
# shellcheck disable=SC2317 # 测试通过同名函数模拟第二次 mv 失败。
mv() {
    mv_calls=$((mv_calls + 1))
    if ((mv_calls == 2)); then
        return 1
    fi
    command mv "$@"
}
set +e
failure_output="$(replace_agent_config_from_release "${agent_failure}" "${agent_template}" 2>&1)"
failure_status=$?
set -e
unset -f mv
[[ "${failure_status}" -ne 0 ]]
grep -Fqx 'original-must-survive' "${agent_failure}"
[[ ! -e "${agent_failure}.old" ]]
if compgen -G "${tmp_dir}/.agent.toml.new.*" >/dev/null; then
    echo "agent replacement left a temporary file" >&2
    exit 1
fi
[[ "${failure_output}" == *"已恢复原文件"* ]]

agent_noninteractive="${tmp_dir}/agent-noninteractive.toml"
printf '%s\n' 'noninteractive-must-update' > "${agent_noninteractive}"
output="$(upgrade_agent_config_from_release "${agent_noninteractive}" "${agent_template}" < /dev/null)"
cmp -s "${agent_noninteractive}" "${agent_template}"
grep -Fqx 'noninteractive-must-update' "${agent_noninteractive}.old"
[[ "${output}" == *"自动备份并更新"* ]]

# 两个升级入口共享 marker：任一入口完成迁移后，另一入口不得再次覆盖用户配置。
mixed_app="${tmp_dir}/mixed-upgrade"
APP_DIR="${mixed_app}"
mkdir -p "${APP_DIR}/config"
mixed_agent="${APP_DIR}/config/agent.toml"
mixed_marker="${APP_DIR}/config/.agent-config-v0.20.2"
printf '%s\n' 'before-updater' > "${mixed_agent}"
upgrade_agent_config_from_release "${mixed_agent}" "${agent_template}" >/dev/null
mark_agent_config_migration_complete v0.20.1 v0.20.2
[[ -f "${mixed_marker}" ]]
printf '%s\n' 'user-config-after-updater' > "${mixed_agent}"
if [[ ! -e "${mixed_marker}" ]]; then
    cp "${agent_template}" "${mixed_agent}"
fi
grep -Fqx 'user-config-after-updater' "${mixed_agent}"

printf '%s\n' 'user-config-after-remote' > "${mixed_agent}"
! agent_config_reset_required v0.20.1 v0.20.2 "${mixed_marker}"
grep -Fqx 'user-config-after-remote' "${mixed_agent}"
grep -Fqx 'marker=config/.agent-config-v0.20.2' "${REPO_DIR}/scripts/deploy-remote.sh"

current_version_app="${tmp_dir}/current-version"
APP_DIR="${current_version_app}"
mkdir -p "${APP_DIR}/config"
mark_agent_config_migration_complete v0.20.3 v0.21.0
[[ -f "${APP_DIR}/config/.agent-config-v0.20.2" ]]

failed_marker_app="${tmp_dir}/failed-migration"
APP_DIR="${failed_marker_app}"
mkdir -p "${APP_DIR}/config"
set +e
replace_agent_config_from_release "${APP_DIR}/config/missing.toml" "${agent_template}" >/dev/null 2>&1
replacement_status=$?
set -e
[[ "${replacement_status}" -ne 0 ]]
[[ ! -e "${APP_DIR}/config/.agent-config-v0.20.2" ]]

fixture="${tmp_dir}/fixture"
output="${tmp_dir}/output"
package="qq-maid-bot-v9.9.9-linux-x86_64"
mkdir -p "${fixture}/${package}/config" "${output}"
printf '#!/usr/bin/env bash\nexit 0\n' > "${fixture}/${package}/qq-maid-bot"
printf '#!/usr/bin/env bash\nexit 0\n' > "${fixture}/${package}/botctl.sh"
printf 'EXAMPLE=1\n' > "${fixture}/${package}/config/.env.example"
printf '[agent]\n' > "${fixture}/${package}/config/agent.example.toml"
printf 'fixture\n' > "${fixture}/${package}/README.md"
printf 'v9.9.9\n' > "${fixture}/${package}/VERSION"
chmod +x "${fixture}/${package}/qq-maid-bot" "${fixture}/${package}/botctl.sh"
(
    cd "${fixture}"
    tar -czf "${package}.tar.gz" "${package}"
    sha256sum "${package}.tar.gz" > "${package}.tar.gz.sha256"
)

# 结构损坏 mock 产物：gzip 容器有效但内容不是 tar / 顶层目录不是包名，
# 用于验证“单来源成功前”的归档深度校验能触发回退。
structure_bad_gzip="${tmp_dir}/structure-bad-gzip.tar.gz"
awk 'BEGIN { for (i = 0; i < 200; i++) print "not-a-tar-payload-line" }' | gzip -c > "${structure_bad_gzip}"
structure_wrong_dir="${tmp_dir}/structure-wrong-dir.tar.gz"
mkdir -p "${tmp_dir}/structure-wrong-dir-fixture/wrong-package-dir"
printf 'x\n' > "${tmp_dir}/structure-wrong-dir-fixture/wrong-package-dir/file"
(
    cd "${tmp_dir}/structure-wrong-dir-fixture"
    tar -czf "${structure_wrong_dir}" wrong-package-dir
)

# —— GitHub 下载候选与失败回退回归 ——
# 用假 curl_qbot 模拟网络：请求 URL 写入日志；按规则返回失败、空文件、损坏压缩包或错误校验和；
# 正常请求从 fixture 目录取文件，从而在无网络环境验证候选顺序、去重与回退行为。
CURL_LOG="${tmp_dir}/curl-log"
MOCK_FAIL_PATTERNS=()
MOCK_EMPTY_PATTERNS=()
MOCK_CORRUPT_PATTERNS=()
MOCK_WRONG_HASH_PATTERNS=()
MOCK_STRUCTURE_PATTERNS=()
MOCK_STRUCTURE_ARCHIVE=""

curl_qbot() {
    local url="" out="" arg
    local -a args=("$@")
    local i
    for ((i = 0; i < ${#args[@]}; i++)); do
        arg="${args[$i]}"
        if [[ "${arg}" == "-o" && $((i + 1)) -le $# ]]; then
            out="${args[$((i + 1))]}"
        elif [[ "${arg}" == http* ]]; then
            url="${arg}"
        fi
    done
    printf '%s\n' "${url}" >> "${CURL_LOG}"
    local pattern base
    for pattern in "${MOCK_FAIL_PATTERNS[@]}"; do
        if [[ "${url}" == "${pattern}"* ]]; then
            return 1
        fi
    done
    [[ -n "${out}" ]] || return 0
    base="$(basename -- "${url}")"
    for pattern in "${MOCK_EMPTY_PATTERNS[@]}"; do
        if [[ "${url}" == "${pattern}"* ]]; then
            : > "${out}"
            return 0
        fi
    done
    for pattern in "${MOCK_CORRUPT_PATTERNS[@]}"; do
        if [[ "${url}" == "${pattern}"* ]]; then
            printf 'not-a-gzip\n' > "${out}"
            return 0
        fi
    done
    for pattern in "${MOCK_STRUCTURE_PATTERNS[@]}"; do
        if [[ "${url}" == "${pattern}"* ]]; then
            if [[ "${url}" == *.sha256 ]]; then
                printf '%s  %s\n' "$(sha256sum "${MOCK_STRUCTURE_ARCHIVE}" | awk '{print $1}')" "${package}.tar.gz" > "${out}"
            else
                cp "${MOCK_STRUCTURE_ARCHIVE}" "${out}"
            fi
            return 0
        fi
    done
    for pattern in "${MOCK_WRONG_HASH_PATTERNS[@]}"; do
        if [[ "${url}" == "${pattern}"* && "${url}" == *.sha256 ]]; then
            printf '%064d\n' 0 > "${out}"
            return 0
        fi
    done
    cp "${fixture}/${base}" "${out}"
    return 0
}

# 1) 未配置代理时只访问 GitHub 官方源，且安装成功。
: > "${CURL_LOG}"
GITHUB_ACCEL_PROXY=""
GITHUB_ACCEL_PROXIES=""
download_release v9.9.9 linux-x86_64 "${output}" >/dev/null 2>&1
[[ -x "${output}/${package}/qq-maid-bot" ]]
if grep -qv '^https://github.com/kuliantnt/qq-maid-bot/releases/download/v9.9.9/' "${CURL_LOG}"; then
    echo "未配置代理时访问了非官方源" >&2
    exit 1
fi

# 2) 官方源失败时回退到单个代理源。
: > "${CURL_LOG}"
MOCK_FAIL_PATTERNS=("https://github.com/kuliantnt")
GITHUB_ACCEL_PROXY="https://proxy-a.example.com/"
GITHUB_ACCEL_PROXIES=""
download_release v9.9.9 linux-x86_64 "${output}" >/dev/null 2>&1
grep -q '^https://proxy-a.example.com/https://github.com/kuliantnt/qq-maid-bot/releases/download/v9.9.9/qq-maid-bot-v9.9.9-linux-x86_64.tar.gz$' "${CURL_LOG}"

# 3) 第一个代理失败时继续尝试后续代理。
: > "${CURL_LOG}"
MOCK_FAIL_PATTERNS=("https://github.com/kuliantnt" "https://proxy-bad.example.com")
GITHUB_ACCEL_PROXY="https://proxy-bad.example.com"
GITHUB_ACCEL_PROXIES="https://proxy-good.example.com"
download_release v9.9.9 linux-x86_64 "${output}" >/dev/null 2>&1
proxy_bad_first="$(grep -n 'proxy-bad.example.com' "${CURL_LOG}" | head -n 1 | cut -d: -f1)"
proxy_good_first="$(grep -n 'proxy-good.example.com' "${CURL_LOG}" | head -n 1 | cut -d: -f1)"
[[ -n "${proxy_bad_first}" && -n "${proxy_good_first}" && "${proxy_bad_first}" -lt "${proxy_good_first}" ]]

# 4) HTTP 成功但文件为空 / 压缩包损坏 / 校验和无效时继续回退。
: > "${CURL_LOG}"
MOCK_EMPTY_PATTERNS=("https://github.com/kuliantnt")
GITHUB_ACCEL_PROXY="https://proxy-a.example.com"
GITHUB_ACCEL_PROXIES=""
download_release v9.9.9 linux-x86_64 "${output}" >/dev/null 2>&1

: > "${CURL_LOG}"
MOCK_EMPTY_PATTERNS=()
MOCK_CORRUPT_PATTERNS=("https://github.com/kuliantnt")
download_release v9.9.9 linux-x86_64 "${output}" >/dev/null 2>&1

: > "${CURL_LOG}"
MOCK_CORRUPT_PATTERNS=()
MOCK_WRONG_HASH_PATTERNS=("https://github.com/kuliantnt")
download_release v9.9.9 linux-x86_64 "${output}" >/dev/null 2>&1

# 4b) gzip 容器有效但结构损坏（内容非 tar / 顶层目录不是包名）时继续回退下一来源。
: > "${CURL_LOG}"
MOCK_CORRUPT_PATTERNS=()
MOCK_WRONG_HASH_PATTERNS=()
GITHUB_ACCEL_PROXY="https://proxy-a.example.com"
GITHUB_ACCEL_PROXIES=""

MOCK_STRUCTURE_PATTERNS=("https://github.com/kuliantnt")
MOCK_STRUCTURE_ARCHIVE="${structure_bad_gzip}"
download_release v9.9.9 linux-x86_64 "${output}" >/dev/null 2>&1
[[ -x "${output}/${package}/qq-maid-bot" ]]
grep -q '^https://github.com/kuliantnt/qq-maid-bot/releases/download/v9.9.9/qq-maid-bot-v9.9.9-linux-x86_64.tar.gz$' "${CURL_LOG}"
grep -q '^https://proxy-a.example.com/' "${CURL_LOG}"

MOCK_STRUCTURE_ARCHIVE="${structure_wrong_dir}"
download_release v9.9.9 linux-x86_64 "${output}" >/dev/null 2>&1
[[ -x "${output}/${package}/qq-maid-bot" ]]
grep -q '^https://github.com/kuliantnt/qq-maid-bot/releases/download/v9.9.9/qq-maid-bot-v9.9.9-linux-x86_64.tar.gz$' "${CURL_LOG}"
grep -q '^https://proxy-a.example.com/' "${CURL_LOG}"
MOCK_STRUCTURE_PATTERNS=()
MOCK_STRUCTURE_ARCHIVE=""

# 5) 重复代理（含尾部斜杠差异）不产生重复请求。
: > "${CURL_LOG}"
MOCK_WRONG_HASH_PATTERNS=()
MOCK_FAIL_PATTERNS=("https://github.com/kuliantnt")
GITHUB_ACCEL_PROXY="https://proxy-a.example.com/"
GITHUB_ACCEL_PROXIES="https://proxy-a.example.com https://proxy-a.example.com/"
download_release v9.9.9 linux-x86_64 "${output}" >/dev/null 2>&1
archive_requests="$(grep -c '^https://proxy-a.example.com/https://github.com/kuliantnt/qq-maid-bot/releases/download/v9.9.9/qq-maid-bot-v9.9.9-linux-x86_64.tar.gz$' "${CURL_LOG}" || true)"
[[ "${archive_requests}" == "1" ]]

# 6) 所有来源失败时返回非零且不落盘任何程序文件。
: > "${CURL_LOG}"
MOCK_FAIL_PATTERNS=("https://github.com/kuliantnt")
GITHUB_ACCEL_PROXY=""
GITHUB_ACCEL_PROXIES=""
fail_output="${tmp_dir}/output-fail"
set +e
fail_output_text="$(download_release v9.9.9 linux-x86_64 "${fail_output}" 2>&1)"
fail_status=$?
set -e
[[ "${fail_status}" -ne 0 ]]
[[ ! -e "${fail_output}/${package}" ]]
[[ "${fail_output_text}" == *"QBOT_GITHUB_PROXY"* ]]

# 7) 候选列表规范化与去重（与 PowerShell 端语义一致）。
GITHUB_ACCEL_PROXY="https://proxy-a.example.com/"
GITHUB_ACCEL_PROXIES="https://proxy-b.example.com https://proxy-a.example.com bad-addr"
candidate_list="$(github_accel_prefixes)"
expected_candidates="$(printf '\n%s\n%s' 'https://proxy-a.example.com' 'https://proxy-b.example.com')"
[[ "${candidate_list}" == "${expected_candidates}" ]]

# 7a) 代理前缀必须含非空主机部分：纯 scheme、空主机、仅端口等写法都应被忽略。
GITHUB_ACCEL_PROXY="https://proxy-a.example.com"
GITHUB_ACCEL_PROXIES="http:// http:///path https://?query http://:8080 https://#frag https://good.example.com"
candidate_list="$(github_accel_prefixes 2>/dev/null)"
expected_candidates="$(printf '\n%s\n%s' 'https://proxy-a.example.com' 'https://good.example.com')"
[[ "${candidate_list}" == "${expected_candidates}" ]]

# 8) QBOT_GITHUB_PROXY / QBOT_GITHUB_PROXIES 环境变量映射到内部候选变量。
env_mapping="$(QBOT_GITHUB_PROXY='https://env-a.example.com/' QBOT_GITHUB_PROXIES='https://env-b.example.com' bash -c '
    source "$1/scripts/qbot.sh"
    printf "%s|%s" "${GITHUB_ACCEL_PROXY}" "${GITHUB_ACCEL_PROXIES}"
' _ "${REPO_DIR}")"
# 内部候选变量保留原始值，去尾部斜杠等规范化发生在 github_accel_prefixes（见第 7 项测试）。
[[ "${env_mapping}" == "https://env-a.example.com/|https://env-b.example.com" ]]

# 恢复正常来源，验证完整安装链路仍然可用。
MOCK_FAIL_PATTERNS=()
MOCK_EMPTY_PATTERNS=()
MOCK_CORRUPT_PATTERNS=()
MOCK_WRONG_HASH_PATTERNS=()
MOCK_STRUCTURE_PATTERNS=()
MOCK_STRUCTURE_ARCHIVE=""
GITHUB_ACCEL_PROXY=""
GITHUB_ACCEL_PROXIES=""
release_dir="$(download_release v9.9.9 linux-x86_64 "${output}")"
[[ -x "${release_dir}/qq-maid-bot" ]]

APP_DIR="${tmp_dir}/installed"
mkdir -p "${APP_DIR}/config" "${APP_DIR}/data/storage" "${APP_DIR}/logs" "${APP_DIR}/run"
printf '%s\n' \
    'PRIVATE=keep' \
    'LLM_MODEL=openai:legacy-model' \
    ' export TOOL_CALLING_ENABLED = true' \
    'TODO_MODEL=legacy-todo-model' \
    'QQ_MAID_ENABLE_IMAGE=false' \
    'QWEATHER_API_KEY=' > "${APP_DIR}/config/.env"
printf 'db\n' > "${APP_DIR}/data/storage/app.db"
printf 'log\n' > "${APP_DIR}/logs/qq-maid-bot.log"
printf '123\n' > "${APP_DIR}/run/qq-maid-bot.pid"
for obsolete_windows_file in \
    qbot.ps1 \
    qbot.cmd \
    botctl.ps1 \
    botctl.cmd \
    windows-startup-example.bat
do
    printf 'obsolete\n' > "${APP_DIR}/${obsolete_windows_file}"
done

copy_release_into_app "${release_dir}" v9.9.9
[[ -x "${APP_DIR}/qq-maid-bot" ]]
[[ -x "${APP_DIR}/botctl.sh" ]]
[[ -f "${APP_DIR}/config/.env.example" ]]
[[ -f "${APP_DIR}/config/agent.example.toml" ]]
[[ ! -e "${APP_DIR}/config/agent.toml" ]]
grep -Fqx 'PRIVATE=keep' "${APP_DIR}/config/.env"
grep -Fqx 'QWEATHER_API_KEY=' "${APP_DIR}/config/.env"
! grep -Eq '^[[:space:]]*(export[[:space:]]+)?(LLM_MODEL|TOOL_CALLING_ENABLED|TODO_MODEL|QQ_MAID_ENABLE_IMAGE)[[:space:]]*=' "${APP_DIR}/config/.env"
backup_files=("${APP_DIR}"/config/.env.bak.v0.20.*)
[[ "${#backup_files[@]}" -eq 1 ]]
grep -Fqx 'LLM_MODEL=openai:legacy-model' "${backup_files[0]}"
grep -Fqx 'QQ_MAID_ENABLE_IMAGE=false' "${backup_files[0]}"
grep -Fqx 'db' "${APP_DIR}/data/storage/app.db"
grep -Fqx 'log' "${APP_DIR}/logs/qq-maid-bot.log"
grep -Fqx '123' "${APP_DIR}/run/qq-maid-bot.pid"
for obsolete_windows_file in \
    qbot.ps1 \
    qbot.cmd \
    botctl.ps1 \
    botctl.cmd \
    windows-startup-example.bat
do
    [[ ! -e "${APP_DIR}/${obsolete_windows_file}" ]] || {
        echo "obsolete Windows control file was not removed: ${obsolete_windows_file}" >&2
        exit 1
    }
done

# 校验用户 shell、Git 与系统全局配置在测试前后没有变化。
for snapshot_file in .bashrc .zshrc .profile .gitconfig; do
    if [[ -f "${config_snapshot}/${snapshot_file}" ]]; then
        # 快照时文件存在：结束时必须仍存在且内容一致。
        if [[ -f "${HOME}/${snapshot_file}" ]]; then
            cmp -s "${HOME}/${snapshot_file}" "${config_snapshot}/${snapshot_file}" || {
                echo "用户配置文件在测试期间被修改: ${HOME}/${snapshot_file}" >&2
                exit 1
            }
        else
            echo "用户配置文件在测试期间被删除: ${HOME}/${snapshot_file}" >&2
            exit 1
        fi
    else
        # 快照时文件不存在：结束时也不应出现。
        if [[ -f "${HOME}/${snapshot_file}" ]]; then
            echo "测试期间不应创建用户配置文件: ${HOME}/${snapshot_file}" >&2
            exit 1
        fi
    fi
done
git config --global --list > "${config_snapshot}/git-global-config.after" 2>/dev/null || true
cmp -s "${config_snapshot}/git-global-config" "${config_snapshot}/git-global-config.after" || {
    echo "git 全局配置在测试期间被修改" >&2
    exit 1
}

echo "qbot Unix installer regression tests passed"

#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "${REPO_DIR}/scripts/qbot.sh"

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

agent_config_reset_required v0.20.1 v0.20.2
agent_config_reset_required v0.20.1 v0.21.0
! agent_config_reset_required v0.20.2 v0.20.3
! agent_config_reset_required v0.20.3 v0.21.0

# Unix 安装器不得再包含 Windows target、ZIP 或原生 Windows 二进制分支。
if rg -n 'MINGW|MSYS|CYGWIN|windows-(x86_64|aarch64)|\.zip|qq-maid-bot\.exe' \
    "${REPO_DIR}/scripts/qbot.sh" >/dev/null; then
    echo "scripts/qbot.sh unexpectedly contains Windows-specific logic" >&2
    exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

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

fixture="${tmp_dir}/fixture"
output="${tmp_dir}/output"
package="qq-maid-bot-v9.9.9-linux-x86_64"
mkdir -p "${fixture}/${package}/config" "${output}"
printf '#!/usr/bin/env bash\nexit 0\n' > "${fixture}/${package}/qq-maid-bot"
printf '#!/usr/bin/env bash\nexit 0\n' > "${fixture}/${package}/botctl.sh"
printf 'EXAMPLE=1\n' > "${fixture}/${package}/config/.env.example"
printf '[agent]\n' > "${fixture}/${package}/config/agent.toml"
printf 'fixture\n' > "${fixture}/${package}/README.md"
printf 'v9.9.9\n' > "${fixture}/${package}/VERSION"
chmod +x "${fixture}/${package}/qq-maid-bot" "${fixture}/${package}/botctl.sh"
(
    cd "${fixture}"
    tar -czf "${package}.tar.gz" "${package}"
    sha256sum "${package}.tar.gz" > "${package}.tar.gz.sha256"
)

download_github_file() {
    cp "${fixture}/$3" "$2"
}

release_dir="$(download_release v9.9.9 linux-x86_64 "${output}")"
[[ -x "${release_dir}/qq-maid-bot" ]]

APP_DIR="${tmp_dir}/installed"
mkdir -p "${APP_DIR}/config" "${APP_DIR}/data/storage" "${APP_DIR}/logs" "${APP_DIR}/run"
printf '%s\n' \
    'PRIVATE=keep' \
    'LLM_MODEL=openai:legacy-model' \
    ' export TOOL_CALLING_ENABLED = true' \
    'TODO_MODEL=legacy-todo-model' \
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
grep -Fqx 'PRIVATE=keep' "${APP_DIR}/config/.env"
grep -Fqx 'QWEATHER_API_KEY=' "${APP_DIR}/config/.env"
! grep -Eq '^[[:space:]]*(export[[:space:]]+)?(LLM_MODEL|TOOL_CALLING_ENABLED|TODO_MODEL)[[:space:]]*=' "${APP_DIR}/config/.env"
backup_files=("${APP_DIR}"/config/.env.bak.v0.20.*)
[[ "${#backup_files[@]}" -eq 1 ]]
grep -Fqx 'LLM_MODEL=openai:legacy-model' "${backup_files[0]}"
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

echo "qbot Unix installer regression tests passed"

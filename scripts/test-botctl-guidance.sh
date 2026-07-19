#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf -- "${TMP_ROOT}"' EXIT

new_runtime() {
    local name="$1"
    local runtime="${TMP_ROOT}/${name}"
    mkdir -p "${runtime}/config/secrets"
    cp "${REPO_DIR}/scripts/botctl.sh" "${runtime}/botctl.sh"
    printf '%s\n' '#!/usr/bin/env bash' 'exec sleep 30' > "${runtime}/qq-maid-bot"
    chmod +x "${runtime}/botctl.sh" "${runtime}/qq-maid-bot"
    echo "${runtime}"
}

start_and_stop() {
    local runtime="$1"
    local output
    output="$(QQ_MAID_RUNTIME_DIR="${runtime}" bash "${runtime}/botctl.sh" start)"
    QQ_MAID_RUNTIME_DIR="${runtime}" bash "${runtime}/botctl.sh" stop >/dev/null
    printf '%s' "${output}"
}

runtime="$(new_runtime no-token)"
output="$(start_and_stop "${runtime}")"
[[ "${output}" == *"qq-maid-bot started"* ]]
[[ "${output}" != *"首次配置"* ]]
[[ "${output}" != *"密码重置"* ]]

runtime="$(new_runtime initialize)"
printf '%s\n' \
    'LLM_SERVER_PORT=9988' \
    'WEB_CONSOLE_ENABLED=true' \
    'LLM_MODEL=openai:legacy-model' \
    ' export TOOL_CALLING_ENABLED = true' \
    'QQ_MAID_ENABLE_IMAGE=false' \
    'QWEATHER_API_KEY=' > "${runtime}/config/.env"
printf '%s\n' 'qq-maid-bootstrap-v1:1:initialize-secret' > "${runtime}/config/secrets/bootstrap.token"
output="$(start_and_stop "${runtime}")"
[[ "${output}" == *"--- 首次配置 ---"* ]]
[[ "${output}" == *"http://127.0.0.1:9988/console/"* ]]
[[ "${output}" != *"initialize-secret"* ]]
[[ "${output}" != *"legacy-model"* ]]
grep -Fqx 'QWEATHER_API_KEY=' "${runtime}/config/.env"
! grep -Eq '^[[:space:]]*(export[[:space:]]+)?(LLM_MODEL|TOOL_CALLING_ENABLED|QQ_MAID_ENABLE_IMAGE)[[:space:]]*=' "${runtime}/config/.env"
backup_files=("${runtime}"/config/.env.bak.v0.20.*)
[[ "${#backup_files[@]}" -eq 1 ]]
grep -Fqx 'LLM_MODEL=openai:legacy-model' "${backup_files[0]}"
grep -Fqx 'QQ_MAID_ENABLE_IMAGE=false' "${backup_files[0]}"

runtime="$(new_runtime password-reset)"
printf '%s\n' 'WEB_CONSOLE_ENABLED=true' > "${runtime}/config/.env"
printf '%s\n' 'qq-maid-password-reset-v1:2:reset-secret' > "${runtime}/config/secrets/bootstrap.token"
output="$(start_and_stop "${runtime}")"
[[ "${output}" == *"--- 密码重置待完成 ---"* ]]
[[ "${output}" != *"--- 首次配置 ---"* ]]
[[ "${output}" != *"reset-secret"* ]]

runtime="$(new_runtime invalid-token)"
printf '%s\n' 'WEB_CONSOLE_ENABLED=true' > "${runtime}/config/.env"
printf '%s\n' 'qq-maid-bootstrap-v1:3:must-not-leak' 'unexpected-extra-line' > "${runtime}/config/secrets/bootstrap.token"
output="$(start_and_stop "${runtime}")"
[[ "${output}" != *"首次配置"* ]]
[[ "${output}" != *"密码重置"* ]]
[[ "${output}" != *"must-not-leak"* ]]

runtime="$(new_runtime console-disabled)"
printf '%s\n' 'WEB_CONSOLE_ENABLED=false' > "${runtime}/config/.env"
printf '%s\n' 'qq-maid-bootstrap-v1:4:disabled-secret' > "${runtime}/config/secrets/bootstrap.token"
output="$(start_and_stop "${runtime}")"
[[ "${output}" != *"首次配置"* ]]
[[ "${output}" != *"/console/"* ]]
[[ "${output}" != *"disabled-secret"* ]]

runtime="$(new_runtime wildcard-host)"
printf '%s\n' 'LLM_SERVER_HOST=0.0.0.0' 'LLM_SERVER_PORT=9989' 'WEB_CONSOLE_ENABLED=true' > "${runtime}/config/.env"
printf '%s\n' 'qq-maid-bootstrap-v1:5:wildcard-secret' > "${runtime}/config/secrets/bootstrap.token"
output="$(start_and_stop "${runtime}")"
[[ "${output}" == *"使用实际服务器地址或反向代理地址"* ]]
[[ "${output}" != *"http://0.0.0.0"* ]]
[[ "${output}" != *"wildcard-secret"* ]]

echo "botctl guidance regression tests passed"

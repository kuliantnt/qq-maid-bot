#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "${TMP_ROOT}"' EXIT

new_fixture() {
    local name="$1"
    local app_dir="${TMP_ROOT}/${name}"
    mkdir -p "${app_dir}/config"
    : > "${app_dir}/config/.env"
    echo "${app_dir}"
}

run_config_bot() {
    local app_dir="$1"
    shift
    QBOT_APP_DIR="${app_dir}" \
        QBOT_CONFIG_NO_BACKUP=1 \
        QBOT_NO_CLEAR=1 \
        NO_COLOR=1 \
        bash "${REPO_DIR}/scripts/qbot.sh" config bot "$@"
}

assert_config_value() {
    local app_dir="$1"
    local expected="$2"
    grep -Fqx "${expected}" "${app_dir}/config/.env"
}

assert_config_absent() {
    local app_dir="$1"
    local key="$2"
    ! grep -Eq "^[[:space:]]*${key}=" "${app_dir}/config/.env"
}

for args in "--enable --disable" "--unbind --disable" "--unbind --enable"; do
    app_dir="$(new_fixture "mutual-${args// /-}")"
    set +e
    output="$(run_config_bot "${app_dir}" ${args} 2>&1)"
    status=$?
    set -e
    [[ "${status}" -ne 0 ]]
    [[ "${output}" == *"--enable、--disable、--unbind 互斥"* ]]
done

app_dir="$(new_fixture enable-without-credentials)"
set +e
output="$(run_config_bot "${app_dir}" --enable 2>&1)"
status=$?
set -e
[[ "${status}" -ne 0 ]]
[[ "${output}" == *"--enable 需要完整 QQ 凭证"* ]]
[[ "${output}" != *"已启用:"* ]]
assert_config_absent "${app_dir}" QQ_BOT_ENABLED

app_dir="$(new_fixture enable-existing-credentials)"
printf '%s\n' "QQ_BOT_APP_ID='existing-id'" "QQ_BOT_APP_SECRET='existing-secret'" > "${app_dir}/config/.env"
output="$(run_config_bot "${app_dir}" --enable)"
[[ "${output}" == *"已启用:"* ]]
assert_config_value "${app_dir}" "QQ_BOT_ENABLED='true'"

app_dir="$(new_fixture enable-legacy-credentials)"
printf '%s\n' "QQ_APPID='legacy-id'" "QQ_SECRET='legacy-secret'" > "${app_dir}/config/.env"
run_config_bot "${app_dir}" --enable >/dev/null
assert_config_value "${app_dir}" "QQ_BOT_ENABLED='true'"

app_dir="$(new_fixture enable-new-credentials)"
output="$(run_config_bot "${app_dir}" --enable --app-id new-id --app-secret new-secret)"
[[ "${output}" == *"已启用:"* ]]
assert_config_value "${app_dir}" "QQ_BOT_APP_ID='new-id'"
assert_config_value "${app_dir}" "QQ_BOT_APP_SECRET='new-secret'"
assert_config_value "${app_dir}" "QQ_BOT_ENABLED='true'"

app_dir="$(new_fixture complete-credentials-default-enable)"
run_config_bot "${app_dir}" --app-id default-id --app-secret default-secret >/dev/null
assert_config_value "${app_dir}" "QQ_BOT_ENABLED='true'"

app_dir="$(new_fixture active-keywords)"
run_config_bot "${app_dir}" --active-keywords "小助手,助手,bot" >/dev/null
assert_config_value "${app_dir}" "QQ_MAID_GROUP_ACTIVE_KEYWORDS='小助手,助手,bot'"
assert_config_absent "${app_dir}" QQ_MAID_STATUS_DISPLAY_NAME

app_dir="$(new_fixture legacy-display-name)"
output="$(run_config_bot "${app_dir}" --display-name 小管家 2>&1)"
[[ "${output}" == *"--display-name/--name 已废弃"* ]]
assert_config_value "${app_dir}" "QQ_MAID_GROUP_ACTIVE_KEYWORDS='小管家'"
assert_config_absent "${app_dir}" QQ_MAID_STATUS_DISPLAY_NAME

app_dir="$(new_fixture conflicting-display-name)"
set +e
output="$(run_config_bot "${app_dir}" --display-name 小管家 --active-keywords 小助手 2>&1)"
status=$?
set -e
[[ "${status}" -ne 0 ]]
[[ "${output}" == *"不能与 --active-keywords 同时使用"* ]]

app_dir="$(new_fixture disable-preserves-credentials)"
printf '%s\n' "QQ_BOT_APP_ID='kept-id'" "QQ_BOT_APP_SECRET='kept-secret'" > "${app_dir}/config/.env"
run_config_bot "${app_dir}" --disable >/dev/null
assert_config_value "${app_dir}" "QQ_BOT_APP_ID='kept-id'"
assert_config_value "${app_dir}" "QQ_BOT_APP_SECRET='kept-secret'"
assert_config_value "${app_dir}" "QQ_BOT_ENABLED='false'"

app_dir="$(new_fixture unbind-preserves-other-config)"
printf '%s\n' \
    "QQ_BOT_APP_ID='new-id'" \
    "QQ_BOT_APP_SECRET='new-secret'" \
    "QQ_APPID='legacy-id'" \
    "QQ_SECRET='legacy-secret'" \
    "WECHAT_SERVICE_ENABLED='true'" \
    "APP_DB_FILE='data/storage/app.db'" > "${app_dir}/config/.env"
run_config_bot "${app_dir}" --unbind >/dev/null
assert_config_absent "${app_dir}" QQ_BOT_APP_ID
assert_config_absent "${app_dir}" QQ_BOT_APP_SECRET
assert_config_absent "${app_dir}" QQ_APPID
assert_config_absent "${app_dir}" QQ_SECRET
assert_config_value "${app_dir}" "WECHAT_SERVICE_ENABLED='true'"
assert_config_value "${app_dir}" "APP_DB_FILE='data/storage/app.db'"

echo "qbot config bot regression tests passed"

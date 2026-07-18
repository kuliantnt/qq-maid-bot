#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="${REPO_DIR}/scripts/prepare_project_docs_kb.sh"
TMP_ROOT="$(mktemp -d)"
REAL_RM="$(command -v rm)"
export REAL_RM REPO_DIR
trap '"${REAL_RM}" -rf -- "${TMP_ROOT}"' EXIT

create_wiki_fixture() {
    local directory="$1"
    local omit_page="${2:-}"
    local page
    local pages=(
        HOME.md 使用说明.md 安装手册.md 配置中心.md Napcat接入.md
        ops运维命令.md ops-codex.md 和风天气配置.md 开发维护文档.md 插件开发.md
    )

    mkdir -p "${directory}"
    git -C "${directory}" init -q -b main
    git -C "${directory}" config user.name test
    git -C "${directory}" config user.email test@example.invalid
    for page in "${pages[@]}"; do
        [[ "${page}" == "${omit_page}" ]] && continue
        printf '# %s\n\nfixture content\n' "${page%.md}" > "${directory}/${page}"
    done
    git -C "${directory}" add .
    git -C "${directory}" commit -q -m fixture
}

run_prepare() {
    local wiki_url="$1"
    local cache="$2"
    local output="$3"
    WIKI_URL="${wiki_url}" WIKI_CACHE="${cache}" bash "${SCRIPT}" --out "${output}"
}

assert_rejected() {
    local output="$1"
    shift
    set +e
    local message
    message="$("$@" 2>&1)"
    local status=$?
    set -e
    [[ "${status}" -ne 0 ]]
    [[ -n "${message}" ]]
    [[ -z "${output}" || ! -e "${output}" || -d "${output}" ]]
}

valid_wiki="${TMP_ROOT}/wiki-valid"
missing_wiki="${TMP_ROOT}/wiki-missing"
other_wiki="${TMP_ROOT}/wiki-other"
create_wiki_fixture "${valid_wiki}"
create_wiki_fixture "${missing_wiki}" "插件开发.md"
create_wiki_fixture "${other_wiki}"

output_dir="${TMP_ROOT}/output"
cache_dir="${TMP_ROOT}/cache/wiki"
run_prepare "${valid_wiki}" "${cache_dir}" "${output_dir}" >/dev/null
[[ -f "${output_dir}/README.md" ]]
[[ -f "${output_dir}/wiki-usage.md" ]]
grep -Fq "fixture content" "${output_dir}/wiki-usage.md"
grep -Fqx "qq-maid-project-docs-kb-v1" "${output_dir}/.qq-maid-project-docs-kb"
[[ "$(git -C "${cache_dir}" config --local core.hooksPath)" == "/dev/null" ]]
[[ "$(stat -c '%a' "${cache_dir}")" == "700" ]]

# 即使复用已有同 owner、同 remote 缓存，也必须在 pull 前禁用仓库 hooks。
hook_cache="${TMP_ROOT}/hook-cache"
hook_output="${TMP_ROOT}/hook-output"
hook_fired="${TMP_ROOT}/hook-fired"
git clone -q "${valid_wiki}" "${hook_cache}"
printf '%s\n' '#!/usr/bin/env bash' "printf fired > '${hook_fired}'" > "${hook_cache}/.git/hooks/post-merge"
chmod +x "${hook_cache}/.git/hooks/post-merge"
printf '\nupdated\n' >> "${valid_wiki}/HOME.md"
git -C "${valid_wiki}" add HOME.md
git -C "${valid_wiki}" commit -q -m update
run_prepare "${valid_wiki}" "${hook_cache}" "${hook_output}" >/dev/null
[[ ! -e "${hook_fired}" ]]

# 生成失败不能覆盖上一版托管输出。
printf 'keep previous output\n' > "${output_dir}/preserved.txt"
assert_rejected "${output_dir}" run_prepare \
    "${missing_wiki}" "${TMP_ROOT}/cache/missing" "${output_dir}"
grep -Fqx "keep previous output" "${output_dir}/preserved.txt"

# 既有非托管目录必须原样保留。
unmanaged="${TMP_ROOT}/unmanaged"
mkdir -p "${unmanaged}"
printf 'user data\n' > "${unmanaged}/important.txt"
assert_rejected "${unmanaged}" run_prepare "${valid_wiki}" "${cache_dir}" "${unmanaged}"
grep -Fqx "user data" "${unmanaged}/important.txt"

# 输出目录与缓存目录的符号链接都不得被跟随。
output_target="${TMP_ROOT}/output-target"
mkdir -p "${output_target}"
ln -s "${output_target}" "${TMP_ROOT}/output-link"
assert_rejected "${TMP_ROOT}/output-link" run_prepare \
    "${valid_wiki}" "${cache_dir}" "${TMP_ROOT}/output-link"
cache_target="${TMP_ROOT}/cache-target"
git clone -q "${valid_wiki}" "${cache_target}"
ln -s "${cache_target}" "${TMP_ROOT}/cache-link"
assert_rejected "${TMP_ROOT}/unused-output" run_prepare \
    "${valid_wiki}" "${TMP_ROOT}/cache-link" "${TMP_ROOT}/unused-output"

# 已有缓存的 remote 不匹配时拒绝复用，不执行 pull。
wrong_cache="${TMP_ROOT}/wrong-cache"
git clone -q "${valid_wiki}" "${wrong_cache}"
assert_rejected "${TMP_ROOT}/wrong-output" run_prepare \
    "${other_wiki}" "${wrong_cache}" "${TMP_ROOT}/wrong-output"

# 危险路径检查发生在 Git 和目录替换之前。HOME 使用隔离 fixture；rm shim 为回归失效兜底。
shim_dir="${TMP_ROOT}/shim"
mkdir -p "${shim_dir}"
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'for arg in "$@"; do' \
    '  case "$arg" in /|"$REPO_DIR"|"${TEST_HOME:-}") exit 99 ;; esac' \
    'done' \
    'exec "$REAL_RM" "$@"' > "${shim_dir}/rm"
chmod +x "${shim_dir}/rm"
test_home="${TMP_ROOT}/home"
mkdir -p "${test_home}"
TEST_HOME="${test_home}" HOME="${test_home}" PATH="${shim_dir}:${PATH}" \
    assert_rejected "/" run_prepare "${valid_wiki}" "${cache_dir}" "/"
TEST_HOME="${test_home}" HOME="${test_home}" PATH="${shim_dir}:${PATH}" \
    assert_rejected "${test_home}" run_prepare "${valid_wiki}" "${cache_dir}" "${test_home}"
TEST_HOME="${test_home}" PATH="${shim_dir}:${PATH}" \
    assert_rejected "${REPO_DIR}" run_prepare "${valid_wiki}" "${cache_dir}" "${REPO_DIR}"

# 未指定 WIKI_CACHE 时落到隔离 HOME 下的用户私有缓存目录。
default_home="${TMP_ROOT}/default-home"
default_output="${TMP_ROOT}/default-output"
mkdir -p "${default_home}"
HOME="${default_home}" WIKI_URL="${valid_wiki}" bash "${SCRIPT}" --out "${default_output}" >/dev/null
[[ -d "${default_home}/.cache/qq-maid-bot/wiki/.git" ]]

echo "project docs knowledge-base regression tests passed"

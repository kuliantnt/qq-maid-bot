#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "${TEST_DIR}"' EXIT

TAG_COMMIT="1111111111111111111111111111111111111111"
FAKE_GH="${TEST_DIR}/gh"

cat > "${FAKE_GH}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >> "${FAKE_GH_LOG}"
case "$*" in
    *'/actions/workflows/container.yml/runs'*)
        call_count=0
        if [[ -f "${FAKE_GH_CONTAINER_CALLS_FILE}" ]]; then
            read -r call_count < "${FAKE_GH_CONTAINER_CALLS_FILE}"
        fi
        call_count=$((call_count + 1))
        printf '%s\n' "${call_count}" > "${FAKE_GH_CONTAINER_CALLS_FILE}"
        if ((call_count > FAKE_GH_EMPTY_RESPONSES_BEFORE_SUCCESS)); then
            printf '%s\n' "${FAKE_GH_RUN_IDS:-}"
        fi
        ;;
    *'/jobs'*)
        [[ "${FAKE_GH_FAIL_ON_JOBS:-false}" != "true" ]] || exit 42
        printf '%s\n' "${FAKE_GH_DEPLOY_CONCLUSION:-}"
        ;;
    *)
        printf 'unexpected fake gh call: %s\n' "$*" >&2
        exit 2
        ;;
esac
EOF
chmod +x "${FAKE_GH}"

run_gate() {
    local require_gate="$1"
    local run_ids="$2"
    local deploy_conclusion="$3"
    local fail_on_jobs="$4"
    local case_name="$5"
    local empty_responses_before_success="${6:-0}"
    local summary_file="${TEST_DIR}/${case_name}.summary"

    GH_BIN="${FAKE_GH}" \
    GITHUB_REPOSITORY="kuliantnt/qq-maid-bot" \
    TAG_COMMIT="${TAG_COMMIT}" \
    REQUIRE_TEST_DEPLOY_FOR_RELEASE="${require_gate}" \
    GITHUB_STEP_SUMMARY="${summary_file}" \
    FAKE_GH_LOG="${TEST_DIR}/gh.log" \
    FAKE_GH_RUN_IDS="${run_ids}" \
    FAKE_GH_EMPTY_RESPONSES_BEFORE_SUCCESS="${empty_responses_before_success}" \
    FAKE_GH_CONTAINER_CALLS_FILE="${TEST_DIR}/${case_name}.container-calls" \
    FAKE_GH_DEPLOY_CONCLUSION="${deploy_conclusion}" \
    FAKE_GH_FAIL_ON_JOBS="${fail_on_jobs}" \
    CONTAINER_WORKFLOW_WAIT_SECONDS=2 \
    CONTAINER_WORKFLOW_POLL_SECONDS=1 \
        bash "${ROOT_DIR}/scripts/verify-container-release.sh"
}

# 默认未启用严格门禁：只要对应 master Container workflow 成功即可继续，且不会查询测试部署。
run_gate "" "101" "" true default-disabled
grep -Fqx 'verified master Container workflow run: 101' "${TEST_DIR}/default-disabled.summary"
grep -Fqx \
    'test Environment deployment gate: disabled (REQUIRE_TEST_DEPLOY_FOR_RELEASE != true)' \
    "${TEST_DIR}/default-disabled.summary"

# tag 紧跟 master push 时，首次查询可能看不到尚未完成的 Container workflow；等待后应继续晋级。
run_gate "" "105" "" true delayed-container-workflow 2
grep -Fqx \
    'verified master Container workflow run: 105' \
    "${TEST_DIR}/delayed-container-workflow.summary"
grep -Fqx '3' "${TEST_DIR}/delayed-container-workflow.container-calls"

# 无论是否开启测试门禁，都必须存在对应 commit 的成功 master Container workflow。
if run_gate "" "" "" true missing-container-workflow; then
    echo "expected missing master Container workflow to reject promotion" >&2
    exit 1
fi

# 强制启用但测试部署未成功：必须拒绝晋级。
if run_gate true "102" skipped false required-without-deploy; then
    echo "expected required test deployment gate to reject promotion" >&2
    exit 1
fi

# 强制启用且测试部署成功：允许晋级。
run_gate true $'103\n104' success false required-with-deploy
grep -Fqx \
    'verified test Environment deployment in workflow run: 103' \
    "${TEST_DIR}/required-with-deploy.summary"

printf 'container release gate tests passed\n'

#!/usr/bin/env bash
set -euo pipefail

GH_BIN="${GH_BIN:-gh}"
REPOSITORY="${GITHUB_REPOSITORY:-}"
TAG_COMMIT="${TAG_COMMIT:-}"
# 仓库未配置该变量时按 false 处理；只有精确的 true 才开启严格测试部署门禁。
REQUIRE_TEST_DEPLOY="${REQUIRE_TEST_DEPLOY_FOR_RELEASE:-false}"

fail() {
    printf '::error::%s\n' "$*" >&2
    exit 1
}

summary() {
    if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
        printf '%s\n' "$*" >> "${GITHUB_STEP_SUMMARY}"
    else
        printf '%s\n' "$*"
    fi
}

[[ "${REPOSITORY}" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
    || fail "GITHUB_REPOSITORY 必须是 owner/repository"
[[ "${TAG_COMMIT}" =~ ^[0-9a-f]{40}$ ]] \
    || fail "TAG_COMMIT 必须是 40 位小写 Git commit"

successful_run_ids="$("${GH_BIN}" api --method GET \
    "repos/${REPOSITORY}/actions/workflows/container.yml/runs" \
    -f branch=master \
    -f event=push \
    -f status=completed \
    -f head_sha="${TAG_COMMIT}" \
    --jq '.workflow_runs[] | select(.conclusion == "success") | .id')"

container_run_id="$(printf '%s\n' "${successful_run_ids}" | sed -n '/[^[:space:]]/ { p; q; }')"
[[ -n "${container_run_id}" ]] \
    || fail "tag commit 没有成功的 master Container workflow，拒绝晋级容器镜像"

summary "verified master Container workflow run: ${container_run_id}"

if [[ "${REQUIRE_TEST_DEPLOY}" != "true" ]]; then
    summary "test Environment deployment gate: disabled (REQUIRE_TEST_DEPLOY_FOR_RELEASE != true)"
    exit 0
fi

deployed_run_id=""
while IFS= read -r run_id; do
    [[ -n "${run_id}" ]] || continue
    deploy_conclusions="$("${GH_BIN}" api --paginate \
        "repos/${REPOSITORY}/actions/runs/${run_id}/jobs" \
        --jq '.jobs[] | select(.name == "Deploy test environment") | .conclusion')"
    if grep -Fqx 'success' <<< "${deploy_conclusions}"; then
        deployed_run_id="${run_id}"
        break
    fi
done <<< "${successful_run_ids}"

[[ -n "${deployed_run_id}" ]] \
    || fail "严格发布门禁已开启，但 tag commit 没有成功的 test Environment 部署"

summary "verified test Environment deployment in workflow run: ${deployed_run_id}"

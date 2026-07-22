#!/usr/bin/env bash
set -euo pipefail

GH_BIN="${GH_BIN:-gh}"
REPOSITORY="${GITHUB_REPOSITORY:-}"
TAG_COMMIT="${TAG_COMMIT:-}"
# 仓库未配置该变量时按 false 处理；只有精确的 true 才开启严格测试部署门禁。
REQUIRE_TEST_DEPLOY="${REQUIRE_TEST_DEPLOY_FOR_RELEASE:-false}"
# tag 与 master push 会分别触发 Release 和 Container workflow；两者并发时，
# Release 必须给正在构建的 commit 镜像留出完成时间，避免把正常构建误判为缺失。
CONTAINER_WAIT_SECONDS="${CONTAINER_WORKFLOW_WAIT_SECONDS:-600}"
CONTAINER_POLL_SECONDS="${CONTAINER_WORKFLOW_POLL_SECONDS:-15}"

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
[[ "${CONTAINER_WAIT_SECONDS}" =~ ^[0-9]+$ ]] \
    || fail "CONTAINER_WORKFLOW_WAIT_SECONDS 必须是非负整数"
[[ "${CONTAINER_POLL_SECONDS}" =~ ^[1-9][0-9]*$ ]] \
    || fail "CONTAINER_WORKFLOW_POLL_SECONDS 必须是正整数"

successful_run_ids=""
container_run_id=""
waited_seconds=0
while [[ -z "${container_run_id}" ]]; do
    successful_run_ids="$("${GH_BIN}" api --method GET \
        "repos/${REPOSITORY}/actions/workflows/container.yml/runs" \
        -f branch=master \
        -f event=push \
        -f status=completed \
        -f head_sha="${TAG_COMMIT}" \
        --jq '.workflow_runs[] | select(.conclusion == "success") | .id')"

    container_run_id="$(printf '%s\n' "${successful_run_ids}" | sed -n '/[^[:space:]]/ { p; q; }')"
    [[ -z "${container_run_id}" ]] || break
    ((waited_seconds < CONTAINER_WAIT_SECONDS)) \
        || fail "tag commit 在 ${CONTAINER_WAIT_SECONDS} 秒内没有成功的 master Container workflow，拒绝晋级容器镜像"

    remaining_seconds=$((CONTAINER_WAIT_SECONDS - waited_seconds))
    sleep_seconds="${CONTAINER_POLL_SECONDS}"
    if ((sleep_seconds > remaining_seconds)); then
        sleep_seconds="${remaining_seconds}"
    fi
    printf '::notice::等待 tag commit 对应的 master Container workflow 完成（已等待 %s 秒）\n' \
        "${waited_seconds}"
    sleep "${sleep_seconds}"
    waited_seconds=$((waited_seconds + sleep_seconds))
done

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

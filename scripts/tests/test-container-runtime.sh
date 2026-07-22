#!/usr/bin/env bash
set -euo pipefail

IMAGE="${1:?Usage: test-container-runtime.sh <image> [basic|full] [amd64|arm64]}"
MODE="${2:-full}"
EXPECTED_ARCH="${3:-}"
ROOT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/qq-maid-container.XXXXXX")"
CONTAINER_NAME="qq-maid-runtime-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-1}-${RANDOM}"
COMPOSE_ENVS=()

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

compose_for() {
    local env_file="$1"
    shift
    docker compose \
        --project-directory "$(dirname -- "${env_file}")" \
        --env-file "${env_file}" \
        -f "${ROOT_DIR}/compose.yaml" \
        "$@"
}

cleanup() {
    local env_file
    set +e
    for env_file in "${COMPOSE_ENVS[@]}"; do
        compose_for "${env_file}" down --volumes --remove-orphans >/dev/null 2>&1
    done
    docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1
    rm -rf "${TEST_ROOT}"
}
trap cleanup EXIT

prepare_instance() {
    local directory="$1"
    install -d \
        "${directory}/config/secrets" \
        "${directory}/data/storage" \
        "${directory}/media/inbound"
    printf '# Container CI 使用空配置进入 setup_required。\n' > "${directory}/config/.env"
    sudo chown -R 10001:10001 \
        "${directory}/config" "${directory}/data" "${directory}/media"
}

run_direct_container() {
    local directory="$1"
    docker run --detach \
        --name "${CONTAINER_NAME}" \
        --read-only \
        --user 10001:10001 \
        --env-file "${directory}/config/.env" \
        --mount "type=bind,src=${directory}/config,dst=/app/runtime/config" \
        --mount "type=bind,src=${directory}/data,dst=/app/runtime/data" \
        --mount "type=bind,src=${directory}/media,dst=/app/runtime/media" \
        --tmpfs /tmp:rw,noexec,nosuid,nodev,size=64m \
        --security-opt no-new-privileges=true \
        --cap-drop ALL \
        --stop-timeout 20 \
        "${IMAGE}" >/dev/null
}

wait_healthy() {
    local container="$1"
    local health=""
    local attempt
    for ((attempt = 1; attempt <= 90; attempt++)); do
        health="$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}missing{{end}}' "${container}")"
        if [[ "${health}" == "healthy" ]]; then
            return 0
        fi
        if [[ "$(docker inspect --format '{{.State.Running}}' "${container}")" != "true" ]]; then
            fail "容器在进入 healthy 前退出"
        fi
        sleep 2
    done
    fail "容器未在超时前进入 healthy，最终状态=${health}"
}

assert_runtime_contract() {
    local container="$1"
    [[ "$(docker inspect --format '{{.HostConfig.ReadonlyRootfs}}' "${container}")" == "true" ]] \
        || fail "根文件系统不是 read_only"
    docker exec "${container}" sh -eu -c '
        test "$(cat /proc/1/comm)" = "qq-maid-bot"
        grep -Eq "^Uid:[[:space:]]+10001[[:space:]]+10001" /proc/1/status
        grep -Eq "^Gid:[[:space:]]+10001[[:space:]]+10001" /proc/1/status
    '
}

assert_clean_stop() {
    local container="$1"
    docker stop --time 20 "${container}" >/dev/null
    [[ "$(docker inspect --format '{{.State.ExitCode}}' "${container}")" == "0" ]] \
        || fail "docker stop 后主进程未正常退出"
    [[ "$(docker inspect --format '{{.State.OOMKilled}}' "${container}")" == "false" ]] \
        || fail "容器被 OOM kill"
}

if [[ "${MODE}" != "basic" && "${MODE}" != "full" ]]; then
    fail "测试模式只能是 basic 或 full"
fi
if [[ -n "${EXPECTED_ARCH}" ]]; then
    [[ "$(docker image inspect --format '{{.Architecture}}' "${IMAGE}")" == "${EXPECTED_ARCH}" ]] \
        || fail "加载镜像架构与预期 ${EXPECTED_ARCH} 不一致"
fi

direct_dir="${TEST_ROOT}/direct"
prepare_instance "${direct_dir}"
run_direct_container "${direct_dir}"
wait_healthy "${CONTAINER_NAME}"
assert_runtime_contract "${CONTAINER_NAME}"

if [[ "${MODE}" == "basic" ]]; then
    assert_clean_stop "${CONTAINER_NAME}"
    printf 'container basic runtime tests passed: image=%s arch=%s\n' "${IMAGE}" "${EXPECTED_ARCH:-unchecked}"
    exit 0
fi

# 由容器内的 10001:10001 用户分别读写三个 bind mount，证明只读根文件系统不会
# 阻止配置、数据库/缓存和媒体目录的正常持久化。
docker exec "${CONTAINER_NAME}" sh -eu -c '
    test -r /app/runtime/config/.env
    printf config > /app/runtime/config/ci-persistent
    printf data > /app/runtime/data/ci-persistent
    printf media > /app/runtime/media/ci-persistent
'
for directory in config data media; do
    [[ -f "${direct_dir}/${directory}/ci-persistent" ]] \
        || fail "${directory} bind mount 未写入宿主机"
done

assert_clean_stop "${CONTAINER_NAME}"
docker rm "${CONTAINER_NAME}" >/dev/null
run_direct_container "${direct_dir}"
wait_healthy "${CONTAINER_NAME}"
docker exec "${CONTAINER_NAME}" sh -eu -c '
    test "$(cat /app/runtime/config/ci-persistent)" = config
    test "$(cat /app/runtime/data/ci-persistent)" = data
    test "$(cat /app/runtime/media/ci-persistent)" = media
'
assert_clean_stop "${CONTAINER_NAME}"
docker rm "${CONTAINER_NAME}" >/dev/null

project_a="qq-maid-ci-a-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-1}"
project_b="qq-maid-ci-b-${GITHUB_RUN_ID:-local}-${GITHUB_RUN_ATTEMPT:-1}"
for project in "${project_a}" "${project_b}"; do
    instance_dir="${TEST_ROOT}/${project}"
    prepare_instance "${instance_dir}"
    env_file="${instance_dir}/compose.env"
    {
        printf 'COMPOSE_PROJECT_NAME=%s\n' "${project}"
        printf 'QQ_MAID_IMAGE=%s\n' "${IMAGE}"
        printf 'QQ_MAID_UID=10001\nQQ_MAID_GID=10001\n'
        printf 'QQ_MAID_ENV_FILE=%s/config/.env\n' "${instance_dir}"
        printf 'QQ_MAID_CONFIG_DIR=%s/config\n' "${instance_dir}"
        printf 'QQ_MAID_DATA_DIR=%s/data\n' "${instance_dir}"
        printf 'QQ_MAID_MEDIA_DIR=%s/media\n' "${instance_dir}"
    } > "${env_file}"
    COMPOSE_ENVS+=("${env_file}")
done

compose_for "${COMPOSE_ENVS[0]}" up -d --wait --wait-timeout 140 bot
compose_for "${COMPOSE_ENVS[1]}" up -d --wait --wait-timeout 140 bot
container_a="$(compose_for "${COMPOSE_ENVS[0]}" ps -q bot)"
container_b="$(compose_for "${COMPOSE_ENVS[1]}" ps -q bot)"
[[ -n "${container_a}" && -n "${container_b}" && "${container_a}" != "${container_b}" ]] \
    || fail "两个 Compose project 未创建独立容器"
[[ "$(docker inspect --format '{{index .Config.Labels "com.docker.compose.project"}}' "${container_a}")" == "${project_a}" ]]
[[ "$(docker inspect --format '{{index .Config.Labels "com.docker.compose.project"}}' "${container_b}")" == "${project_b}" ]]

network_a="$(docker inspect "${container_a}" | jq -r '.[0].NetworkSettings.Networks | keys[]')"
network_b="$(docker inspect "${container_b}" | jq -r '.[0].NetworkSettings.Networks | keys[]')"
[[ -n "${network_a}" && -n "${network_b}" && "${network_a}" != "${network_b}" ]] \
    || fail "两个 Compose project 的网络未隔离"

docker exec "${container_a}" sh -c 'printf a > /app/runtime/data/project-a'
docker exec "${container_b}" sh -c 'printf b > /app/runtime/data/project-b'
docker exec "${container_a}" test ! -e /app/runtime/data/project-b
docker exec "${container_b}" test ! -e /app/runtime/data/project-a
[[ -f "${TEST_ROOT}/${project_a}/data/project-a" ]]
[[ -f "${TEST_ROOT}/${project_b}/data/project-b" ]]

printf 'container full runtime tests passed: image=%s arch=%s\n' "${IMAGE}" "${EXPECTED_ARCH:-unchecked}"

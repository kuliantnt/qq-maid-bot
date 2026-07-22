#!/usr/bin/env bash
set -euo pipefail

# 测试/正式环境只部署受信仓库的 digest，避免可变 tag 在拉取和回滚期间漂移。
EXPECTED_IMAGE_REPOSITORY="${QQ_MAID_EXPECTED_IMAGE_REPOSITORY:-ghcr.io/kuliantnt/qq-maid-bot}"
DOCKER_BIN="${DOCKER_BIN:-docker}"
HEALTH_ATTEMPTS="${DEPLOY_HEALTH_ATTEMPTS:-60}"
HEALTH_INTERVAL_SECONDS="${DEPLOY_HEALTH_INTERVAL_SECONDS:-2}"

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE_FILE="${SCRIPT_DIR}/compose.yaml"
INSTANCE_ENV="${SCRIPT_DIR}/compose.env"
IMAGE_ENV="${SCRIPT_DIR}/.image.env"
STATE_DIR="${SCRIPT_DIR}/deployments"
CURRENT_STATE="${STATE_DIR}/current.env"
PRE_UPGRADE_BACKUP=""

usage() {
    cat <<'EOF'
Usage:
  docker-deploy.sh deploy --image <ghcr.io/...@sha256:...> --commit <40位commit> [--release vX.Y.Z]
  docker-deploy.sh status

环境变量：
  DOCKER_BIN                       Docker 命令路径（测试替身使用）
  QQ_MAID_EXPECTED_IMAGE_REPOSITORY 允许部署的唯一镜像仓库
  DEPLOY_HEALTH_ATTEMPTS           healthy 轮询次数，默认 60
  DEPLOY_HEALTH_INTERVAL_SECONDS   轮询间隔秒数，默认 2
EOF
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

validate_runtime_files() {
    [[ -f "${COMPOSE_FILE}" && ! -L "${COMPOSE_FILE}" ]] \
        || fail "缺少普通文件 ${COMPOSE_FILE}"
    [[ -f "${INSTANCE_ENV}" && ! -L "${INSTANCE_ENV}" ]] \
        || fail "缺少普通文件 ${INSTANCE_ENV}"
    "${DOCKER_BIN}" compose version >/dev/null
}

validate_image_reference() {
    local image="$1"
    local escaped_repository
    escaped_repository="$(printf '%s' "${EXPECTED_IMAGE_REPOSITORY}" | sed 's/[][\\.^$*+?{}|()]/\\&/g')"
    [[ "${image}" =~ ^${escaped_repository}@sha256:[0-9a-f]{64}$ ]] \
        || fail "镜像必须是 ${EXPECTED_IMAGE_REPOSITORY}@sha256:<64位小写digest>"
}

validate_commit() {
    [[ "$1" =~ ^[0-9a-f]{40}$ ]] || fail "commit 必须是 40 位小写十六进制 SHA"
}

validate_release() {
    [[ -z "$1" || "$1" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
        || fail "release 必须为空或稳定版本 vX.Y.Z"
}

read_image_reference() {
    local file="$1"
    local line
    [[ -f "${file}" && ! -L "${file}" ]] || return 1
    line="$(sed -n 's/^QQ_MAID_IMAGE=//p' "${file}" | tail -n 1)"
    [[ -n "${line}" ]] || return 1
    printf '%s\n' "${line}"
}

write_image_env() {
    local file="$1"
    local image="$2"
    umask 077
    printf 'QQ_MAID_IMAGE=%s\n' "${image}" > "${file}"
}

compose_with_image_env() {
    local image_env="$1"
    shift
    "${DOCKER_BIN}" compose \
        --project-directory "${SCRIPT_DIR}" \
        --env-file "${INSTANCE_ENV}" \
        --env-file "${image_env}" \
        -f "${COMPOSE_FILE}" \
        "$@"
}

container_id() {
    local image_env="$1"
    compose_with_image_env "${image_env}" ps -q bot
}

container_health() {
    local id="$1"
    "${DOCKER_BIN}" inspect \
        --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}missing{{end}}' \
        "${id}" 2>/dev/null
}

wait_until_healthy() {
    local image_env="$1"
    local attempt id health
    for ((attempt = 1; attempt <= HEALTH_ATTEMPTS; attempt++)); do
        id="$(container_id "${image_env}")"
        if [[ -n "${id}" ]]; then
            health="$(container_health "${id}" || true)"
            if [[ "${health}" == "healthy" ]]; then
                printf '%s\n' "${id}"
                return 0
            fi
            if [[ "${health}" == "unhealthy" ]]; then
                printf 'warning: 容器 %s 已进入 unhealthy\n' "${id}" >&2
                return 1
            fi
        fi
        sleep "${HEALTH_INTERVAL_SECONDS}"
    done
    printf 'warning: 等待容器 healthy 超时\n' >&2
    return 1
}

image_label() {
    local image="$1"
    local label="$2"
    "${DOCKER_BIN}" image inspect \
        --format "{{ index .Config.Labels \"${label}\" }}" \
        "${image}"
}

recorded_release_for_image() {
    local image="$1"
    local recorded_image release
    [[ -f "${CURRENT_STATE}" && ! -L "${CURRENT_STATE}" ]] || return 1
    recorded_image="$(sed -n 's/^image=//p' "${CURRENT_STATE}" | tail -n 1)"
    [[ "${recorded_image}" == "${image}" ]] || return 1
    release="$(sed -n 's/^release=//p' "${CURRENT_STATE}" | tail -n 1)"
    [[ "${release}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || return 1
    printf '%s\n' "${release}"
}

record_state() {
    local image="$1"
    local commit="$2"
    local container="$3"
    local release="$4"
    local build_version_label deployed_at tmp
    build_version_label="$(image_label "${image}" org.opencontainers.image.version)"
    deployed_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    install -d -m 0755 "${STATE_DIR}"
    tmp="${CURRENT_STATE}.new"
    umask 022
    {
        printf 'image=%s\n' "${image}"
        printf 'commit=%s\n' "${commit}"
        printf 'release=%s\n' "${release:-unreleased}"
        # commit 镜像在 tag 晋级前已经构建；该 OCI label 只能描述构建时版本，
        # 不能把 sha-* 值当成之后追加的正式发布版本。
        printf 'build_version_label=%s\n' "${build_version_label}"
        printf 'deployed_at=%s\n' "${deployed_at}"
        printf 'container_id=%s\n' "${container}"
        printf 'pre_upgrade_backup=%s\n' "${PRE_UPGRADE_BACKUP:-none}"
    } > "${tmp}"
    mv -f "${tmp}" "${CURRENT_STATE}"
}

create_pre_upgrade_backup() {
    local target_env="$1"
    local commit="$2"
    local stamp backup
    if [[ "${DEPLOY_BACKUP_BEFORE_UPGRADE:-true}" == "false" ]]; then
        printf 'warning: 已显式关闭升级前备份；schema 变化后镜像回滚可能不可用\n' >&2
        return 0
    fi
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    backup="/app/runtime/data/backups/pre-upgrade-${stamp}-${commit:0:12}"
    printf '==> 创建升级前数据库与配置恢复包 %s\n' "${backup}" >&2
    if ! compose_with_image_env "${target_env}" run --rm --no-deps bot \
        backup create --output "${backup}" --include-secrets >&2; then
        printf 'error: 升级前备份失败，保持当前镜像和数据不变\n' >&2
        return 1
    fi
    PRE_UPGRADE_BACKUP="${backup}"
}

show_status() {
    local image id health revision release build_version_label
    validate_runtime_files
    image="$(read_image_reference "${IMAGE_ENV}")" \
        || fail "尚未记录已部署镜像"
    validate_image_reference "${image}"
    id="$(container_id "${IMAGE_ENV}")"
    [[ -n "${id}" ]] || fail "Compose 服务 bot 当前没有容器"
    health="$(container_health "${id}" || true)"
    revision="$(image_label "${image}" org.opencontainers.image.revision)"
    release="$(recorded_release_for_image "${image}" || true)"
    build_version_label="$(image_label "${image}" org.opencontainers.image.version)"
    printf 'image=%s\ncommit=%s\nrelease=%s\nbuild_version_label=%s\ncontainer_id=%s\nhealth=%s\n' \
        "${image}" "${revision}" "${release:-unrecorded}" "${build_version_label}" \
        "${id}" "${health:-unknown}"
    "${DOCKER_BIN}" stats --no-stream \
        --format 'resources={{.CPUPerc}} cpu, {{.MemUsage}} memory' "${id}" || true
}

rollback() {
    local previous_image="$1"
    local previous_commit previous_container previous_release restore_tmp
    if [[ -z "${previous_image}" ]]; then
        printf 'error: 首次部署失败，没有上一镜像可回滚\n' >&2
        return 1
    fi

    printf '==> 恢复上一镜像 %s\n' "${previous_image}" >&2
    validate_image_reference "${previous_image}"
    restore_tmp="${IMAGE_ENV}.rollback"
    write_image_env "${restore_tmp}" "${previous_image}"
    mv -f "${restore_tmp}" "${IMAGE_ENV}"
    if ! compose_with_image_env "${IMAGE_ENV}" pull bot; then
        printf 'warning: 无法重新拉取上一镜像，将尝试使用本地缓存\n' >&2
    fi
    compose_with_image_env "${IMAGE_ENV}" up -d --remove-orphans --pull never bot
    previous_container="$(wait_until_healthy "${IMAGE_ENV}")" || {
        printf 'error: 上一镜像恢复后仍未进入 healthy\n' >&2
        return 1
    }
    previous_commit="$(image_label "${previous_image}" org.opencontainers.image.revision)"
    previous_release="$(recorded_release_for_image "${previous_image}" || true)"
    record_state "${previous_image}" "${previous_commit}" "${previous_container}" "${previous_release}"
    printf '==> 已恢复上一镜像并确认 healthy\n' >&2
}

deploy() {
    local image="$1"
    local commit="$2"
    local release="$3"
    local current_image=""
    local target_env target_revision new_container

    validate_runtime_files
    validate_image_reference "${image}"
    validate_commit "${commit}"
    validate_release "${release}"
    install -d -m 0755 "${STATE_DIR}"

    current_image="$(read_image_reference "${IMAGE_ENV}" || true)"
    if [[ -z "${release}" && "${current_image}" == "${image}" ]]; then
        release="$(recorded_release_for_image "${image}" || true)"
    fi
    if [[ "${current_image}" == "${image}" ]]; then
        if new_container="$(wait_until_healthy "${IMAGE_ENV}")"; then
            printf '==> 目标 digest 已在运行且 healthy，无需重建\n'
            record_state "${image}" "${commit}" "${new_container}" "${release}"
            return 0
        fi
        printf 'warning: 相同 digest 当前不健康，将重新创建容器\n' >&2
    fi

    target_env="${IMAGE_ENV}.target"
    trap 'rm -f "${IMAGE_ENV}.target" "${IMAGE_ENV}.rollback"' EXIT
    write_image_env "${target_env}" "${image}"

    compose_with_image_env "${target_env}" config --quiet
    printf '==> 拉取目标镜像 %s\n' "${image}"
    compose_with_image_env "${target_env}" pull bot

    target_revision="$(image_label "${image}" org.opencontainers.image.revision)"
    [[ "${target_revision}" == "${commit}" ]] \
        || fail "镜像 revision label (${target_revision}) 与部署 commit (${commit}) 不一致"

    if [[ -n "${current_image}" ]]; then
        create_pre_upgrade_backup "${target_env}" "${commit}" || return 1
    fi

    mv -f "${target_env}" "${IMAGE_ENV}"
    printf '==> 使用目标 digest 重建容器\n'
    if ! compose_with_image_env "${IMAGE_ENV}" up -d --remove-orphans --pull never bot; then
        printf 'error: 目标镜像启动命令失败，开始回滚\n' >&2
        rollback "${current_image}" || true
        return 1
    fi

    if ! new_container="$(wait_until_healthy "${IMAGE_ENV}")"; then
        printf 'error: 目标镜像未通过健康检查，开始回滚\n' >&2
        rollback "${current_image}" || true
        if [[ -n "${PRE_UPGRADE_BACKUP}" ]]; then
            printf 'error: 若旧镜像因新 schema 无法启动，请从备份 %s 恢复到干净实例目录\n' \
                "${PRE_UPGRADE_BACKUP}" >&2
        fi
        return 1
    fi

    record_state "${image}" "${commit}" "${new_container}" "${release}"
    printf '==> 部署成功：commit=%s image=%s\n' "${commit}" "${image}"
    "${DOCKER_BIN}" stats --no-stream \
        --format 'resources={{.CPUPerc}} cpu, {{.MemUsage}} memory' "${new_container}" || true
}

command="${1:-}"
case "${command}" in
    status)
        [[ $# -eq 1 ]] || fail "status 不接受额外参数"
        show_status
        ;;
    deploy)
        shift
        image=""
        commit=""
        release=""
        while [[ $# -gt 0 ]]; do
            case "$1" in
                --image)
                    shift
                    [[ $# -gt 0 ]] || fail "--image 缺少参数"
                    image="$1"
                    ;;
                --commit)
                    shift
                    [[ $# -gt 0 ]] || fail "--commit 缺少参数"
                    commit="$1"
                    ;;
                --release)
                    shift
                    [[ $# -gt 0 ]] || fail "--release 缺少参数"
                    release="$1"
                    ;;
                -h|--help)
                    usage
                    exit 0
                    ;;
                *)
                    fail "未知参数: $1"
                    ;;
            esac
            shift
        done
        [[ -n "${image}" ]] || fail "deploy 需要 --image"
        [[ -n "${commit}" ]] || fail "deploy 需要 --commit"
        deploy "${image}" "${commit}" "${release}"
        ;;
    -h|--help)
        usage
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

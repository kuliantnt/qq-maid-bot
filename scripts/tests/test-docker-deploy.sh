#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
TEST_DIR="$(mktemp -d "${TMPDIR:-/tmp}/qq-maid-docker-deploy.XXXXXX")"
trap 'rm -rf "${TEST_DIR}"' EXIT

TARGET_COMMIT="1111111111111111111111111111111111111111"
OLD_COMMIT="2222222222222222222222222222222222222222"
TARGET_IMAGE="ghcr.io/kuliantnt/qq-maid-bot@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
OLD_IMAGE="ghcr.io/kuliantnt/qq-maid-bot@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"

FAKE_DOCKER="${TEST_DIR}/docker"
cat > "${FAKE_DOCKER}" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >> "${FAKE_DOCKER_LOG}"

if [[ "${1:-}" == "compose" && "${2:-}" == "version" ]]; then
    printf 'Docker Compose version v2.test\n'
    exit 0
fi

if [[ "${1:-}" == "compose" ]]; then
    action=""
    image_env=""
    previous=""
    for arg in "$@"; do
        if [[ "${previous}" == "--env-file" ]]; then
            image_env="${arg}"
        fi
        case "${arg}" in
            config|pull|up|ps) action="${arg}" ;;
        esac
        previous="${arg}"
    done
    case "${action}" in
        config) exit 0 ;;
        pull)
            [[ "${FAKE_DOCKER_MODE:-success}" != "pull-fail" ]]
            exit
            ;;
        up)
            target_image="$(sed -n 's/^QQ_MAID_IMAGE=//p' "${image_env}")"
            if [[ "${FAKE_DOCKER_MODE:-success}" == "up-fail" && "${target_image}" == "${FAKE_TARGET_IMAGE}" ]]; then
                exit 1
            fi
            sed -n 's/^QQ_MAID_IMAGE=//p' "${image_env}" > "${FAKE_DOCKER_STATE}/active-image"
            exit 0
            ;;
        ps)
            [[ -f "${FAKE_DOCKER_STATE}/active-image" ]] && printf 'fake-container\n'
            exit 0
            ;;
    esac
fi

if [[ "${1:-}" == "image" && "${2:-}" == "inspect" ]]; then
    image="${*: -1}"
    if [[ "$*" == *org.opencontainers.image.revision* ]]; then
        if [[ "${image}" == "${FAKE_TARGET_IMAGE}" ]]; then
            printf '%s\n' "${FAKE_TARGET_COMMIT}"
        else
            printf '%s\n' "${FAKE_OLD_COMMIT}"
        fi
    else
        printf 'sha-test\n'
    fi
    exit 0
fi

if [[ "${1:-}" == "inspect" ]]; then
    active="$(cat "${FAKE_DOCKER_STATE}/active-image" 2>/dev/null || true)"
    if [[ "${FAKE_DOCKER_MODE:-success}" == "health-fail" && "${active}" == "${FAKE_TARGET_IMAGE}" ]]; then
        printf 'unhealthy\n'
    else
        printf 'healthy\n'
    fi
    exit 0
fi

if [[ "${1:-}" == "stats" ]]; then
    printf 'resources=0.00%% cpu, 20MiB / 2GiB memory\n'
    exit 0
fi

printf 'unexpected fake docker call: %s\n' "$*" >&2
exit 2
EOF
chmod +x "${FAKE_DOCKER}"

new_instance() {
    local name="$1"
    local directory="${TEST_DIR}/${name}"
    mkdir -p "${directory}/runtime/config" "${directory}/runtime/data" "${directory}/runtime/media"
    cp "${ROOT_DIR}/scripts/docker-deploy.sh" "${directory}/docker-deploy.sh"
    cp "${ROOT_DIR}/compose.yaml" "${directory}/compose.yaml"
    cat > "${directory}/compose.env" <<EOF
COMPOSE_PROJECT_NAME=${name}
QQ_MAID_ENV_FILE=${directory}/runtime/config/.env
QQ_MAID_CONFIG_DIR=${directory}/runtime/config
QQ_MAID_DATA_DIR=${directory}/runtime/data
QQ_MAID_MEDIA_DIR=${directory}/runtime/media
EOF
    : > "${directory}/runtime/config/.env"
    mkdir -p "${directory}/fake-state"
    printf '%s\n' "${directory}"
}

run_deploy() {
    local directory="$1"
    local mode="$2"
    local image="$3"
    local commit="$4"
    local release="${5:-}"
    local args=(deploy --image "${image}" --commit "${commit}")
    if [[ -n "${release}" ]]; then
        args+=(--release "${release}")
    fi
    DOCKER_BIN="${FAKE_DOCKER}" \
    FAKE_DOCKER_LOG="${directory}/fake-docker.log" \
    FAKE_DOCKER_STATE="${directory}/fake-state" \
    FAKE_DOCKER_MODE="${mode}" \
    FAKE_TARGET_IMAGE="${TARGET_IMAGE}" \
    FAKE_TARGET_COMMIT="${TARGET_COMMIT}" \
    FAKE_OLD_COMMIT="${OLD_COMMIT}" \
    DEPLOY_HEALTH_ATTEMPTS=2 \
    DEPLOY_HEALTH_INTERVAL_SECONDS=0 \
        bash "${directory}/docker-deploy.sh" "${args[@]}"
}

run_status() {
    local directory="$1"
    DOCKER_BIN="${FAKE_DOCKER}" \
    FAKE_DOCKER_LOG="${directory}/fake-docker.log" \
    FAKE_DOCKER_STATE="${directory}/fake-state" \
    FAKE_DOCKER_MODE=success \
    FAKE_TARGET_IMAGE="${TARGET_IMAGE}" \
    FAKE_TARGET_COMMIT="${TARGET_COMMIT}" \
    FAKE_OLD_COMMIT="${OLD_COMMIT}" \
        bash "${directory}/docker-deploy.sh" status
}

success_dir="$(new_instance success)"
run_deploy "${success_dir}" success "${TARGET_IMAGE}" "${TARGET_COMMIT}" v1.2.3
grep -Fqx "QQ_MAID_IMAGE=${TARGET_IMAGE}" "${success_dir}/.image.env"
grep -Fqx "commit=${TARGET_COMMIT}" "${success_dir}/deployments/current.env"
grep -Fqx "release=v1.2.3" "${success_dir}/deployments/current.env"
grep -Fqx "build_version_label=sha-test" "${success_dir}/deployments/current.env"

up_count_before="$(grep -c ' compose .* up ' "${success_dir}/fake-docker.log" || true)"
run_deploy "${success_dir}" success "${TARGET_IMAGE}" "${TARGET_COMMIT}"
up_count_after="$(grep -c ' compose .* up ' "${success_dir}/fake-docker.log" || true)"
[[ "${up_count_before}" == "${up_count_after}" ]]
status_output="$(run_status "${success_dir}")"
grep -Fqx "release=v1.2.3" <<< "${status_output}"
grep -Fqx "build_version_label=sha-test" <<< "${status_output}"
if grep -Eq '^version=' <<< "${status_output}"; then
    echo "status must not call the build-time sha label a release version" >&2
    exit 1
fi

rollback_dir="$(new_instance rollback)"
printf 'QQ_MAID_IMAGE=%s\n' "${OLD_IMAGE}" > "${rollback_dir}/.image.env"
printf '%s\n' "${OLD_IMAGE}" > "${rollback_dir}/fake-state/active-image"
if run_deploy "${rollback_dir}" health-fail "${TARGET_IMAGE}" "${TARGET_COMMIT}"; then
    echo "expected health failure" >&2
    exit 1
fi
grep -Fqx "QQ_MAID_IMAGE=${OLD_IMAGE}" "${rollback_dir}/.image.env"
grep -Fqx "commit=${OLD_COMMIT}" "${rollback_dir}/deployments/current.env"

up_fail_dir="$(new_instance up-fail)"
printf 'QQ_MAID_IMAGE=%s\n' "${OLD_IMAGE}" > "${up_fail_dir}/.image.env"
printf '%s\n' "${OLD_IMAGE}" > "${up_fail_dir}/fake-state/active-image"
if run_deploy "${up_fail_dir}" up-fail "${TARGET_IMAGE}" "${TARGET_COMMIT}"; then
    echo "expected compose up failure" >&2
    exit 1
fi
grep -Fqx "QQ_MAID_IMAGE=${OLD_IMAGE}" "${up_fail_dir}/.image.env"

pull_fail_dir="$(new_instance pull-fail)"
printf 'QQ_MAID_IMAGE=%s\n' "${OLD_IMAGE}" > "${pull_fail_dir}/.image.env"
printf '%s\n' "${OLD_IMAGE}" > "${pull_fail_dir}/fake-state/active-image"
if run_deploy "${pull_fail_dir}" pull-fail "${TARGET_IMAGE}" "${TARGET_COMMIT}"; then
    echo "expected image pull failure" >&2
    exit 1
fi
grep -Fqx "QQ_MAID_IMAGE=${OLD_IMAGE}" "${pull_fail_dir}/.image.env"

invalid_dir="$(new_instance invalid)"
if run_deploy "${invalid_dir}" success "ghcr.io/kuliantnt/qq-maid-bot:master" "${TARGET_COMMIT}"; then
    echo "expected mutable tag rejection" >&2
    exit 1
fi
if run_deploy "${invalid_dir}" success "${TARGET_IMAGE}" "${TARGET_COMMIT}" sha-test; then
    echo "expected invalid release rejection" >&2
    exit 1
fi

printf 'docker deploy script tests passed\n'

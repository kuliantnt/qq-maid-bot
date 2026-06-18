#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${DIST_DIR:-${REPO_DIR}/dist}"
TARGET_TRIPLE="${TARGET_TRIPLE:-linux-x86_64}"
VERSION="${1:-${GITHUB_REF_NAME:-dev}}"

PACKAGE_NAME="qq-maid-bot-${VERSION}-${TARGET_TRIPLE}"
STAGING_DIR="${DIST_DIR}/${PACKAGE_NAME}"
ARCHIVE_PATH="${DIST_DIR}/${PACKAGE_NAME}.tar.gz"
SHA256_PATH="${ARCHIVE_PATH}.sha256"

die() {
    echo "error: $*" >&2
    exit 1
}

copy_file() {
    local src="$1"
    local dst="$2"
    [[ -f "${src}" ]] || die "required file not found: ${src}"
    install -m 0644 "${src}" "${dst}"
}

copy_executable() {
    local src="$1"
    local dst="$2"
    [[ -f "${src}" ]] || die "required executable not found: ${src}"
    install -m 0755 "${src}" "${dst}"
}

assert_no_private_runtime_file() {
    local relative="$1"

    case "${relative}" in
        runtime/.env.example|runtime/README.md|runtime/config/*.example.*|runtime/config/prompts/*.example.*)
            return 0
            ;;
    esac

    die "refuse to package non-example runtime file: ${relative}"
}

check_archive_contents() {
    local listing
    listing="$(tar -tzf "${ARCHIVE_PATH}")"

    printf '%s\n' "${listing}"

    if printf '%s\n' "${listing}" | grep -E '(^|/)\.env$|(^|/)app\.db$|(^|/)[^/]*\.db$|(^|/)logs/|(^|/)run/.*\.pid$' >/dev/null; then
        die "archive contains forbidden runtime files"
    fi

    if ! printf '%s\n' "${listing}" | grep -Fx "${PACKAGE_NAME}/.env.example" >/dev/null; then
        die "archive missing .env.example"
    fi
}

main() {
    cd "${REPO_DIR}"

    [[ -f target/release/qq-maid-llm ]] || die "missing target/release/qq-maid-llm; run cargo build --workspace --release first"
    [[ -f target/release/qq-maid-gateway-rs ]] || die "missing target/release/qq-maid-gateway-rs; run cargo build --workspace --release first"

    rm -rf "${STAGING_DIR}" "${ARCHIVE_PATH}" "${SHA256_PATH}"
    mkdir -p "${STAGING_DIR}/config" "${STAGING_DIR}/data/storage"

    copy_executable target/release/qq-maid-llm "${STAGING_DIR}/qq-maid-llm"
    copy_executable target/release/qq-maid-gateway-rs "${STAGING_DIR}/qq-maid-gateway-rs"
    copy_executable scripts/llmctl.sh "${STAGING_DIR}/llmctl.sh"
    copy_executable scripts/gatewayctl.sh "${STAGING_DIR}/gatewayctl.sh"
    copy_executable scripts/diagnose-network.sh "${STAGING_DIR}/diagnose-network.sh"
    copy_file runtime/README.md "${STAGING_DIR}/README.md"
    copy_file runtime/.env.example "${STAGING_DIR}/.env.example"

    while IFS= read -r tracked_file; do
        assert_no_private_runtime_file "${tracked_file}"
        target_path="${STAGING_DIR}/${tracked_file#runtime/}"
        mkdir -p "$(dirname -- "${target_path}")"
        copy_file "${tracked_file}" "${target_path}"
    done < <(git ls-files 'runtime/config')

    # 预置 SQLite 父目录，避免首次使用默认 APP_DB_FILE 时缺少 data/storage。
    # logs/ 和 run/ 由控制脚本启动时创建，不写进归档以避免混入运行产物。
    : > "${STAGING_DIR}/data/storage/.gitkeep"

    printf '%s\n' "${VERSION}" > "${STAGING_DIR}/VERSION"

    tar -C "${DIST_DIR}" -czf "${ARCHIVE_PATH}" "${PACKAGE_NAME}"
    (
        cd "${DIST_DIR}"
        sha256sum "$(basename -- "${ARCHIVE_PATH}")" > "$(basename -- "${SHA256_PATH}")"
        sha256sum -c "$(basename -- "${SHA256_PATH}")"
    )

    check_archive_contents

    test -x "${STAGING_DIR}/qq-maid-llm"
    test -x "${STAGING_DIR}/qq-maid-gateway-rs"
    test -x "${STAGING_DIR}/llmctl.sh"
    test -x "${STAGING_DIR}/gatewayctl.sh"
    test -x "${STAGING_DIR}/diagnose-network.sh"

    printf 'created %s\n' "${ARCHIVE_PATH}"
    printf 'created %s\n' "${SHA256_PATH}"
}

main "$@"

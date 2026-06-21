#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${DIST_DIR:-${REPO_DIR}/dist}"
# TARGET_TRIPLE：面向用户的平台名称，用于命名发布包（如 linux-x86_64、windows-x86_64）。
TARGET_TRIPLE="${TARGET_TRIPLE:-linux-x86_64}"
# BUILD_TARGET：Rust target triple，用于定位 target/<triple>/release/ 下的二进制文件。
# 留空时默认为原生构建，直接从 target/release/ 读取。
BUILD_TARGET="${BUILD_TARGET:-}"
# ARCHIVE_FORMAT：发布包格式，默认为 tar.gz；Windows 平台传入 zip。
ARCHIVE_FORMAT="${ARCHIVE_FORMAT:-tar.gz}"
VERSION="${1:-${GITHUB_REF_NAME:-dev}}"

PACKAGE_NAME="qq-maid-bot-${VERSION}-${TARGET_TRIPLE}"
STAGING_DIR="${DIST_DIR}/${PACKAGE_NAME}"
ARCHIVE_PATH="${DIST_DIR}/${PACKAGE_NAME}.${ARCHIVE_FORMAT}"
SHA256_PATH="${ARCHIVE_PATH}.sha256"

# 构建产物目录：跨编译时位于 target/<triple>/release/，原生编译时位于 target/release/。
if [[ -n "${BUILD_TARGET}" ]]; then
    BUILD_DIR="${REPO_DIR}/target/${BUILD_TARGET}/release"
else
    BUILD_DIR="${REPO_DIR}/target/release"
fi

# Windows 可执行文件后缀
EXE_SUFFIX=""
if [[ "${ARCHIVE_FORMAT}" == "zip" ]]; then
    EXE_SUFFIX=".exe"
fi

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

    for required in \
        "${PACKAGE_NAME}/.env.example" \
        "${PACKAGE_NAME}/static/index.html" \
        "${PACKAGE_NAME}/botctl.sh" \
        "${PACKAGE_NAME}/validate-runtime.sh"
    do
        if ! printf '%s\n' "${listing}" | grep -Fx "${required}" >/dev/null; then
            die "archive missing ${required#${PACKAGE_NAME}/}"
        fi
    done
}

main() {
    cd "${REPO_DIR}"

    [[ -f "${BUILD_DIR}/qq-maid-llm${EXE_SUFFIX}" ]] || die "missing ${BUILD_DIR}/qq-maid-llm${EXE_SUFFIX}; run cargo build --release first"
    [[ -f "${BUILD_DIR}/qq-maid-gateway-rs${EXE_SUFFIX}" ]] || die "missing ${BUILD_DIR}/qq-maid-gateway-rs${EXE_SUFFIX}; run cargo build --release first"

    rm -rf "${STAGING_DIR}" "${ARCHIVE_PATH}" "${SHA256_PATH}"
    mkdir -p "${STAGING_DIR}/config" "${STAGING_DIR}/data/storage" "${STAGING_DIR}/static"

    copy_executable "${BUILD_DIR}/qq-maid-llm${EXE_SUFFIX}" "${STAGING_DIR}/qq-maid-llm${EXE_SUFFIX}"
    copy_executable "${BUILD_DIR}/qq-maid-gateway-rs${EXE_SUFFIX}" "${STAGING_DIR}/qq-maid-gateway-rs${EXE_SUFFIX}"
    copy_executable scripts/llmctl.sh "${STAGING_DIR}/llmctl.sh"
    copy_executable scripts/gatewayctl.sh "${STAGING_DIR}/gatewayctl.sh"
    copy_executable scripts/botctl.sh "${STAGING_DIR}/botctl.sh"
    copy_executable scripts/diagnose-network.sh "${STAGING_DIR}/diagnose-network.sh"
    copy_executable scripts/validate-runtime.sh "${STAGING_DIR}/validate-runtime.sh"
    copy_file runtime/README.md "${STAGING_DIR}/README.md"
    copy_file runtime/.env.example "${STAGING_DIR}/.env.example"
    copy_file runtime/static/index.html "${STAGING_DIR}/static/index.html"

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

    case "${ARCHIVE_FORMAT}" in
        zip)
            # 进入 staging 父目录，用 zip 打包，确保解压后只有包名一层目录。
            (
                cd "${DIST_DIR}"
                zip -rq "${PACKAGE_NAME}.zip" "${PACKAGE_NAME}"
                sha256sum "$(basename -- "${ARCHIVE_PATH}")" > "$(basename -- "${SHA256_PATH}")"
                sha256sum -c "$(basename -- "${SHA256_PATH}")"
            )
            # 检查 zip 内容，避免混入敏感文件。
            zip_listing="$(unzip -l "${ARCHIVE_PATH}")"
            printf '%s\n' "${zip_listing}"
            if printf '%s\n' "${zip_listing}" | grep -E '(^|[ /])\.env$|(^|[ /])app\.db$|(^|[ /])[^/]*\.db$|(^|[ /])logs/|(^|[ /])run/.*\.pid$' >/dev/null; then
                die "archive contains forbidden runtime files"
            fi
            for required in ".env.example" "static/index.html" "botctl.sh" "validate-runtime.sh"; do
                if ! printf '%s\n' "${zip_listing}" | grep -F "${PACKAGE_NAME}/${required}" >/dev/null; then
                    die "archive missing ${required}"
                fi
            done
            ;;
        *)
            tar -C "${DIST_DIR}" -czf "${ARCHIVE_PATH}" "${PACKAGE_NAME}"
            (
                cd "${DIST_DIR}"
                sha256sum "$(basename -- "${ARCHIVE_PATH}")" > "$(basename -- "${SHA256_PATH}")"
                sha256sum -c "$(basename -- "${SHA256_PATH}")"
            )
            check_archive_contents
            ;;
    esac

    test -x "${STAGING_DIR}/qq-maid-llm${EXE_SUFFIX}"
    test -x "${STAGING_DIR}/qq-maid-gateway-rs${EXE_SUFFIX}"
    test -x "${STAGING_DIR}/llmctl.sh"
    test -x "${STAGING_DIR}/gatewayctl.sh"
    test -x "${STAGING_DIR}/botctl.sh"
    test -x "${STAGING_DIR}/diagnose-network.sh"
    test -x "${STAGING_DIR}/validate-runtime.sh"
    test -f "${STAGING_DIR}/static/index.html"

    printf 'created %s\n' "${ARCHIVE_PATH}"
    printf 'created %s\n' "${SHA256_PATH}"
}

main "$@"

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
DETECTED_PAYLOAD_PROFILE="unix"
case "${TARGET_TRIPLE}" in
    windows-*)
        EXE_SUFFIX=".exe"
        DETECTED_PAYLOAD_PROFILE="windows"
        ;;
esac
PAYLOAD_PROFILE="${PAYLOAD_PROFILE:-${DETECTED_PAYLOAD_PROFILE}}"

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
        # 发布包只允许带 `.example.*` 的公开模板进入 runtime/config；
        # knowledge/ 子目录新增后也要显式放行，否则 tag 打包会误判为私有文件。
        runtime/config/.env.example|runtime/README.md|runtime/config/*.example.*|runtime/config/prompts/*.example.*|runtime/config/knowledge/*.example.*|runtime/config/knowledge/**/*.example.*)
            return 0
            ;;
    esac

    die "refuse to package non-example runtime file: ${relative}"
}

check_archive_contents() {
    local listing
    listing="$(tar -tzf "${ARCHIVE_PATH}")"

    printf '%s\n' "${listing}"

    if printf '%s\n' "${listing}" | grep -E '(^|/)\.env$|(^|/)runtime\.toml$|(^|/)master\.key$|(^|/)config/secrets/|(^|/)app\.db$|(^|/)[^/]*\.db$|(^|/)logs/|(^|/)run/.*\.pid$' >/dev/null; then
        die "archive contains forbidden runtime files"
    fi

    for required in "${REQUIRED_PAYLOAD_FILES[@]}"; do
        required="${PACKAGE_NAME}/${required}"
        if ! printf '%s\n' "${listing}" | grep -Fx "${required}" >/dev/null; then
            die "archive missing ${required#${PACKAGE_NAME}/}"
        fi
    done

    if printf '%s\n' "${listing}" | grep -E '\.(bat|cmd|ps1)$' >/dev/null; then
        die "Unix archive contains Windows control scripts"
    fi
    if printf '%s\n' "${listing}" | grep -Fx "${PACKAGE_NAME}/.env.example" >/dev/null; then
        die "archive contains legacy root .env.example"
    fi
}

main() {
    cd "${REPO_DIR}"

    [[ "${PAYLOAD_PROFILE}" == "${DETECTED_PAYLOAD_PROFILE}" ]] || \
        die "payload profile ${PAYLOAD_PROFILE} does not match target ${TARGET_TRIPLE}"
    [[ -f "${BUILD_DIR}/qq-maid-bot${EXE_SUFFIX}" ]] || die "missing ${BUILD_DIR}/qq-maid-bot${EXE_SUFFIX}; run cargo build --release first"

    rm -rf "${STAGING_DIR}" "${ARCHIVE_PATH}" "${SHA256_PATH}"
    mkdir -p "${STAGING_DIR}/config" "${STAGING_DIR}/data/storage" "${STAGING_DIR}/lib"

    copy_executable "${BUILD_DIR}/qq-maid-bot${EXE_SUFFIX}" "${STAGING_DIR}/qq-maid-bot${EXE_SUFFIX}"
    copy_file runtime/README.md "${STAGING_DIR}/README.md"

    if [[ "${PAYLOAD_PROFILE}" == "windows" ]]; then
        copy_file scripts/qbot.ps1 "${STAGING_DIR}/qbot.ps1"
        copy_file scripts/qbot.cmd "${STAGING_DIR}/qbot.cmd"
        copy_file scripts/lib/agent-config.ps1 "${STAGING_DIR}/lib/agent-config.ps1"
        copy_file scripts/botctl.ps1 "${STAGING_DIR}/botctl.ps1"
        copy_file scripts/botctl.cmd "${STAGING_DIR}/botctl.cmd"
        copy_file scripts/windows-startup-example.bat "${STAGING_DIR}/windows-startup-example.bat"
    else
        copy_file scripts/lib/agent-config.sh "${STAGING_DIR}/lib/agent-config.sh"
        copy_executable scripts/botctl.sh "${STAGING_DIR}/botctl.sh"
        copy_executable scripts/botmon.sh "${STAGING_DIR}/botmon.sh"
        copy_executable scripts/diagnose-network.sh "${STAGING_DIR}/diagnose-network.sh"
        copy_executable scripts/validate-runtime.sh "${STAGING_DIR}/validate-runtime.sh"
        copy_executable scripts/qq-maid-healthcheck.sh "${STAGING_DIR}/qq-maid-healthcheck.sh"
        copy_executable scripts/qq-maid-systemd.sh "${STAGING_DIR}/qq-maid-systemd.sh"
    fi

    while IFS= read -r tracked_file; do
        assert_no_private_runtime_file "${tracked_file}"
        target_path="${STAGING_DIR}/${tracked_file#runtime/}"
        mkdir -p "$(dirname -- "${target_path}")"
        copy_file "${tracked_file}" "${target_path}"
    done < <(git ls-files 'runtime/config')

    # 预置 SQLite 父目录，避免首次使用默认 APP_DB_FILE 时缺少 data/storage。
    # logs/ 和 run/ 由控制脚本启动时创建，不写进归档以避免混入运行产物。
    : > "${STAGING_DIR}/data/storage/.gitkeep"

    # 归档前先用统一 helper 校验 staging 目录，避免 deploy/package 两条链路的
    # 文件完整性约束出现漂移。
    bash scripts/validate-release-runtime.sh "${STAGING_DIR}" "${PAYLOAD_PROFILE}"

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
            zip_listing="$(unzip -Z1 "${ARCHIVE_PATH}")"
            printf '%s\n' "${zip_listing}"
            if printf '%s\n' "${zip_listing}" | grep -E '(^|/)\.env$|(^|/)runtime\.toml$|(^|/)master\.key$|(^|/)config/secrets/|(^|/)app\.db$|(^|/)[^/]*\.db$|(^|/)logs/|(^|/)run/.*\.pid$' >/dev/null; then
                die "archive contains forbidden runtime files"
            fi
            for required in "${REQUIRED_PAYLOAD_FILES[@]}"; do
                if ! printf '%s\n' "${zip_listing}" | grep -Fx "${PACKAGE_NAME}/${required}" >/dev/null; then
                    die "archive missing ${required}"
                fi
            done
            if printf '%s\n' "${zip_listing}" | grep -E '\.sh$' >/dev/null; then
                die "Windows archive contains shell scripts"
            fi
            if printf '%s\n' "${zip_listing}" | grep -Fx "${PACKAGE_NAME}/.env.example" >/dev/null; then
                die "archive contains legacy root .env.example"
            fi
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

    test -x "${STAGING_DIR}/qq-maid-bot${EXE_SUFFIX}"

    printf 'created %s\n' "${ARCHIVE_PATH}"
    printf 'created %s\n' "${SHA256_PATH}"
}

if [[ "${PAYLOAD_PROFILE}" == "windows" ]]; then
    REQUIRED_PAYLOAD_FILES=(
        "config/.env.example"
        "config/agent.example.toml"
        "config/ops.example.toml"
        "config/runtime.example.toml"
        "qbot.ps1"
        "qbot.cmd"
        "lib/agent-config.ps1"
        "botctl.ps1"
        "botctl.cmd"
        "windows-startup-example.bat"
    )
else
    REQUIRED_PAYLOAD_FILES=(
        "config/.env.example"
        "config/agent.example.toml"
        "config/ops.example.toml"
        "config/runtime.example.toml"
        "botctl.sh"
        "lib/agent-config.sh"
        "botmon.sh"
        "diagnose-network.sh"
        "validate-runtime.sh"
        "qq-maid-healthcheck.sh"
        "qq-maid-systemd.sh"
    )
fi

main "$@"

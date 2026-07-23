#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_TARGET="package-release-test-$$"
BUILD_DIR="${REPO_DIR}/target/${BUILD_TARGET}/release"
DIST_DIR="$(mktemp -d)"
trap 'rm -rf "${DIST_DIR}" "${REPO_DIR}/target/${BUILD_TARGET}"' EXIT

mkdir -p "${BUILD_DIR}"
printf '#!/usr/bin/env bash\nexit 0\n' > "${BUILD_DIR}/qq-maid-bot"
cp "${BUILD_DIR}/qq-maid-bot" "${BUILD_DIR}/qq-maid-bot.exe"
chmod +x "${BUILD_DIR}/qq-maid-bot" "${BUILD_DIR}/qq-maid-bot.exe"

DIST_DIR="${DIST_DIR}" BUILD_TARGET="${BUILD_TARGET}" TARGET_TRIPLE="linux-x86_64" \
    ARCHIVE_FORMAT="tar.gz" bash "${REPO_DIR}/scripts/package-release.sh" test
unix_listing="$(tar -tzf "${DIST_DIR}/qq-maid-bot-test-linux-x86_64.tar.gz")"
printf '%s\n' "${unix_listing}" | grep -Fx 'qq-maid-bot-test-linux-x86_64/botctl.sh' >/dev/null
printf '%s\n' "${unix_listing}" | grep -Fx 'qq-maid-bot-test-linux-x86_64/lib/agent-config.sh' >/dev/null
printf '%s\n' "${unix_listing}" | grep -Fx 'qq-maid-bot-test-linux-x86_64/config/.env.example' >/dev/null
printf '%s\n' "${unix_listing}" | grep -Fx 'qq-maid-bot-test-linux-x86_64/config/agent.example.toml' >/dev/null
printf '%s\n' "${unix_listing}" | grep -Fx 'qq-maid-bot-test-linux-x86_64/config/ops.example.toml' >/dev/null
printf '%s\n' "${unix_listing}" | grep -Fx 'qq-maid-bot-test-linux-x86_64/config/runtime.example.toml' >/dev/null
if printf '%s\n' "${unix_listing}" | grep -E 'config/runtime\.toml$|config/secrets/|master\.key$' >/dev/null; then
    echo "Unix package unexpectedly contains managed runtime state or master key" >&2
    exit 1
fi
if printf '%s\n' "${unix_listing}" | grep -E '\.(bat|cmd|ps1)$' >/dev/null; then
    echo "Unix package unexpectedly contains Windows control scripts" >&2
    exit 1
fi

DIST_DIR="${DIST_DIR}" BUILD_TARGET="${BUILD_TARGET}" TARGET_TRIPLE="windows-x86_64" \
    ARCHIVE_FORMAT="zip" bash "${REPO_DIR}/scripts/package-release.sh" test
windows_listing="$(unzip -Z1 "${DIST_DIR}/qq-maid-bot-test-windows-x86_64.zip")"
printf '%s\n' "${windows_listing}" | grep -Fx 'qq-maid-bot-test-windows-x86_64/botctl.cmd' >/dev/null
printf '%s\n' "${windows_listing}" | grep -Fx 'qq-maid-bot-test-windows-x86_64/qbot.cmd' >/dev/null
printf '%s\n' "${windows_listing}" | grep -Fx 'qq-maid-bot-test-windows-x86_64/lib/agent-config.ps1' >/dev/null
printf '%s\n' "${windows_listing}" | grep -Fx 'qq-maid-bot-test-windows-x86_64/config/.env.example' >/dev/null
printf '%s\n' "${windows_listing}" | grep -Fx 'qq-maid-bot-test-windows-x86_64/config/agent.example.toml' >/dev/null
printf '%s\n' "${windows_listing}" | grep -Fx 'qq-maid-bot-test-windows-x86_64/config/ops.example.toml' >/dev/null
printf '%s\n' "${windows_listing}" | grep -Fx 'qq-maid-bot-test-windows-x86_64/config/runtime.example.toml' >/dev/null
if printf '%s\n' "${windows_listing}" | grep -E 'config/runtime\.toml$|config/secrets/|master\.key$' >/dev/null; then
    echo "Windows package unexpectedly contains managed runtime state or master key" >&2
    exit 1
fi
if printf '%s\n' "${windows_listing}" | grep -E '\.sh$' >/dev/null; then
    echo "Windows package unexpectedly contains shell scripts" >&2
    exit 1
fi

echo "release package platform filtering tests passed"

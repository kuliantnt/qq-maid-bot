#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "${REPO_DIR}/qbot.sh"

assert_target() {
    local system="$1"
    local fixture_arch="$2"
    local expected="$3"
    uname() {
        [[ "${1:-}" == "-s" ]] && echo "${system}" || echo "${fixture_arch}"
    }
    local actual
    actual="$(detect_target)"
    [[ "${actual}" == "${expected}" ]] || {
        echo "target mismatch: ${system}/${machine}: expected ${expected}, got ${actual}" >&2
        return 1
    }
}

assert_target Linux x86_64 linux-x86_64
assert_target Linux aarch64 linux-aarch64
assert_target Darwin x86_64 macos-x86_64
assert_target Darwin arm64 macos-aarch64
assert_target MINGW64_NT-10.0 x86_64 windows-x86_64
assert_target MSYS_NT-10.0 x86_64 windows-x86_64
assert_target CYGWIN_NT-10.0 amd64 windows-x86_64

for system in MINGW64_NT-10.0 MSYS_NT-10.0 CYGWIN_NT-10.0; do
    uname() {
        [[ "${1:-}" == "-s" ]] && echo "${system}" || echo arm64
    }
    set +e
    arm_output="$(detect_target 2>&1)"
    arm_status=$?
    set -e
    [[ "${arm_status}" -ne 0 ]]
    [[ "${arm_output}" == *"当前不提供 Windows ARM64 Release"* ]]
    [[ "${arm_output}" != *"windows-aarch64"* ]]
done

# 用本地 fixture 覆盖下载函数，真实执行 SHA-256 校验和 ZIP 解压。
tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT
fixture="${tmp_dir}/fixture"
output="${tmp_dir}/output"
package="qq-maid-bot-v9.9.9-windows-x86_64"
mkdir -p "${fixture}/${package}" "${output}"
printf 'fixture\n' > "${fixture}/${package}/qq-maid-bot.exe"
printf 'fixture\n' > "${fixture}/${package}/botctl.ps1"
printf 'fixture\n' > "${fixture}/${package}/botctl.cmd"
(
    cd "${fixture}"
    zip -qr "${package}.zip" "${package}"
    sha256sum "${package}.zip" > "${package}.zip.sha256"
)
download_github_file() {
    cp "${fixture}/$3" "$2"
}
release_dir="$(download_release v9.9.9 windows-x86_64 "${output}")"
[[ -f "${release_dir}/qq-maid-bot.exe" ]]
APP_DIR="${tmp_dir}/installed"
copy_release_into_app "${release_dir}" v9.9.9
[[ -f "${APP_DIR}/qq-maid-bot.exe" ]]
[[ -f "${APP_DIR}/botctl.ps1" ]]
[[ -f "${APP_DIR}/botctl.cmd" ]]

# MSYS2 仅为缺失命令调用 pacman，并将 sha256sum/mktemp 去重映射到 coreutils。
deps_bin="${tmp_dir}/deps-bin"
pacman_log="${tmp_dir}/pacman.log"
mkdir -p "${deps_bin}"
printf '%s\n' '#!/bin/bash' '[[ "${1:-}" == "-s" ]] && echo MSYS_NT-10.0 || echo x86_64' > "${deps_bin}/uname"
printf '%s\n' \
    '#!/bin/bash' \
    'printf "%s\n" "$*" > "${PACMAN_LOG}"' \
    'bin_dir="${0%/*}"' \
    'for dependency in curl sha256sum mktemp unzip; do' \
    '  printf "%s\n" "#!/bin/bash" "exit 0" > "${bin_dir}/${dependency}"' \
    '  /bin/chmod +x "${bin_dir}/${dependency}"' \
    'done' > "${deps_bin}/pacman"
chmod +x "${deps_bin}/uname" "${deps_bin}/pacman"
PACMAN_LOG="${pacman_log}" PATH="${deps_bin}" /bin/bash -c "source '${REPO_DIR}/qbot.sh'; install_deps"
pacman_args="$(<"${pacman_log}")"
[[ "${pacman_args}" == "-S --needed --noconfirm curl coreutils unzip" ]]

echo "qbot installer regression tests passed"

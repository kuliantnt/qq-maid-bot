#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "${REPO_DIR}/scripts/qbot.sh"

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
        echo "target mismatch: ${system}/${fixture_arch}: expected ${expected}, got ${actual}" >&2
        return 1
    }
}

assert_target Linux x86_64 linux-x86_64
assert_target Linux aarch64 linux-aarch64
assert_target Darwin x86_64 macos-x86_64
assert_target Darwin arm64 macos-aarch64

# Unix 安装器不得再包含 Windows target、ZIP 或原生 Windows 文件分支。
if rg -n 'MINGW|MSYS|CYGWIN|windows-|\.zip|qq-maid-bot\.exe|botctl\.(ps1|cmd)' \
    "${REPO_DIR}/scripts/qbot.sh" >/dev/null; then
    echo "scripts/qbot.sh unexpectedly contains Windows-specific logic" >&2
    exit 1
fi

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT
fixture="${tmp_dir}/fixture"
output="${tmp_dir}/output"
package="qq-maid-bot-v9.9.9-linux-x86_64"
mkdir -p "${fixture}/${package}/config" "${output}"
printf '#!/usr/bin/env bash\nexit 0\n' > "${fixture}/${package}/qq-maid-bot"
printf '#!/usr/bin/env bash\nexit 0\n' > "${fixture}/${package}/botctl.sh"
printf 'EXAMPLE=1\n' > "${fixture}/${package}/config/.env.example"
printf '[agent]\n' > "${fixture}/${package}/config/agent.toml"
printf 'fixture\n' > "${fixture}/${package}/README.md"
printf 'v9.9.9\n' > "${fixture}/${package}/VERSION"
chmod +x "${fixture}/${package}/qq-maid-bot" "${fixture}/${package}/botctl.sh"
(
    cd "${fixture}"
    tar -czf "${package}.tar.gz" "${package}"
    sha256sum "${package}.tar.gz" > "${package}.tar.gz.sha256"
)

download_github_file() {
    cp "${fixture}/$3" "$2"
}

release_dir="$(download_release v9.9.9 linux-x86_64 "${output}")"
[[ -x "${release_dir}/qq-maid-bot" ]]

APP_DIR="${tmp_dir}/installed"
mkdir -p "${APP_DIR}/config" "${APP_DIR}/data/storage" "${APP_DIR}/logs" "${APP_DIR}/run"
printf 'PRIVATE=keep\n' > "${APP_DIR}/config/.env"
printf 'db\n' > "${APP_DIR}/data/storage/app.db"
printf 'log\n' > "${APP_DIR}/logs/qq-maid-bot.log"
printf '123\n' > "${APP_DIR}/run/qq-maid-bot.pid"

copy_release_into_app "${release_dir}" v9.9.9
[[ -x "${APP_DIR}/qq-maid-bot" ]]
[[ -x "${APP_DIR}/botctl.sh" ]]
[[ -f "${APP_DIR}/config/.env.example" ]]
grep -Fqx 'PRIVATE=keep' "${APP_DIR}/config/.env"
grep -Fqx 'db' "${APP_DIR}/data/storage/app.db"
grep -Fqx 'log' "${APP_DIR}/logs/qq-maid-bot.log"
grep -Fqx '123' "${APP_DIR}/run/qq-maid-bot.pid"

echo "qbot Unix installer regression tests passed"

#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

if [[ -d "${SCRIPT_DIR}/config" ]]; then
    DEFAULT_RUNTIME_DIR="${SCRIPT_DIR}"
else
    DEFAULT_RUNTIME_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/../runtime" && pwd)"
fi

COMMAND="${1:-render}"
if [[ $# -gt 0 ]]; then
    shift
fi

SERVICE_NAME="qq-maid-bot"
SCOPE="system"
RUNTIME_DIR="${QQ_MAID_RUNTIME_DIR:-${DEFAULT_RUNTIME_DIR}}"
BINARY=""
ENV_FILE=""
RUN_AS_USER="${QQ_MAID_SYSTEMD_USER:-}"

usage() {
    cat <<'EOF'
Usage: qq-maid-systemd.sh <command> [options]

Commands:
  render     Print the generated systemd service without writing files
  install    Write the service file and run daemon-reload
  uninstall  Remove the service file and run daemon-reload

Options:
  --runtime-dir PATH    Runtime directory, default: detected runtime/
  --binary PATH         qq-maid-bot executable, default: <runtime-dir>/qq-maid-bot
  --env-file PATH       Env file, default: config/.env then .env under runtime
  --service-name NAME   Service name without ".service", default: qq-maid-bot
  --scope system|user   Install as system or user service, default: system
  --user NAME           Linux user for system service; omit for current systemd user
  -h, --help            Show this help

Examples:
  bash scripts/qq-maid-systemd.sh render --runtime-dir /opt/qqbot/runtime --user qqmaid
  sudo bash scripts/qq-maid-systemd.sh install --runtime-dir /opt/qqbot/runtime --user qqmaid
  bash scripts/qq-maid-systemd.sh install --scope user --runtime-dir "$HOME/qq-maid-bot/runtime"
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

abs_path() {
    local path="$1"
    if [[ "${path}" == /* ]]; then
        printf '%s\n' "${path}"
    else
        printf '%s/%s\n' "$(pwd)" "${path}"
    fi
}

quote_env_value() {
    local value="$1"
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    printf '"%s"' "${value}"
}

validate_service_name() {
    [[ "${SERVICE_NAME}" =~ ^[A-Za-z0-9_.@-]+$ ]] || die "invalid service name: ${SERVICE_NAME}"
    SERVICE_NAME="${SERVICE_NAME%.service}"
}

resolve_defaults() {
    RUNTIME_DIR="$(abs_path "${RUNTIME_DIR}")"
    BINARY="${BINARY:-${RUNTIME_DIR}/qq-maid-bot}"

    if [[ -z "${ENV_FILE}" ]]; then
        if [[ -f "${RUNTIME_DIR}/config/.env" ]]; then
            ENV_FILE="${RUNTIME_DIR}/config/.env"
        else
            ENV_FILE="${RUNTIME_DIR}/.env"
        fi
    fi

    BINARY="$(abs_path "${BINARY}")"
    ENV_FILE="$(abs_path "${ENV_FILE}")"
}

validate_inputs() {
    [[ "${SCOPE}" == "system" || "${SCOPE}" == "user" ]] || die "--scope must be system or user"
    [[ -d "${RUNTIME_DIR}" ]] || die "runtime dir not found: ${RUNTIME_DIR}"
    [[ -f "${RUNTIME_DIR}/botctl.sh" ]] || die "botctl.sh not found in runtime dir: ${RUNTIME_DIR}"
    [[ -f "${BINARY}" ]] || die "binary not found: ${BINARY}"
    [[ -f "${ENV_FILE}" ]] || die "env file not found: ${ENV_FILE}"
    validate_service_name
}

service_path() {
    if [[ "${SCOPE}" == "user" ]]; then
        local config_home="${XDG_CONFIG_HOME:-${HOME}/.config}"
        printf '%s/systemd/user/%s.service\n' "${config_home}" "${SERVICE_NAME}"
    else
        printf '/etc/systemd/system/%s.service\n' "${SERVICE_NAME}"
    fi
}

systemctl_cmd() {
    if [[ "${SCOPE}" == "user" ]]; then
        printf 'systemctl --user'
    else
        printf 'systemctl'
    fi
}

render_service() {
    local install_target user_line
    if [[ "${SCOPE}" == "user" ]]; then
        install_target="default.target"
        user_line=""
    else
        install_target="multi-user.target"
        if [[ -n "${RUN_AS_USER}" ]]; then
            user_line="User=${RUN_AS_USER}"
        else
            user_line=""
        fi
    fi

    cat <<EOF
[Unit]
Description=QQ Maid Bot
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
EOF
    if [[ -n "${user_line}" ]]; then
        printf '%s\n' "${user_line}"
    fi
    cat <<EOF
WorkingDirectory=${RUNTIME_DIR}
Environment=$(quote_env_value "QQ_MAID_RUNTIME_DIR=${RUNTIME_DIR}")
Environment=$(quote_env_value "BOT_BINARY=${BINARY}")
Environment=$(quote_env_value "BOT_ENV_FILE=${ENV_FILE}")
ExecStart=${RUNTIME_DIR}/botctl.sh run
Restart=on-failure
RestartSec=5s
KillSignal=SIGTERM
TimeoutStopSec=30s
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full

[Install]
WantedBy=${install_target}
EOF
}

install_service() {
    command -v systemctl >/dev/null 2>&1 || die "systemctl not found; use the non-systemd fallback in runtime/README.md"

    local target
    target="$(service_path)"

    if [[ "${SCOPE}" == "system" && "${EUID}" -ne 0 ]]; then
        die "system service install requires root; rerun with sudo or use --scope user"
    fi

    echo "target: ${target}"
    echo "service content:"
    render_service

    mkdir -p "$(dirname -- "${target}")"
    render_service > "${target}"
    chmod 0644 "${target}"
    $(systemctl_cmd) daemon-reload

    echo "installed ${target}"
    echo "next: $(systemctl_cmd) enable --now ${SERVICE_NAME}.service"
}

uninstall_service() {
    command -v systemctl >/dev/null 2>&1 || die "systemctl not found"

    local target
    target="$(service_path)"

    if [[ "${SCOPE}" == "system" && "${EUID}" -ne 0 ]]; then
        die "system service uninstall requires root; rerun with sudo or use --scope user"
    fi

    if [[ -f "${target}" ]]; then
        $(systemctl_cmd) disable --now "${SERVICE_NAME}.service" 2>/dev/null || true
        rm -f "${target}"
        $(systemctl_cmd) daemon-reload
        echo "removed ${target}"
    else
        echo "service file not found: ${target}"
    fi
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --runtime-dir)
            RUNTIME_DIR="${2:-}"
            shift 2
            ;;
        --binary)
            BINARY="${2:-}"
            shift 2
            ;;
        --env-file)
            ENV_FILE="${2:-}"
            shift 2
            ;;
        --service-name)
            SERVICE_NAME="${2:-}"
            shift 2
            ;;
        --scope)
            SCOPE="${2:-}"
            shift 2
            ;;
        --user)
            RUN_AS_USER="${2:-}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            die "unknown option: $1"
            ;;
    esac
done

case "${COMMAND}" in
    -h|--help|help)
        usage
        exit 0
        ;;
esac

resolve_defaults
validate_inputs

case "${COMMAND}" in
    render)
        echo "target: $(service_path)"
        render_service
        ;;
    install)
        install_service
        ;;
    uninstall)
        uninstall_service
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

if [[ -d "${SCRIPT_DIR}/config" ]]; then
    DEFAULT_RUNTIME_DIR="${SCRIPT_DIR}"
elif [[ -d "${SCRIPT_DIR}/../runtime" ]]; then
    DEFAULT_RUNTIME_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/../runtime" && pwd)"
else
    DEFAULT_RUNTIME_DIR="${SCRIPT_DIR}/../runtime"
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

require_value() {
    local option="$1"
    local value="${2-}"
    if [[ $# -lt 2 || -z "${value}" || "${value}" == --* ]]; then
        die "${option} requires a value"
    fi
    printf '%s\n' "${value}"
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
    SERVICE_NAME="${SERVICE_NAME%.service}"
    [[ -n "${SERVICE_NAME}" ]] || die "--service-name must not be empty"
    [[ "${SERVICE_NAME}" =~ ^[A-Za-z0-9_.@-]+$ ]] || die "invalid service name: ${SERVICE_NAME}; allowed: letters, digits, dot, underscore, @ and dash"
}

validate_systemd_path() {
    local name="$1"
    local value="$2"
    [[ -n "${value}" ]] || die "${name} must not be empty"
    if [[ "${value}" =~ [[:cntrl:]] ]]; then
        die "${name} contains control characters, which are not safe in systemd unit fields"
    fi
    if [[ "${value}" =~ [[:space:]] ]]; then
        die "${name} contains whitespace; use a path without spaces for systemd unit generation"
    fi
    if [[ "${value}" == *%* ]]; then
        die "${name} contains %, which systemd treats as a specifier; use a path without %"
    fi
}

validate_system_user() {
    if [[ "${SCOPE}" == "user" ]]; then
        if [[ -n "${RUN_AS_USER}" ]]; then
            die "--user and QQ_MAID_SYSTEMD_USER are only valid with --scope system"
        fi
        return 0
    fi

    [[ -z "${RUN_AS_USER}" ]] && return 0
    if [[ "${RUN_AS_USER}" =~ [[:cntrl:]] || "${RUN_AS_USER}" =~ [[:space:]] ]]; then
        die "--user contains whitespace or control characters"
    fi
    if [[ "${RUN_AS_USER}" == */* ]]; then
        die "--user must be a Linux user name, not a path"
    fi
    if [[ "${RUN_AS_USER}" == *:* ]]; then
        die "--user must not contain ':'"
    fi
    if [[ ! "${RUN_AS_USER}" =~ ^[A-Za-z_][A-Za-z0-9_.@-]*[$]?$ && ! "${RUN_AS_USER}" =~ ^[0-9]+$ ]]; then
        die "--user has invalid format; use a Linux user name or numeric UID"
    fi
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

validate_common_inputs() {
    [[ "${SCOPE}" == "system" || "${SCOPE}" == "user" ]] || die "--scope must be system or user"
    validate_system_user
    validate_service_name
}

validate_render_install_inputs() {
    validate_common_inputs
    validate_systemd_path "runtime dir" "${RUNTIME_DIR}"
    validate_systemd_path "binary path" "${BINARY}"
    validate_systemd_path "env file path" "${ENV_FILE}"
    [[ -d "${RUNTIME_DIR}" ]] || die "runtime dir not found: ${RUNTIME_DIR}"
    [[ -f "${RUNTIME_DIR}/botctl.sh" ]] || die "botctl.sh not found in runtime dir: ${RUNTIME_DIR}"
    [[ -f "${BINARY}" ]] || die "binary not found: ${BINARY}"
    [[ -f "${ENV_FILE}" ]] || die "env file not found: ${ENV_FILE}"
}

validate_uninstall_inputs() {
    validate_common_inputs
    if [[ "${SCOPE}" == "user" ]]; then
        user_service_config_home >/dev/null
    fi
}

user_service_config_home() {
    if [[ -n "${XDG_CONFIG_HOME:-}" ]]; then
        printf '%s\n' "${XDG_CONFIG_HOME}"
        return 0
    fi
    if [[ -n "${HOME:-}" ]]; then
        printf '%s/.config\n' "${HOME}"
        return 0
    fi
    die "HOME is not set; set HOME or XDG_CONFIG_HOME to locate user systemd service directory"
}

service_path() {
    if [[ "${SCOPE}" == "user" ]]; then
        local config_home
        config_home="$(user_service_config_home)"
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
    echo "before enabling: run './botctl.sh stop' in ${RUNTIME_DIR} if a background botctl process is already running"
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
            RUNTIME_DIR="$(require_value "$1" "${2-}")"
            shift 2
            ;;
        --binary)
            BINARY="$(require_value "$1" "${2-}")"
            shift 2
            ;;
        --env-file)
            ENV_FILE="$(require_value "$1" "${2-}")"
            shift 2
            ;;
        --service-name)
            SERVICE_NAME="$(require_value "$1" "${2-}")"
            shift 2
            ;;
        --scope)
            SCOPE="$(require_value "$1" "${2-}")"
            shift 2
            ;;
        --user)
            RUN_AS_USER="$(require_value "$1" "${2-}")"
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

case "${COMMAND}" in
    render)
        resolve_defaults
        validate_render_install_inputs
        echo "target: $(service_path)"
        render_service
        ;;
    install)
        resolve_defaults
        validate_render_install_inputs
        install_service
        ;;
    uninstall)
        validate_uninstall_inputs
        uninstall_service
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

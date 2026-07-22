#!/usr/bin/env bash
set -euo pipefail

DEPLOY_USER="qqmaid"
DEPLOY_UID="10001"
DEPLOY_GID="10001"
APP_DIR="/opt/qq-maid-bot-test"
PROJECT_NAME="qq-maid-bot-test"
AUTHORIZED_KEY_FILE=""

usage() {
    cat <<'EOF'
Usage: sudo bash docker-host-init.sh [options]

Options:
  --user <name>                 部署用户，默认 qqmaid
  --uid <id>                    部署用户 UID，默认 10001
  --gid <id>                    部署用户 GID，默认 10001
  --app-dir <absolute-path>     测试实例目录，默认 /opt/qq-maid-bot-test
  --project-name <name>         Compose project，默认 qq-maid-bot-test
  --authorized-key-file <path>  要安装的专用 SSH 公钥文件

本脚本检查 Docker Engine/Compose，创建非 root 部署用户、实例目录和最小配置。
注意：该用户会加入 docker 组，事实上可获得接近宿主机 root 的权限，并非完整隔离边界。
Docker 的安装步骤见 docs/deployment/docker.md；脚本不会静默修改系统软件源。
EOF
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --user) shift; DEPLOY_USER="${1:-}" ;;
        --uid) shift; DEPLOY_UID="${1:-}" ;;
        --gid) shift; DEPLOY_GID="${1:-}" ;;
        --app-dir) shift; APP_DIR="${1:-}" ;;
        --project-name) shift; PROJECT_NAME="${1:-}" ;;
        --authorized-key-file) shift; AUTHORIZED_KEY_FILE="${1:-}" ;;
        -h|--help) usage; exit 0 ;;
        *) fail "未知参数: $1" ;;
    esac
    shift
done

[[ "$(id -u)" -eq 0 ]] || fail "需要 root 执行用户和目录初始化"
[[ "${DEPLOY_UID}" =~ ^[0-9]+$ && "${DEPLOY_GID}" =~ ^[0-9]+$ ]] \
    || fail "UID/GID 必须是正整数"
[[ "${APP_DIR}" =~ ^/opt/[A-Za-z0-9._/-]+$ ]] \
    || fail "--app-dir 必须是 /opt 下的具体绝对目录"
[[ ! "${APP_DIR}" =~ (^|/)\.{1,2}(/|$) ]] \
    || fail "--app-dir 不能包含 . 或 .. 路径段"
[[ "${PROJECT_NAME}" =~ ^[a-z0-9][a-z0-9_-]*$ ]] \
    || fail "project name 只能包含小写字母、数字、下划线和连字符"

command -v docker >/dev/null || fail "未安装 Docker Engine，请先按 Docker 官方文档安装"
docker compose version >/dev/null || fail "未安装 Docker Compose plugin"
getent group docker >/dev/null || fail "系统缺少 docker 组，请检查 Docker Engine 安装"

if getent group "${DEPLOY_USER}" >/dev/null; then
    existing_gid="$(getent group "${DEPLOY_USER}" | cut -d: -f3)"
    [[ "${existing_gid}" == "${DEPLOY_GID}" ]] \
        || fail "现有组 ${DEPLOY_USER} 的 GID 不等于 ${DEPLOY_GID}"
elif getent group "${DEPLOY_GID}" >/dev/null; then
    existing_group="$(getent group "${DEPLOY_GID}" | cut -d: -f1)"
    fail "GID ${DEPLOY_GID} 已被组 ${existing_group} 使用"
else
    groupadd --gid "${DEPLOY_GID}" "${DEPLOY_USER}"
fi

if id "${DEPLOY_USER}" >/dev/null 2>&1; then
    [[ "$(id -u "${DEPLOY_USER}")" == "${DEPLOY_UID}" ]] \
        || fail "现有用户 ${DEPLOY_USER} 的 UID 不等于 ${DEPLOY_UID}"
    [[ "$(id -g "${DEPLOY_USER}")" == "${DEPLOY_GID}" ]] \
        || fail "现有用户 ${DEPLOY_USER} 的主 GID 不等于 ${DEPLOY_GID}"
else
    useradd --create-home --uid "${DEPLOY_UID}" --gid "${DEPLOY_GID}" \
        --shell /bin/bash "${DEPLOY_USER}"
fi
usermod --append --groups docker "${DEPLOY_USER}"

install -d -m 0755 -o "${DEPLOY_UID}" -g "${DEPLOY_GID}" "${APP_DIR}"
install -d -m 0750 -o "${DEPLOY_UID}" -g "${DEPLOY_GID}" \
    "${APP_DIR}/runtime/config" \
    "${APP_DIR}/runtime/data/storage" \
    "${APP_DIR}/runtime/media/inbound" \
    "${APP_DIR}/deployments"
install -d -m 0700 -o "${DEPLOY_UID}" -g "${DEPLOY_GID}" \
    "${APP_DIR}/runtime/config/secrets"

if [[ ! -e "${APP_DIR}/runtime/config/.env" ]]; then
    install -m 0600 -o "${DEPLOY_UID}" -g "${DEPLOY_GID}" /dev/null \
        "${APP_DIR}/runtime/config/.env"
fi

if [[ ! -e "${APP_DIR}/compose.env" ]]; then
    compose_env_tmp="$(mktemp "${APP_DIR}/.compose.env.XXXXXX")"
    {
        printf 'COMPOSE_PROJECT_NAME=%s\n' "${PROJECT_NAME}"
        printf 'QQ_MAID_UID=%s\n' "${DEPLOY_UID}"
        printf 'QQ_MAID_GID=%s\n' "${DEPLOY_GID}"
        printf 'QQ_MAID_ENV_FILE=%s/runtime/config/.env\n' "${APP_DIR}"
        printf 'QQ_MAID_CONFIG_DIR=%s/runtime/config\n' "${APP_DIR}"
        printf 'QQ_MAID_DATA_DIR=%s/runtime/data\n' "${APP_DIR}"
        printf 'QQ_MAID_MEDIA_DIR=%s/runtime/media\n' "${APP_DIR}"
    } > "${compose_env_tmp}"
    chown "${DEPLOY_UID}:${DEPLOY_GID}" "${compose_env_tmp}"
    chmod 0600 "${compose_env_tmp}"
    mv "${compose_env_tmp}" "${APP_DIR}/compose.env"
fi

if [[ -n "${AUTHORIZED_KEY_FILE}" ]]; then
    [[ -f "${AUTHORIZED_KEY_FILE}" && ! -L "${AUTHORIZED_KEY_FILE}" ]] \
        || fail "SSH 公钥文件不存在或不是普通文件"
    deploy_home="$(getent passwd "${DEPLOY_USER}" | cut -d: -f6)"
    install -d -m 0700 -o "${DEPLOY_UID}" -g "${DEPLOY_GID}" "${deploy_home}/.ssh"
    authorized_keys="${deploy_home}/.ssh/authorized_keys"
    touch "${authorized_keys}"
    chown "${DEPLOY_UID}:${DEPLOY_GID}" "${authorized_keys}"
    chmod 0600 "${authorized_keys}"
    public_key="$(tr -d '\r\n' < "${AUTHORIZED_KEY_FILE}")"
    [[ "${public_key}" == ssh-* ]] || fail "SSH 公钥格式不合法"
    grep -Fqx "${public_key}" "${authorized_keys}" \
        || printf '%s\n' "${public_key}" >> "${authorized_keys}"
fi

printf '初始化完成：user=%s uid=%s gid=%s app_dir=%s\n' \
    "${DEPLOY_USER}" "${DEPLOY_UID}" "${DEPLOY_GID}" "${APP_DIR}"
printf '下一步：填写 %s/runtime/config/.env，并配置 GitHub Environment test。\n' "${APP_DIR}"

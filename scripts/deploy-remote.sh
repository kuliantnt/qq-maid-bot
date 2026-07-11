#!/usr/bin/env bash
set -euo pipefail

# ============================================================
# deploy-remote.sh - 构建并部署 qq-maid 项目到远程服务器
#
# 远程服务器信息从 scripts/deploy.conf 读取 (与 sync_knowledge.sh 共用)。
# 首次使用请从 deploy.conf.example 复制并填入实际值。
# 部署组件: qq-maid-bot、控制脚本、健康检查、systemd/Windows 启动模板与诊断工具
# ============================================================

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DEPLOY_CONF="${SCRIPT_DIR}/deploy.conf"

if [[ ! -f "$DEPLOY_CONF" ]]; then
  echo "[错误] 远程服务器配置不存在: $DEPLOY_CONF"
  echo "  请从 deploy.conf.example 复制并填入实际值。"
  exit 1
fi
source "$DEPLOY_CONF"

if [[ -z "${REMOTE_HOST:-}" ]]; then
  echo "[错误] deploy.conf 缺少 REMOTE_HOST"
  exit 1
fi
if [[ -z "${REMOTE_PROJECT_DIR:-}" ]]; then
  echo "[错误] deploy.conf 缺少 REMOTE_PROJECT_DIR"
  exit 1
fi

REMOTE_RUNTIME_DIR="${REMOTE_PROJECT_DIR}/runtime"
LOCAL_VALIDATE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/qqbot-deploy-validate.XXXXXX")"

cleanup_local_validate_dir() {
    rm -rf "${LOCAL_VALIDATE_DIR}"
}

prepare_validate_runtime() {
    install -d "${LOCAL_VALIDATE_DIR}/config"
    install -m 0755 target/release/qq-maid-bot "${LOCAL_VALIDATE_DIR}/qq-maid-bot"
    install -m 0755 scripts/botctl.sh "${LOCAL_VALIDATE_DIR}/botctl.sh"
    install -m 0755 scripts/diagnose-network.sh "${LOCAL_VALIDATE_DIR}/diagnose-network.sh"
    install -m 0755 scripts/validate-runtime.sh "${LOCAL_VALIDATE_DIR}/validate-runtime.sh"
    install -m 0755 scripts/qq-maid-healthcheck.sh "${LOCAL_VALIDATE_DIR}/qq-maid-healthcheck.sh"
    install -m 0755 scripts/botmon.sh "${LOCAL_VALIDATE_DIR}/botmon.sh"
    install -m 0755 scripts/qq-maid-systemd.sh "${LOCAL_VALIDATE_DIR}/qq-maid-systemd.sh"
    install -m 0644 scripts/windows-startup-example.bat "${LOCAL_VALIDATE_DIR}/windows-startup-example.bat"
    install -m 0644 runtime/config/.env.example "${LOCAL_VALIDATE_DIR}/.env.example"
    install -m 0644 runtime/config/agent.toml "${LOCAL_VALIDATE_DIR}/config/agent.toml"
    install -m 0644 runtime/README.md "${LOCAL_VALIDATE_DIR}/README.md"
}

trap cleanup_local_validate_dir EXIT

echo "==> Building release..."
SECONDS=0
make build
BUILD_ELAPSED="${SECONDS}"

echo "==> Validating release payload..."
# 这里校验的是待上传的离线 runtime 目录结构；在线服务状态检查应使用
# scripts/validate-runtime.sh 的 check/glm/console 等子命令，不能混用。
prepare_validate_runtime
bash scripts/validate-release-runtime.sh "${LOCAL_VALIDATE_DIR}"

echo "==> Uploading artifacts..."
# runtime 是远端运行目录，专门放二进制、控制脚本、配置模板和运行期文件。
ssh "${REMOTE_HOST}" "mkdir -p '${REMOTE_RUNTIME_DIR}'"

# 将编译产物、脚本和配置模板上传为 .new 临时文件，避免覆盖正在运行的服务。
scp target/release/qq-maid-bot "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/.qq-maid-bot.new"
scp scripts/botctl.sh "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/.botctl.sh.new"
scp scripts/diagnose-network.sh "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/.diagnose-network.sh.new"
scp scripts/validate-runtime.sh "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/.validate-runtime.sh.new"
scp scripts/qq-maid-healthcheck.sh "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/.qq-maid-healthcheck.sh.new"
scp scripts/botmon.sh "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/.botmon.sh.new"
scp scripts/qq-maid-systemd.sh "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/.qq-maid-systemd.sh.new"
scp scripts/windows-startup-example.bat "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/windows-startup-example.bat.new"
scp runtime/config/.env.example "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/.env.example"
ssh "${REMOTE_HOST}" "mkdir -p '${REMOTE_RUNTIME_DIR}/config'"
scp runtime/config/agent.toml "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/config/agent.toml.new"
scp runtime/README.md "${REMOTE_HOST}:${REMOTE_RUNTIME_DIR}/README.md"

echo "==> Installing artifacts..."
# 设置可执行权限后，将临时文件原子地替换为目标文件；清理旧 qq-maid-* 时需保留
# 当前二进制、健康检查脚本和 systemd 管理脚本，避免远端巡检/自启动入口在部署后被误删。
ssh "${REMOTE_HOST}" "cd '${REMOTE_RUNTIME_DIR}' && chmod 0755 .qq-maid-bot.new .botctl.sh.new .diagnose-network.sh.new .validate-runtime.sh.new .qq-maid-healthcheck.sh.new .botmon.sh.new .qq-maid-systemd.sh.new && mv -f .qq-maid-bot.new qq-maid-bot && mv -f .botctl.sh.new botctl.sh && mv -f .diagnose-network.sh.new diagnose-network.sh && mv -f .validate-runtime.sh.new validate-runtime.sh && mv -f .qq-maid-healthcheck.sh.new qq-maid-healthcheck.sh && mv -f .botmon.sh.new botmon.sh && mv -f .qq-maid-systemd.sh.new qq-maid-systemd.sh && mv -f windows-startup-example.bat.new windows-startup-example.bat && find . -maxdepth 1 -type f -name 'qq-maid-*' ! -name 'qq-maid-bot' ! -name 'qq-maid-healthcheck.sh' ! -name 'qq-maid-systemd.sh' -delete && find . -maxdepth 1 -type f -name '*ctl.sh' ! -name 'botctl.sh' -delete && rm -rf static .static.new static.old"
# agent.toml 是运行期活动策略文件。远端已存在时只留下新版本供人工比对，
# 避免部署覆盖本机模型路线、profile 或 Tool Calling 开关。
ssh "${REMOTE_HOST}" "cd '${REMOTE_RUNTIME_DIR}' && { test -f config/agent.toml || mv config/agent.toml.new config/agent.toml; }"

echo "==> Restarting remote services..."
# 重启统一服务。旧双进程文件在安装阶段清理，避免同机残留旧入口。
SECONDS=0
ssh "${REMOTE_HOST}" "cd '${REMOTE_PROJECT_DIR}' && ./runtime/botctl.sh restart"
RESTART_ELAPSED="${SECONDS}"

echo "==> Checking processes..."
# 检查服务是否已重新拉起
ssh "${REMOTE_HOST}" "ps aux | grep -E 'qq-maid-bot' | grep -v grep || true"

echo "==> Done."
printf '  构建 %ds | 重启 %ds | 总计 %ds\n' \
    "${BUILD_ELAPSED}" "${RESTART_ELAPSED}" "$((BUILD_ELAPSED + RESTART_ELAPSED))"

#!/usr/bin/env bash
set -euo pipefail

# ============================================================
# deploy-remote.sh - 构建并部署 qq-maid 项目到远程服务器
#
# 远程服务器信息从 scripts/deploy.conf 读取 (与 sync_knowledge.sh 共用)。
# 首次使用请从 deploy.conf.example 复制并填入实际值。
# 部署组件: qq-maid-bot、Unix 控制脚本、健康检查、systemd 管理与诊断工具
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
if [[ -z "${REMOTE_APP_DIR:-}" && -z "${REMOTE_PROJECT_DIR:-}" ]]; then
  echo "[错误] deploy.conf 至少需要 REMOTE_APP_DIR 或 REMOTE_PROJECT_DIR"
  exit 1
fi

# 新部署推荐显式使用独立应用根目录；未配置时保留历史源码目录下 runtime/ 的行为。
REMOTE_APP_DIR="${REMOTE_APP_DIR:-${REMOTE_PROJECT_DIR}/runtime}"
LOCAL_VALIDATE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/qqbot-deploy-validate.XXXXXX")"

cleanup_local_validate_dir() {
    rm -rf "${LOCAL_VALIDATE_DIR}"
}

prepare_validate_runtime() {
    install -d "${LOCAL_VALIDATE_DIR}/config" "${LOCAL_VALIDATE_DIR}/lib"
    install -m 0755 target/release/qq-maid-bot "${LOCAL_VALIDATE_DIR}/qq-maid-bot"
    install -m 0755 scripts/botctl.sh "${LOCAL_VALIDATE_DIR}/botctl.sh"
    install -m 0755 scripts/diagnose-network.sh "${LOCAL_VALIDATE_DIR}/diagnose-network.sh"
    install -m 0755 scripts/validate-runtime.sh "${LOCAL_VALIDATE_DIR}/validate-runtime.sh"
    install -m 0755 scripts/qq-maid-healthcheck.sh "${LOCAL_VALIDATE_DIR}/qq-maid-healthcheck.sh"
    install -m 0755 scripts/botmon.sh "${LOCAL_VALIDATE_DIR}/botmon.sh"
    install -m 0755 scripts/qq-maid-systemd.sh "${LOCAL_VALIDATE_DIR}/qq-maid-systemd.sh"
    install -m 0644 scripts/lib/agent-config.sh "${LOCAL_VALIDATE_DIR}/lib/agent-config.sh"
    install -m 0644 runtime/config/.env.example "${LOCAL_VALIDATE_DIR}/config/.env.example"
    install -m 0644 runtime/config/agent.toml "${LOCAL_VALIDATE_DIR}/config/agent.toml"
    install -m 0644 runtime/config/ops.example.toml "${LOCAL_VALIDATE_DIR}/config/ops.example.toml"
    install -m 0644 runtime/config/runtime.example.toml "${LOCAL_VALIDATE_DIR}/config/runtime.example.toml"
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
bash scripts/validate-release-runtime.sh "${LOCAL_VALIDATE_DIR}" unix

echo "==> Uploading artifacts..."
# 应用根目录独立承载二进制、控制脚本、公开配置模板和运行期子目录。
# secrets/ 是主密钥的持久化目录。部署只负责创建并收紧目录权限；绝不上传、覆盖或
# 重新生成 master.key，避免升级后已有密文永久不可解。
ssh "${REMOTE_HOST}" "mkdir -p '${REMOTE_APP_DIR}/config' '${REMOTE_APP_DIR}/config/secrets' '${REMOTE_APP_DIR}/data/storage' '${REMOTE_APP_DIR}/lib' '${REMOTE_APP_DIR}/logs' '${REMOTE_APP_DIR}/run' && chmod 0700 '${REMOTE_APP_DIR}/config/secrets'"

# 将编译产物、脚本和配置模板上传为 .new 临时文件，避免覆盖正在运行的服务。
scp target/release/qq-maid-bot "${REMOTE_HOST}:${REMOTE_APP_DIR}/.qq-maid-bot.new"
scp scripts/botctl.sh "${REMOTE_HOST}:${REMOTE_APP_DIR}/.botctl.sh.new"
scp scripts/diagnose-network.sh "${REMOTE_HOST}:${REMOTE_APP_DIR}/.diagnose-network.sh.new"
scp scripts/validate-runtime.sh "${REMOTE_HOST}:${REMOTE_APP_DIR}/.validate-runtime.sh.new"
scp scripts/qq-maid-healthcheck.sh "${REMOTE_HOST}:${REMOTE_APP_DIR}/.qq-maid-healthcheck.sh.new"
scp scripts/botmon.sh "${REMOTE_HOST}:${REMOTE_APP_DIR}/.botmon.sh.new"
scp scripts/qq-maid-systemd.sh "${REMOTE_HOST}:${REMOTE_APP_DIR}/.qq-maid-systemd.sh.new"
scp scripts/lib/agent-config.sh "${REMOTE_HOST}:${REMOTE_APP_DIR}/lib/.agent-config.sh.new"
scp runtime/config/.env.example "${REMOTE_HOST}:${REMOTE_APP_DIR}/config/.env.example.new"
scp runtime/config/agent.toml "${REMOTE_HOST}:${REMOTE_APP_DIR}/config/agent.toml.new"
scp runtime/config/ops.example.toml "${REMOTE_HOST}:${REMOTE_APP_DIR}/config/ops.example.toml.new"
scp runtime/config/runtime.example.toml "${REMOTE_HOST}:${REMOTE_APP_DIR}/config/runtime.example.toml.new"
scp runtime/README.md "${REMOTE_HOST}:${REMOTE_APP_DIR}/README.md"

echo "==> Installing artifacts..."
# 设置可执行权限后，将临时文件原子地替换为目标文件；清理旧 qq-maid-* 时需保留
# 当前二进制、健康检查脚本和 systemd 管理脚本，避免远端巡检/自启动入口在部署后被误删。
ssh "${REMOTE_HOST}" "cd '${REMOTE_APP_DIR}' && chmod 0755 .qq-maid-bot.new .botctl.sh.new .diagnose-network.sh.new .validate-runtime.sh.new .qq-maid-healthcheck.sh.new .botmon.sh.new .qq-maid-systemd.sh.new && chmod 0644 lib/.agent-config.sh.new && mv -f .qq-maid-bot.new qq-maid-bot && mv -f .botctl.sh.new botctl.sh && mv -f .diagnose-network.sh.new diagnose-network.sh && mv -f .validate-runtime.sh.new validate-runtime.sh && mv -f .qq-maid-healthcheck.sh.new qq-maid-healthcheck.sh && mv -f .botmon.sh.new botmon.sh && mv -f .qq-maid-systemd.sh.new qq-maid-systemd.sh && mv -f lib/.agent-config.sh.new lib/agent-config.sh && mv -f config/.env.example.new config/.env.example && mv -f config/ops.example.toml.new config/ops.example.toml && mv -f config/runtime.example.toml.new config/runtime.example.toml && find . -maxdepth 1 -type f -name 'qq-maid-*' ! -name 'qq-maid-bot' ! -name 'qq-maid-healthcheck.sh' ! -name 'qq-maid-systemd.sh' -delete && find . -maxdepth 1 -type f -name '*ctl.sh' ! -name 'botctl.sh' -delete && rm -f botctl.ps1 botctl.cmd windows-startup-example.bat .env.example && rm -rf static .static.new static.old"
# Agent 策略模板随 Release 一起升级：先保留远端旧文件，再原子启用新版。
# 这是本次版本升级的唯一自动替换点；后续新增普通可选字段由程序默认值兼容。
ssh "${REMOTE_HOST}" "cd '${REMOTE_APP_DIR}' && bash -s" <<'REMOTE_AGENT_CONFIG'
set -euo pipefail
marker=config/.agent-config-v0.20.2
if test ! -e "$marker"; then
    if test -L config/agent.toml; then
        echo "refuse to replace symbolic-link config/agent.toml" >&2
        exit 1
    fi
    if test -f config/agent.toml; then
        backup=config/agent.toml.old
        suffix=0
        while test -e "$backup" || test -L "$backup"; do
            suffix=$((suffix + 1))
            backup="config/agent.toml.old.${suffix}"
        done
        mv config/agent.toml "$backup"
        if ! mv config/agent.toml.new config/agent.toml; then
            mv "$backup" config/agent.toml || true
            exit 1
        fi
    else
        mv config/agent.toml.new config/agent.toml
    fi
    : > "$marker"
else
    rm -f config/agent.toml.new
fi
REMOTE_AGENT_CONFIG
# runtime.toml 是 WebUI 与人工编辑共享的活动配置，部署只更新公开示例，不能覆盖它。

echo "==> Restarting remote services..."
# 重启统一服务。旧双进程文件在安装阶段清理，避免同机残留旧入口。
SECONDS=0
ssh "${REMOTE_HOST}" "cd '${REMOTE_APP_DIR}' && ./botctl.sh restart"
RESTART_ELAPSED="${SECONDS}"

echo "==> Checking processes..."
# 检查服务是否已重新拉起
ssh "${REMOTE_HOST}" "ps aux | grep -E 'qq-maid-bot' | grep -v grep || true"

echo "==> Done."
printf '  构建 %ds | 重启 %ds | 总计 %ds\n' \
    "${BUILD_ELAPSED}" "${RESTART_ELAPSED}" "$((BUILD_ELAPSED + RESTART_ELAPSED))"

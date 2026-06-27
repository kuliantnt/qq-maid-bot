#!/usr/bin/env bash
set -euo pipefail

# ============================================================
# deploy-remote.sh - 构建并部署 qq-maid 项目到远程服务器
#
# 远程主机: aliyun
# 远程路径: /root/project/qqbot
# 部署组件: qq-maid-bot、控制脚本与诊断工具
# ============================================================

REMOTE="aliyun"
REMOTE_DIR="/root/project/qqbot"
REMOTE_RUNTIME_DIR="${REMOTE_DIR}/runtime"
LOCAL_VALIDATE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/qqbot-deploy-validate.XXXXXX")"

cleanup_local_validate_dir() {
    rm -rf "${LOCAL_VALIDATE_DIR}"
}

prepare_validate_runtime() {
    install -d "${LOCAL_VALIDATE_DIR}/static"
    install -m 0755 target/release/qq-maid-bot "${LOCAL_VALIDATE_DIR}/qq-maid-bot"
    install -m 0755 scripts/botctl.sh "${LOCAL_VALIDATE_DIR}/botctl.sh"
    install -m 0755 scripts/diagnose-network.sh "${LOCAL_VALIDATE_DIR}/diagnose-network.sh"
    install -m 0755 scripts/validate-runtime.sh "${LOCAL_VALIDATE_DIR}/validate-runtime.sh"
    install -m 0644 runtime/config/.env.example "${LOCAL_VALIDATE_DIR}/.env.example"
    install -m 0644 runtime/README.md "${LOCAL_VALIDATE_DIR}/README.md"
    cp -R runtime/static/. "${LOCAL_VALIDATE_DIR}/static/"
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
ssh "${REMOTE}" "mkdir -p '${REMOTE_RUNTIME_DIR}'"

# 将编译产物、脚本和配置模板上传为 .new 临时文件，避免覆盖正在运行的服务
scp target/release/qq-maid-bot "${REMOTE}:${REMOTE_RUNTIME_DIR}/.qq-maid-bot.new"
scp scripts/botctl.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.botctl.sh.new"
scp scripts/diagnose-network.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.diagnose-network.sh.new"
scp scripts/validate-runtime.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.validate-runtime.sh.new"
scp runtime/config/.env.example "${REMOTE}:${REMOTE_RUNTIME_DIR}/.env.example"
scp runtime/README.md "${REMOTE}:${REMOTE_RUNTIME_DIR}/README.md"
scp -r runtime/static "${REMOTE}:${REMOTE_RUNTIME_DIR}/.static.new"

echo "==> Installing artifacts..."
# 设置可执行权限后，将临时文件原子地替换为目标文件
ssh "${REMOTE}" "cd '${REMOTE_RUNTIME_DIR}' && rm -rf static.old && chmod 0755 .qq-maid-bot.new .botctl.sh.new .diagnose-network.sh.new .validate-runtime.sh.new && mv -f .qq-maid-bot.new qq-maid-bot && mv -f .botctl.sh.new botctl.sh && mv -f .diagnose-network.sh.new diagnose-network.sh && mv -f .validate-runtime.sh.new validate-runtime.sh && find . -maxdepth 1 -type f -name 'qq-maid-*' ! -name 'qq-maid-bot' -delete && find . -maxdepth 1 -type f -name '*ctl.sh' ! -name 'botctl.sh' -delete && { test ! -d static || mv static static.old; } && mv .static.new static && rm -rf static.old"

echo "==> Restarting remote services..."
# 重启统一服务。旧双进程文件在安装阶段清理，避免同机残留旧入口。
SECONDS=0
ssh "${REMOTE}" "cd '${REMOTE_DIR}' && ./runtime/botctl.sh restart"
RESTART_ELAPSED="${SECONDS}"

echo "==> Checking processes..."
# 检查服务是否已重新拉起
ssh "${REMOTE}" "ps aux | grep -E 'qq-maid-bot' | grep -v grep || true"

echo "==> Done."
printf '  构建 %ds | 重启 %ds | 总计 %ds\n' \
    "${BUILD_ELAPSED}" "${RESTART_ELAPSED}" "$((BUILD_ELAPSED + RESTART_ELAPSED))"

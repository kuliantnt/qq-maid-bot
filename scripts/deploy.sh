#!/usr/bin/env bash
set -euo pipefail

# ============================================================
# deploy.sh - 构建并部署 qq-maid 项目到远程服务器
#
# 远程主机: aliyun
# 远程路径: /root/project/qqbot
# 部署组件: qq-maid-gateway-rs, qq-maid-llm, 控制脚本与诊断工具
# ============================================================

REMOTE="aliyun"
REMOTE_DIR="/root/project/qqbot"
REMOTE_RUNTIME_DIR="${REMOTE_DIR}/runtime"

echo "==> Building release..."
make build

echo "==> Uploading artifacts..."
# runtime 是远端运行目录，专门放二进制、控制脚本和运行期配置。
ssh "${REMOTE}" "mkdir -p '${REMOTE_RUNTIME_DIR}'"

# 将编译产物和脚本上传为 .new 临时文件，避免覆盖正在运行的服务
scp target/release/qq-maid-gateway-rs "${REMOTE}:${REMOTE_RUNTIME_DIR}/.qq-maid-gateway-rs.new"
scp target/release/qq-maid-llm "${REMOTE}:${REMOTE_RUNTIME_DIR}/.qq-maid-llm.new"
scp scripts/llmctl.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.llmctl.sh.new"
scp scripts/gatewayctl.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.gatewayctl.sh.new"
scp scripts/diagnose-network.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.diagnose-network.sh.new"

echo "==> Installing artifacts..."
# 设置可执行权限后，将临时文件原子地替换为目标文件
ssh "${REMOTE}" "cd '${REMOTE_RUNTIME_DIR}' && chmod 0755 .qq-maid-gateway-rs.new .qq-maid-llm.new .llmctl.sh.new .gatewayctl.sh.new .diagnose-network.sh.new && mv -f .qq-maid-gateway-rs.new qq-maid-gateway-rs && mv -f .qq-maid-llm.new qq-maid-llm && mv -f .llmctl.sh.new llmctl.sh && mv -f .gatewayctl.sh.new gatewayctl.sh && mv -f .diagnose-network.sh.new diagnose-network.sh"

echo "==> Restarting remote services..."
# 依次重启 LLM 和 gateway 服务。服务器旧 llm/ 目录的迁移由运维手动处理。
ssh "${REMOTE}" "cd '${REMOTE_DIR}' && ./runtime/llmctl.sh restart && ./runtime/gatewayctl.sh restart"

echo "==> Checking processes..."
# 检查服务是否已重新拉起
ssh "${REMOTE}" "ps aux | grep -E 'qq-maid-llm|qq-maid-gateway-rs' | grep -v grep || true"

echo "==> Done."

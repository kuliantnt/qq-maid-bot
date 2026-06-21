#!/usr/bin/env bash
set -euo pipefail

# ============================================================
# deploy-local.sh - 构建并部署 qq-maid 项目到本地 runtime/ 目录
#
# 部署目标: 仓库根目录下的 runtime/
# 部署组件: qq-maid-gateway-rs, qq-maid-llm, 控制脚本与诊断工具
# ============================================================

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"
RUNTIME_DIR="${REPO_DIR}/runtime"

echo "==> Building release..."
make build

echo "==> Installing artifacts to ${RUNTIME_DIR}..."
# 复用 make install 逻辑：构建产物、控制脚本和诊断工具安装到 runtime/
make install

echo "==> Restarting local services..."
"${RUNTIME_DIR}/llmctl.sh" restart
"${RUNTIME_DIR}/gatewayctl.sh" restart

echo "==> Checking processes..."
ps aux | grep -E 'qq-maid-llm|qq-maid-gateway-rs' | grep -v grep || true

echo "==> Done."

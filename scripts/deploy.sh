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
LOCAL_VALIDATE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/qqbot-deploy-validate.XXXXXX")"

cleanup_local_validate_dir() {
    rm -rf "${LOCAL_VALIDATE_DIR}"
}

prepare_validate_runtime() {
    install -d "${LOCAL_VALIDATE_DIR}/static"
    install -m 0755 target/release/qq-maid-gateway-rs "${LOCAL_VALIDATE_DIR}/qq-maid-gateway-rs"
    install -m 0755 target/release/qq-maid-llm "${LOCAL_VALIDATE_DIR}/qq-maid-llm"
    install -m 0755 scripts/llmctl.sh "${LOCAL_VALIDATE_DIR}/llmctl.sh"
    install -m 0755 scripts/gatewayctl.sh "${LOCAL_VALIDATE_DIR}/gatewayctl.sh"
    install -m 0755 scripts/botctl.sh "${LOCAL_VALIDATE_DIR}/botctl.sh"
    install -m 0755 scripts/diagnose-network.sh "${LOCAL_VALIDATE_DIR}/diagnose-network.sh"
    install -m 0755 scripts/validate-runtime.sh "${LOCAL_VALIDATE_DIR}/validate-runtime.sh"
    install -m 0644 runtime/.env.example "${LOCAL_VALIDATE_DIR}/.env.example"
    install -m 0644 runtime/README.md "${LOCAL_VALIDATE_DIR}/README.md"
    cp -R runtime/static/. "${LOCAL_VALIDATE_DIR}/static/"
}

trap cleanup_local_validate_dir EXIT

echo "==> Building release..."
SECONDS=0
make build
BUILD_ELAPSED="${SECONDS}"

echo "==> Validating release payload..."
# validate-runtime 只适合检查待发布包；live runtime 目录天然会包含 .env、
# 数据库、日志和 pid 等运行产物，不能在重启后的运行目录上执行这一步。
prepare_validate_runtime
bash scripts/validate-runtime.sh "${LOCAL_VALIDATE_DIR}"

echo "==> Uploading artifacts..."
# runtime 是远端运行目录，专门放二进制、控制脚本、配置模板和运行期文件。
ssh "${REMOTE}" "mkdir -p '${REMOTE_RUNTIME_DIR}'"

# 将编译产物、脚本和配置模板上传为 .new 临时文件，避免覆盖正在运行的服务
scp target/release/qq-maid-gateway-rs "${REMOTE}:${REMOTE_RUNTIME_DIR}/.qq-maid-gateway-rs.new"
scp target/release/qq-maid-llm "${REMOTE}:${REMOTE_RUNTIME_DIR}/.qq-maid-llm.new"
scp scripts/llmctl.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.llmctl.sh.new"
scp scripts/gatewayctl.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.gatewayctl.sh.new"
scp scripts/botctl.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.botctl.sh.new"
scp scripts/diagnose-network.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.diagnose-network.sh.new"
scp scripts/validate-runtime.sh "${REMOTE}:${REMOTE_RUNTIME_DIR}/.validate-runtime.sh.new"
scp runtime/.env.example "${REMOTE}:${REMOTE_RUNTIME_DIR}/.env.example"
scp runtime/README.md "${REMOTE}:${REMOTE_RUNTIME_DIR}/README.md"
scp -r runtime/static "${REMOTE}:${REMOTE_RUNTIME_DIR}/.static.new"

echo "==> Installing artifacts..."
# 设置可执行权限后，将临时文件原子地替换为目标文件
ssh "${REMOTE}" "cd '${REMOTE_RUNTIME_DIR}' && rm -rf static.old && chmod 0755 .qq-maid-gateway-rs.new .qq-maid-llm.new .llmctl.sh.new .gatewayctl.sh.new .botctl.sh.new .diagnose-network.sh.new .validate-runtime.sh.new && mv -f .qq-maid-gateway-rs.new qq-maid-gateway-rs && mv -f .qq-maid-llm.new qq-maid-llm && mv -f .llmctl.sh.new llmctl.sh && mv -f .gatewayctl.sh.new gatewayctl.sh && mv -f .botctl.sh.new botctl.sh && mv -f .diagnose-network.sh.new diagnose-network.sh && mv -f .validate-runtime.sh.new validate-runtime.sh && { test ! -d static || mv static static.old; } && mv .static.new static && rm -rf static.old"

echo "==> Restarting remote services..."
# 依次重启 LLM 和 gateway 服务。服务器旧 llm/ 目录的迁移由运维手动处理。
SECONDS=0
ssh "${REMOTE}" "cd '${REMOTE_DIR}' && ./runtime/llmctl.sh restart && ./runtime/gatewayctl.sh restart"
RESTART_ELAPSED="${SECONDS}"

echo "==> Checking processes..."
# 检查服务是否已重新拉起
ssh "${REMOTE}" "ps aux | grep -E 'qq-maid-llm|qq-maid-gateway-rs' | grep -v grep || true"

echo "==> Done."
printf '  构建 %ds | 重启 %ds | 总计 %ds\n' \
    "${BUILD_ELAPSED}" "${RESTART_ELAPSED}" "$((BUILD_ELAPSED + RESTART_ELAPSED))"

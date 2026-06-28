#!/usr/bin/env bash
# 同步本地知识库到远程服务器 (aliyun)
# 用法:
#   bash scripts/sync_knowledge.sh          # 执行同步
#   bash scripts/sync_knowledge.sh --dry-run # 仅预览差异

set -euo pipefail

REMOTE="aliyun"
REMOTE_BASE="/root/project/qqbot/runtime/config/knowledge"

# 本地目录 -> 远程子目录 映射
declare -A SYNC_MAP=(
  ["/home/lianlian/project/Multiple_personality_system_wiki/dist/knowledge/entries"]="mps_wiki"
  ["/home/lianlian/apps/innerworld/docs"]="inner_world"
)

RSYNC_FLAGS="-avz --progress"
if [[ "${1:-}" == "--dry-run" ]]; then
  RSYNC_FLAGS="$RSYNC_FLAGS --dry-run"
  echo "=== 预览模式 (--dry-run) ==="
fi

for LOCAL_DIR in "${!SYNC_MAP[@]}"; do
  SUB_DIR="${SYNC_MAP[$LOCAL_DIR]}"
  REMOTE_DIR="${REMOTE_BASE}/${SUB_DIR}"

  echo ""
  echo ">>> 同步: ${LOCAL_DIR}/ -> ${REMOTE}:${REMOTE_DIR}/"

  if [[ ! -d "$LOCAL_DIR" ]]; then
    echo "  [跳过] 本地目录不存在: $LOCAL_DIR"
    continue
  fi

  rsync ${RSYNC_FLAGS} \
    --include='*/' --include='*.md' --exclude='*' \
    "${LOCAL_DIR}/" "${REMOTE}:${REMOTE_DIR}/"
done

echo ""
echo "=== 同步完成 ==="

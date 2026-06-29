#!/usr/bin/env bash
# 同步本地知识库 markdown 文件到远程服务器 (镜像语义)
#
# 用法:
#   bash scripts/sync_knowledge.sh          # 执行同步
#   bash scripts/sync_knowledge.sh --dry-run # 仅预览差异
#
# 配置文件: scripts/deploy.conf (与 deploy-remote.sh 共用)
#   首次使用请从 deploy.conf.example 复制并修改。
#
# 同步语义:
#   对每个 SYNC_MAP 条目，把本地目录下的 Markdown (.md) 镜像同步到
#   远端对应子目录。所谓“镜像”指：本地已删除或重命名的 .md 文件会
#   在本次同步中被从远端对应子目录里删除。
#   删除范围严格限定在每个映射条目对应的远端子目录内部，绝不会影响
#   该子目录之外的任何文件。
#   非 .md 文件不在传输/删除范围内：本地上传不含非 md 文件，远端非
#   md 文件也不会被删除 (rsync --delete 不会触及被 exclude 的文件)。
#
# 远端知识库根目录:
#   优先使用 deploy.conf 中的 REMOTE_KNOWLEDGE_DIR (用于支持应用通过
#   KNOWLEDGE_DIR 读取外部知识目录的部署方式)。
#   未设置或为空时默认使用: ${REMOTE_PROJECT_DIR}/runtime/config/knowledge
#   每个 SYNC_MAP 条目的远端目标为: ${REMOTE_KNOWLEDGE_DIR 或默认路径}/${子目录}/

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DEPLOY_CONF="${SCRIPT_DIR}/deploy.conf"
DRY_RUN=""

# --- 参数解析 ---
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run)
      DRY_RUN="--dry-run"
      shift
      ;;
    -h|--help)
      echo "用法: bash scripts/sync_knowledge.sh [--dry-run]"
      echo ""
      echo "  镜像同步本地知识库 Markdown 到远端 (与 deploy-remote.sh 共用 deploy.conf)。"
      echo "  本地删除的 .md 会在对应远端子目录中被删除，删除范围仅限该子目录内部。"
      echo "  非 .md 文件不会被传输或删除。"
      echo ""
      echo "  远端知识库根目录由 deploy.conf 的 REMOTE_KNOWLEDGE_DIR 控制，"
      echo '  未设置时默认使用 ${REMOTE_PROJECT_DIR}/runtime/config/knowledge。'
      echo ""
      echo "  --dry-run 仅预览差异，不实际传输或删除。"
      exit 0
      ;;
    *)
      echo "未知参数: $1"
      exit 1
      ;;
  esac
done

# --- 加载配置 ---
if [[ ! -f "$DEPLOY_CONF" ]]; then
  echo "[错误] 配置文件不存在: $DEPLOY_CONF"
  echo "  请从 deploy.conf.example 复制并填入实际值。"
  exit 1
fi
source "$DEPLOY_CONF"

# --- 校验 ---
if [[ -z "${REMOTE_HOST:-}" ]]; then
  echo "[错误] deploy.conf 缺少 REMOTE_HOST"
  exit 1
fi
if [[ -z "${REMOTE_PROJECT_DIR:-}" ]]; then
  echo "[错误] deploy.conf 缺少 REMOTE_PROJECT_DIR"
  exit 1
fi
# set -u 下直接访问未声明的数组会触发 unbound variable。
# Bash 5.2 中 ${SYNC_MAP+x} 对 \"声明但为空数组\" 与 \"完全未声明\" 均返回空，
# 无法区分，因此改用 declare -p 探测变量是否被声明：
#   未声明 → declare -p 失败 (rc=1)，输出“未声明”友好错误；
#   声明为空 SYNC_MAP=() → declare -p 成功，再由 ${#SYNC_MAP[@]} 判空，
#   输出“缺少条目”错误。两种情况都以非零退出，且不暴露 unbound 错误。
if ! declare -p SYNC_MAP >/dev/null 2>&1; then
  echo "[错误] deploy.conf 未声明 SYNC_MAP，请按 \"本地目录|远端子目录\" 格式定义至少一条映射"
  exit 1
fi
if [[ ${#SYNC_MAP[@]} -eq 0 ]]; then
  echo "[错误] deploy.conf 缺少 SYNC_MAP 条目，请至少配置一条 \"本地目录|远端子目录\" 映射"
  exit 1
fi

# 校验每个映射条目，提前拦截非法配置，避免拼出越界或绝对路径。
# 约束: 必须为 "本地目录|远端子目录"；两侧非空；远端子目录必须是
# 相对路径，禁止绝对路径或 .. 路径穿越，确保 --delete 只在受控子目录内执行。
for ENTRY in "${SYNC_MAP[@]}"; do
  if [[ "$ENTRY" != *"|"* ]]; then
    echo "[错误] SYNC_MAP 条目缺少分隔符 |: '$ENTRY'，应为 '本地目录|远端子目录'"
    exit 1
  fi
  MAP_LOCAL="${ENTRY%%|*}"
  MAP_SUB="${ENTRY##*|}"
  if [[ -z "$MAP_LOCAL" ]]; then
    echo "[错误] SYNC_MAP 条目本地目录为空: '$ENTRY'"
    exit 1
  fi
  if [[ -z "$MAP_SUB" ]]; then
    echo "[错误] SYNC_MAP 条目远端子目录为空: '$ENTRY'"
    exit 1
  fi
  # 禁止绝对路径 (/开头) 和 .. 路径穿越，限制删除范围在受控子目录内
  if [[ "$MAP_SUB" == /* ]]; then
    echo "[错误] SYNC_MAP 远端子目录必须是相对路径，禁止绝对路径: '$ENTRY'"
    exit 1
  fi
  if [[ "$MAP_SUB" == *..* ]]; then
    echo "[错误] SYNC_MAP 远端子目录不允许包含 .. 以防止路径穿越: '$ENTRY'"
    exit 1
  fi
done

# 远端知识库根目录: 优先 REMOTE_KNOWLEDGE_DIR (兼容应用用 KNOWLEDGE_DIR
# 读取外部目录的部署方式)，未设置或为空时回退到默认项目内路径。
# 注意 ${REMOTE_KNOWLEDGE_DIR:-} 在 set -u 下安全: 未声明时展开为空。
REMOTE_KBASE="${REMOTE_KNOWLEDGE_DIR:-${REMOTE_PROJECT_DIR}/runtime/config/knowledge}"

# --- 执行同步 ---
# --delete 实现“镜像”语义：远端对应子目录中本地不存在的 .md 会被删除。
# 由于 include/exclude 规则把非 .md 文件置于 *,exclude，--delete 不会触及
# 这些被排除的文件，因此远端非 .md 文件保持原样，不会被误删。
# 删除范围被 rsync 自动限定在本次传输的远端目标子目录内部，绝不外溢。
RSYNC_FLAGS="-avz --progress --delete"
if [[ -n "$DRY_RUN" ]]; then
  RSYNC_FLAGS="$RSYNC_FLAGS --dry-run"
  echo "=== 预览模式 (--dry-run)，不会实际传输或删除 ==="
fi

OK=0
SKIP=0
FAIL=0

for ENTRY in "${SYNC_MAP[@]}"; do
  LOCAL_DIR="${ENTRY%%|*}"
  SUB_DIR="${ENTRY##*|}"

  echo ""
  echo ">>> 同步: ${LOCAL_DIR}/ -> ${REMOTE_HOST}:${REMOTE_KBASE}/${SUB_DIR}/"

  if [[ ! -d "$LOCAL_DIR" ]]; then
    echo "  [跳过] 本地目录不存在: $LOCAL_DIR"
    SKIP=$((SKIP + 1))
    continue
  fi

  # 失败用 if 判定收集计数，不能让 set -e 直接退出，否则无法输出汇总。
  # 本次同步的状态由循环结束后的汇总决定 (见文末)。
  if rsync ${RSYNC_FLAGS} \
    --include='*/' --include='*.md' --exclude='*' \
    "${LOCAL_DIR}/" "${REMOTE_HOST}:${REMOTE_KBASE}/${SUB_DIR}/"; then
    OK=$((OK + 1))
  else
    echo "  [失败] rsync 返回非零: ${LOCAL_DIR}"
    FAIL=$((FAIL + 1))
  fi
done

echo ""
echo "=== 同步完成: 成功 ${OK}, 跳过 ${SKIP}, 失败 ${FAIL} ==="

# 任意 rsync 失败时整体以非零退出，供 CI / 自动化判定结果。
# 不能只在日志里打印失败计数后仍返回成功 (修前 bug)。
if [[ "$FAIL" -ne 0 ]]; then
  exit 1
fi
exit 0

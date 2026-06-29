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

# --- 预解析全部 SYNC_MAP，并在执行任何 rsync 前完成完整校验 ---
# 先解析再校验，是因为 --delete 具有删除能力：若部分条目合法、部分非法，
# 边执行边报错会让合法条目先同步，产生“只同步了一部分”的中间结果，
# 甚至让一条映射把另一条映射刚同步的内容删掉。全部预校验可避免部分同步。
#
# 解析约束：条目必须“恰好包含一个 |”。用 ${ENTRY%%|*}/${ENTRY##*|} 会在
# 含多个 | 时静默丢弃中间段，因此这里先计算分隔符数量再判定。
#
# 每条映射拆成两个并行数组：
#   MAP_LOCALS[i] / MAP_SUBS[i] / MAP_RAW[i] (原始条目，用于错误提示)
MAP_LOCALS=()
MAP_SUBS=()
MAP_RAW=()
for ENTRY in "${SYNC_MAP[@]}"; do
  # 条目必须恰好包含一个 |：
  # 先把非 | 字符全部删掉，只剩 | 序列，其长度即 | 的个数。
  # 注意 bash 不支持 ${#VAR//PAT/} (长度与替换不能嵌套)，因此先赋中间变量。
  # 这里拒绝 0 个 | (缺分隔符) 和 ≥2 个 | (中间段会被 ${ENTRY%%|*} 静默丢弃)。
  BARS_ONLY=${ENTRY//[!|]/}
  BAR_COUNT=${#BARS_ONLY}
  if [[ "$BAR_COUNT" -ne 1 ]]; then
    echo "[错误] SYNC_MAP 条目必须包含且仅包含一个分隔符 |: '$ENTRY'，应为 '本地目录|远端子目录'"
    exit 1
  fi
  MAP_LOCAL="${ENTRY%%|*}"
  MAP_SUB="${ENTRY#*|}"
  if [[ -z "$MAP_LOCAL" ]]; then
    echo "[错误] SYNC_MAP 条目本地目录为空: '$ENTRY'"
    exit 1
  fi
  if [[ -z "$MAP_SUB" ]]; then
    echo "[错误] SYNC_MAP 条目远端子目录为空: '$ENTRY'"
    exit 1
  fi
  MAP_LOCALS+=("$MAP_LOCAL")
  MAP_SUBS+=("$MAP_SUB")
  MAP_RAW+=("$ENTRY")
done

# 严格校验每个远端子目录并规范化为唯一形式。规范化后才用于冲突判断，
# 避免不同写法 (如 wiki 与 wiki/) 被误判为不同目标后又互相删除文件。
# 规范化结果存入 MAP_SUBS_NORM，后续 rsync 也用规范形式作为实际目标，
# 保证冲突检查所见即 rsync 所写。
MAP_SUBS_NORM=()
for i in "${!MAP_SUBS[@]}"; do
  SUB="${MAP_SUBS[$i]}"
  RAW="${MAP_RAW[$i]}"
  # 禁止绝对路径：/ 开头会跳出 REMOTE_KBASE 划定的受控子目录范围
  if [[ "$SUB" == /* ]]; then
    echo "[错误] SYNC_MAP 远端子目录必须是相对路径，禁止绝对路径: '$RAW'"
    exit 1
  fi
  # 逐段拆解并规范化：折结空段、. 与 ..。
  # 为检测“根目标”与越界，保留一个隐含的根标记逻辑：
  #   - 任一段为空 (重复斜杠 wiki//a) → 拒绝；
  #   - 任一段为 .  → 拒绝，含 ./ 、 wiki/. 、 a/./b 等；
  #   - 任一段为 .. → 拒绝，含 ../ 、wiki/.. 、 wiki/../a 等。
  # 拒绝 . 能避免“wiki|.”这类指向知识库根的映射与其他映射 (或全库) 互删。
  NORM=""
  SEG_IDX=0
  # 在两端补 / 以保证首尾段不会被漏掉，例如 . 、 foo/ 同时可被按段检查。
  REMAINDER="${SUB}/"
  while [[ -n "$REMAINDER" ]]; do
    SEG="${REMAINDER%%/*}"
    REST="${REMAINDER#*/}"
    REMAINDER="$REST"
    if [[ -z "$SEG" ]]; then
      # 首空段 = 绝对路径 (已拦)，中间/末尾空段 = 重复斜杠 (wiki//a) 或尾随斜杠 (wiki/)
      echo "[错误] SYNC_MAP 远端子目录包含空路径段或重复斜杠: '$RAW'"
      exit 1
    fi
    if [[ "$SEG" == "." ]]; then
      echo "[错误] SYNC_MAP 远端子目录不允许使用 '.' 段: '$RAW'"
      exit 1
    fi
    if [[ "$SEG" == ".." ]]; then
      echo "[错误] SYNC_MAP 远端子目录不允许包含 '..' 以防止路径穿越: '$RAW'"
      exit 1
    fi
    if [[ $SEG_IDX -eq 0 ]]; then
      NORM="$SEG"
    else
      NORM="${NORM}/${SEG}"
    fi
    SEG_IDX=$((SEG_IDX + 1))
  done
  if [[ -z "$NORM" ]]; then
    # 例如原始为空 (已在上面拦) 或被上面的逻辑推到，这里二次防御
    echo "[错误] SYNC_MAP 远端子目录规范化后为空: '$RAW'"
    exit 1
  fi
  MAP_SUBS_NORM+=("$NORM")
done

# 检查所有远端子目录目标之间是否存在重复或父子重叠冲突。
# 因为 --delete 会在各自子目录内删除本地不存在的 .md，若两个映射指向
# 同一目标或一个是另一个的父目录，后者会删掉前者刚写的内容，或在
# 重复目标上发生互相覆盖。以规范后的段序列比较，避免写法不同被漏掉。
# 同时将上一轮已见到的目标计入冲突信息，让错误能展示两个冲突目标。
# 冲突检查发生在任何 rsync 之前，保证不存在部分同步结果。
SEEN_NORM=()        # 已见到的规范路径
SEEN_RAW=()         # 对应原始条目，用于冲突报错可读
for i in "${!MAP_SUBS_NORM[@]}"; do
  CUR="${MAP_SUBS_NORM[$i]}"
  CUR_RAW="${MAP_RAW[$i]}"
  for j in "${!SEEN_NORM[@]}"; do
    PREV="${SEEN_NORM[$j]}"
    PREV_RAW="${SEEN_RAW[$j]}"
    if [[ "$CUR" == "$PREV" ]]; then
      echo "[错误] SYNC_MAP 发现重复远端目标: '$CUR_RAW' 与 '$PREV_RAW' 均指向 '$CUR'"
      exit 1
    fi
    # 父子重叠：以“段级前缀”判定。CUR 与 PREV 并存时，均是以 / 划分后的序段序列。
    # 若 A 子目录是 B 的父目录，则 B 序列 = A 序列 + "/...". 例子：wiki vs wiki/private。
    # 这里用 “A / ”前缀比较避免 wiki 与 wiki2 被误判 (段级而非字符串级)。
    if [[ "$CUR" == "${PREV}/"* || "$PREV" == "${CUR}/"* ]]; then
      echo "[错误] SYNC_MAP 发现父子重叠远端目标: '$CUR_RAW' 与 '$PREV_RAW' (路径 $CUR / $PREV)"
      exit 1
    fi
  done
  SEEN_NORM+=("$CUR")
  SEEN_RAW+=("$CUR_RAW")
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
FAIL=0

# 同步循环使用预解析后的 MAP_LOCALS / MAP_SUBS。原始 SUB_SPACE 写法用于日志输出，
# 实际远端路径拼接用规范后的 MAP_SUBS_NORM 以保证冲突检查与实际目标一致。
for i in "${!MAP_LOCALS[@]}"; do
  LOCAL_DIR="${MAP_LOCALS[$i]}"
  SUB_DIR="${MAP_SUBS_NORM[$i]}"

  echo ""
  echo ">>> 同步: ${LOCAL_DIR}/ -> ${REMOTE_HOST}:${REMOTE_KBASE}/${SUB_DIR}/"

  # 本地源目录不存在不再是可忽略的 SKIP：--delete 镜像语义下，
  # 如果当作成功跳过，会在远端残留旧 .md 而脚本仍返回 0，与镜像语义矛盾。
  # 参与计入失败计数，由文末汇总判定整体退出码，避免“返回成功但远端末干净”。
  if [[ ! -d "$LOCAL_DIR" ]]; then
    echo "  [失败] 本地源目录不存在: $LOCAL_DIR"
    FAIL=$((FAIL + 1))
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
echo "=== 同步完成: 成功 ${OK}, 失败 ${FAIL} ==="

# 任意 rsync 失败时整体以非零退出，供 CI / 自动化判定结果。
# 不能只在日志里打印失败计数后仍返回成功 (修前 bug)。
if [[ "$FAIL" -ne 0 ]]; then
  exit 1
fi
exit 0

#!/usr/bin/env bash
# sync_knowledge.sh 回归测试。
#
# 思路: 用一个 fake rsync 把 sync_knowledge.sh 调用中的最后一个 host:path
# 形参改写成本地临时目录，复用真实 /usr/bin/rsync 完成实际镜像动作，
# 从而在不需要真实服务器的情况下验证退出码、镜像删除、删除范围限定、
# 配置校验、目标冲突等行为。
#
# 用法:
#   bash scripts/tests/test-sync-knowledge.sh
#
# 退出码: 全部通过返回 0，任一失败返回 1。临时目录在退出前清理。

set -uo pipefail

# 仓库根目录: 本脚本位于 <repo>/scripts/tests/，向上两级即仓库根。
TEST_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$TEST_DIR/../.." && pwd)"
SCRIPT="$REPO_ROOT/scripts/sync_knowledge.sh"

# 同步脚本固定读取 scripts/deploy.conf，测试期间向其写入临时配置并清理。
DEPLOY_CONF="$REPO_ROOT/scripts/deploy.conf"

PASS=0
FAIL=0
TMPROOT=""

ok() { echo "  PASS: $1"; PASS=$((PASS + 1)); }
ko() { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); }

cleanup() {
  rm -rf "$TMPROOT"
  rm -f "$DEPLOY_CONF"
}
trap cleanup EXIT

# 准备一个 fake rsync + fake ssh 的临时环境到一个独立的 case 根目录。
# 返回的 TROOT 变量供单个用例使用。fake rsync 会把 host:path 改写到
# $FAKE_REMOTE_ROOT/path 之下，并按需预创建目标目录。
setup_fake() {
  TROOT="$1"
  rm -rf "$TROOT"
  mkdir -p "$TROOT/bin" "$TROOT/local" "$TROOT/remote"
  cat > "$TROOT/bin/rsync" <<'RSYNC'
#!/usr/bin/env bash
# 拦截并把最后一个 host:path 形参改写为本地方向，再调用真实 rsync。
real=/usr/bin/rsync
args=()
for a in "$@"; do
  # 仅匹配形如 host:path 的远端目标 (不以 - 开头、含冒号、非绝对路径)。
  if [[ "$a" == *":"* && "$a" != /* && "$a" != -* ]]; then
    path="${a#*:}"
    dest="${FAKE_REMOTE_ROOT}/${path}"
    mkdir -p "$dest"
    args+=("$dest")
  else
    args+=("$a")
  fi
done
# 可选: 命中关键字时模拟 rsync 失败，用于验证失败退出码。
if [[ -n "${FAKE_RSYNC_FAIL_ON:-}" ]]; then
  for a in "${args[@]}"; do
    if [[ "$a" == *"$FAKE_RSYNC_FAIL_ON"* ]]; then
      echo "fake-rsync: simulated failure for $a" >&2
      exit 1
    fi
  done
fi
exec "$real" "${args[@]}"
RSYNC
  chmod +x "$TROOT/bin/rsync"
  cat > "$TROOT/bin/ssh" <<'SSH'
#!/usr/bin/env bash
exit 0
SSH
  chmod +x "$TROOT/bin/ssh"
}

# 把给定 stdin 写入 $DEPLOY_CONF (供 source)。
putconf() { cat > "$DEPLOY_CONF"; }

# 运行被测脚本时注入 fake PATH 与 FAKE_REMOTE_ROOT。
run() {
  PATH="$TROOT/bin:$PATH" FAKE_REMOTE_ROOT="$TROOT/remote" bash "$SCRIPT" "$@"
}

COUNTER=0
newcase() {
  COUNTER=$((COUNTER + 1))
  TROOT="$TMPROOT/c${COUNTER}"
  setup_fake "$TROOT"
}

# ---------------------------------------------------------------------------
TMPROOT="$(mktemp -d "${TMPDIR:-/tmp}/sync-knowledge-tests.XXXXXX")"

# --- 1. 缺失 SYNC_MAP (完全不声明) ---
newcase
putconf <<'CONF'
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
CONF
out=$(run 2>&1); rc=$?
if echo "$out" | grep -q '未声明 SYNC_MAP' && [[ $rc -ne 0 ]] && ! echo "$out" | grep -qi 'unbound'; then
  ok "缺失 SYNC_MAP → 友好错误 + 非零 + 无 unbound"
else
  ko "缺失SYNC_MAP: rc=$rc out=$out"
fi

# --- 2. 空 SYNC_MAP=() ---
newcase
putconf <<'CONF'
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=()
CONF
out=$(run 2>&1); rc=$?
if echo "$out" | grep -q '缺少 SYNC_MAP 条目' && [[ $rc -ne 0 ]]; then
  ok "空 SYNC_MAP → 拒绝 + 非零"
else
  ko "空SYNC_MAP: rc=$rc out=$out"
fi

# --- 3. 缺少分隔符 ---
newcase
putconf <<'CONF'
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("wikinocthing")
CONF
out=$(run 2>&1); rc=$?
if [[ $rc -ne 0 ]] && echo "$out" | grep -q '且仅包含一个分隔符'; then
  ok "缺少分隔符被拒绝"
else
  ko "缺少分隔符: rc=$rc out=$out"
fi

# --- 4. 多个分隔符 ---
newcase
putconf <<'CONF'
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("/home/x/wiki|wiki|extra")
CONF
out=$(run 2>&1); rc=$?
if [[ $rc -ne 0 ]] && echo "$out" | grep -q '且仅包含一个分隔符'; then
  ok "多个分隔符被拒绝 (中间段不被静默丢弃)"
else
  ko "多分隔符: rc=$rc out=$out"
fi

# --- 5. 本地目录为空 / 远端子目录为空 ---
newcase
putconf <<'CONF'
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("|wiki")
CONF
out=$(run 2>&1); rc=$?
[[ $rc -ne 0 ]] && echo "$out" | grep -q '本地目录为空' && ok "本地目录空被拒" || ko "本地空: rc=$rc out=$out"

newcase
putconf <<'CONF'
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("/home/x/wiki|")
CONF
out=$(run 2>&1); rc=$?
[[ $rc -ne 0 ]] && echo "$out" | grep -q '远端子目录为空' && ok "远端子目录空被拒" || ko "远端空: rc=$rc out=$out"

# --- 6. 非法子目录路径 (绝对 / 空段重复斜杠 / . / ..) ---
bad_cases=(
  "wiki|/abs"          # 绝对路径
  "wiki|wiki//a"       # 重复斜杠
  "wiki|wiki/"         # 尾随斜杠 → 末尾空段
  "wiki|."             # 根目录别名
  "wiki|./"            # 根目录别名变体
  "wiki|./wiki"        # 前导 ./
  "wiki|wiki/."        # 末尾 .
  "wiki|wiki/../a"     # .. 穿越
  "wiki|a/../b"        # .. 归一后段
)
for bad in "${bad_cases[@]}"; do
  newcase
  putconf <<CONF
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("$bad")
CONF
  out=$(run 2>&1); rc=$?
  if [[ $rc -ne 0 ]] && echo "$out" | grep -q '\[错误\]'; then
    ok "拒绝非法子目录 [$bad]"
  else
    ko "非法子目录未拒 [$bad] rc=$rc out=$out"
  fi
done

# --- 7. 重复目标: wiki 与 wiki ---
newcase
mkdir -p "$TROOT/local/a" "$TROOT/local/b"
echo "a" > "$TROOT/local/a/a.md"
echo "b" > "$TROOT/local/b/b.md"
putconf <<CONF
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("$TROOT/local/a|wiki" "$TROOT/local/b|wiki")
CONF
out=$(run 2>&1); rc=$?
# 重复目标必须发生在任何 rsync 之前，所以远端不应被写入。
if [[ $rc -ne 0 ]] && echo "$out" | grep -q '重复远端目标' && [ -z "$(ls -A "$TROOT/remote" 2>/dev/null)" ]; then
  ok "重复目标 wiki|wiki 冲突报错且未触发同步"
else
  ko "重复目标: rc=$rc out=$out remote=$(ls -A "$TROOT/remote" 2>/dev/null)"
fi

# --- 8. 父子重叠: wiki 与 wiki/private ---
newcase
mkdir -p "$TROOT/local/a" "$TROOT/local/b"
echo "a" > "$TROOT/local/a/a.md"
echo "b" > "$TROOT/local/b/b.md"
putconf <<CONF
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("$TROOT/local/a|wiki" "$TROOT/local/b|wiki/private")
CONF
out=$(run 2>&1); rc=$?
if [[ $rc -ne 0 ]] && echo "$out" | grep -q '父子重叠' && [ -z "$(ls -A "$TROOT/remote" 2>/dev/null)" ]; then
  ok "父子重叠 wiki / wiki/private 冲突且未同步"
else
  ko "父子重叠: rc=$rc out=$out"
fi

# --- 9. 父子字母相邻但不应误判: wiki 与 wiki2 ---
newcase
mkdir -p "$TROOT/local/a" "$TROOT/local/b"
echo "a" > "$TROOT/local/a/a.md"
echo "b" > "$TROOT/local/b/b.md"
putconf <<CONF
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("$TROOT/local/a|wiki" "$TROOT/local/b|wiki2")
CONF
out=$(run 2>&1); rc=$?
# wiki 与 wiki2 是同级、段级前缀不重叠，应允许通过且成功同步两个目录。
if [[ $rc -eq 0 ]] && echo "$out" | grep -q '成功 2'; then
  ok "wiki 与 wiki2 不被误判为父子重叠"
else
  ko "wiki/wiki2 误判: rc=$rc out=$out"
fi

# --- 10. 本地源目录不存在 → 非零 ---
newcase
putconf <<'CONF'
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("/nonexistent/path/here|wiki")
CONF
out=$(run 2>&1); rc=$?
if [[ $rc -ne 0 ]] && echo "$out" | grep -q '本地源目录不存在'; then
  ok "源目录不存在 → 失败 + 非零"
else
  ko "源目录不存在: rc=$rc out=$out"
fi

# --- 11. 正常多映射同步成功 + 默认目标 ---
newcase
mkdir -p "$TROOT/local/wiki" "$TROOT/local/docs"
echo "w" > "$TROOT/local/wiki/w.md"
echo "d" > "$TROOT/local/docs/d.md"
putconf <<CONF
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("$TROOT/local/wiki|wiki" "$TROOT/local/docs|docs")
CONF
out=$(run 2>&1); rc=$?
base="$TROOT/remote/srv/qqbot/runtime/config/knowledge"
if [[ $rc -eq 0 ]] && echo "$out" | grep -q '成功 2' \
  && [ -f "$base/wiki/w.md" ] && [ -f "$base/docs/d.md" ]; then
  ok "正常多映射成功且默认目标正确"
else
  ko "正常多映射: rc=$rc out=$out"
fi

# --- 12. 自定义 REMOTE_KNOWLEDGE_DIR ---
newcase
mkdir -p "$TROOT/local/wiki"; echo "b" > "$TROOT/local/wiki/b.md"
putconf <<CONF
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
REMOTE_KNOWLEDGE_DIR="/opt/qqbot/private/config/knowledge"
SYNC_MAP=("$TROOT/local/wiki|wiki")
CONF
out=$(run 2>&1); rc=$?
tgt="$TROOT/remote/opt/qqbot/private/config/knowledge/wiki/b.md"
if [[ $rc -eq 0 ]] && [ -f "$tgt" ]; then
  ok "REMOTE_KNOWLEDGE_DIR 覆盖目标"
else
  ko "覆盖失败: rc=$rc out=$out"
fi

# --- 13. dry-run 删除预览 + 非实际删除 + 非md保留 ---
newcase
mkdir -p "$TROOT/local/wiki"; echo "keep" > "$TROOT/local/wiki/keep.md"
R="$TROOT/remote/srv/qqbot/runtime/config/knowledge/wiki"
mkdir -p "$R"
echo "old" > "$R/stale.md"
echo "txt"  > "$R/note.txt"
echo "remote-keep" > "$R/keep.md"
putconf <<CONF
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("$TROOT/local/wiki|wiki")
CONF
out=$(run --dry-run 2>&1); rc=$?
if echo "$out" | grep -q 'deleting' && echo "$out" | grep -q 'stale.md' && [ -f "$R/stale.md" ]; then
  dry_ok=1
else
  dry_ok=0
fi
out=$(run 2>&1); rc=$?
if [[ $dry_ok -eq 1 && $rc -eq 0 ]] && [ ! -f "$R/stale.md" ] && [ -f "$R/note.txt" ] && [ -f "$R/keep.md" ]; then
  ok "dry-run 预览删除 + 正式同步删除 stale.md 且保留 note.txt/keep.md"
else
  ko "dry-run/镜像删除: rc=$rc out=$out ls=$(ls "$R")"
fi

# --- 14. 删除范围限定在子目录内 ---
newcase
mkdir -p "$TROOT/local/wiki"; echo "x" > "$TROOT/local/wiki/x.md"
mkdir -p "$TROOT/remote/srv/qqbot/runtime/config/knowledge/wiki"
mkdir -p "$TROOT/remote/srv/qqbot/runtime/config/knowledge/other"
echo "other-md" > "$TROOT/remote/srv/qqbot/runtime/config/knowledge/other/only.md"
putconf <<CONF
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("$TROOT/local/wiki|wiki")
CONF
out=$(run 2>&1); rc=$?
if [[ $rc -eq 0 ]] && [ -f "$TROOT/remote/srv/qqbot/runtime/config/knowledge/other/only.md" ]; then
  ok "删除范围限定 wiki，other/only.md 保留"
else
  ko "越界删除: rc=$rc out=$out"
fi

# --- 15. rsync 一成一败 → 整体非零 + 汇总正确 ---
newcase
mkdir -p "$TROOT/local/a" "$TROOT/local/b"
echo "1" > "$TROOT/local/a/a.md"
echo "2" > "$TROOT/local/b/b.md"
putconf <<CONF
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("$TROOT/local/a|grp/a" "$TROOT/local/b|grp/b")
CONF
out=$(PATH="$TROOT/bin:$PATH" FAKE_REMOTE_ROOT="$TROOT/remote" FAKE_RSYNC_FAIL_ON="/grp/b" bash "$SCRIPT" 2>&1); rc=$?
if [[ $rc -ne 0 ]] && echo "$out" | grep -q '成功 1' && echo "$out" | grep -q '失败 1'; then
  ok "一成一败 → 整体非零且汇总显示成功 1 失败 1"
else
  ko "一成一败: rc=$rc out=$out"
fi

# --- 16. 所有校验失败时零次 rsync (用 fake rsync 计数) ---
newcase
# 在 fake rsync 内加一个计数文件，验证冲突报错分支没调用 rsync。
cat > "$TROOT/bin/rsync" <<'RSYNC'
#!/usr/bin/env bash
echo "1" >> "${RSYNC_HIT_FILE}"
exit 99
RSYNC
chmod +x "$TROOT/bin/rsync"
mkdir -p "$TROOT/local/a" "$TROOT/local/b"
echo "a" > "$TROOT/local/a/a.md"
echo "b" > "$TROOT/local/b/b.md"
putconf <<CONF
REMOTE_HOST="srv"
REMOTE_PROJECT_DIR="/srv/qqbot"
SYNC_MAP=("$TROOT/local/a|wiki" "$TROOT/local/b|wiki")
CONF
hit="$TROOT/rsync_hits"
out=$(PATH="$TROOT/bin:$PATH" RSYNC_HIT_FILE="$hit" bash "$SCRIPT" 2>&1); rc=$?
if [[ $rc -ne 0 ]] && echo "$out" | grep -q '重复远端目标' && [ ! -f "$hit" ]; then
  ok "冲突校验阶段未触发任何 rsync"
else
  ko "冲突期误调 rsync: rc=$rc hits=$([ -f "$hit" ] && cat "$hit" || echo none) out=$out"
fi

echo ""
echo "==== 总结: PASS=$PASS FAIL=$FAIL ===="
[[ $FAIL -eq 0 ]]
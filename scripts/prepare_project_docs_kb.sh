#!/usr/bin/env bash
# 把 GitHub Wiki + 仓库 CHANGELOG 整理成 knowledge 子目录，供 sync_knowledge.sh 同步。
#
# 用法:
#   bash scripts/prepare_project_docs_kb.sh
#   bash scripts/prepare_project_docs_kb.sh --sync
#   bash scripts/prepare_project_docs_kb.sh --sync --dry-run
#   bash scripts/prepare_project_docs_kb.sh --out /path/to/dir
#
# 默认输出:
#   runtime/config/knowledge/project_docs/
#
# 同步时复用:
#   scripts/sync_knowledge.sh
#   scripts/deploy.conf 的 REMOTE_* / SYNC_MAP
#
# 约定:
#   - 只写公开文档，不复制 .env、密钥、私有 prompt 或真实用户数据
#   - 输出目录中的 .md 由 .gitignore 忽略（knowledge 规则），可安全本地维护
#   - --sync 只是准备完后调用现有 sync_knowledge；镜像删除范围仍由 SYNC_MAP 子目录限定

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DEFAULT_OUT="$REPO_ROOT/runtime/config/knowledge/project_docs"
WIKI_URL="${WIKI_URL:-https://github.com/kuliantnt/qq-maid-bot.wiki.git}"
WIKI_CACHE="${WIKI_CACHE:-/tmp/qq-maid-bot.wiki}"
QQ_GROUP_URL="${QQ_GROUP_URL:-https://qm.qq.com/q/iAZxBO66EE}"
QQ_GROUP_NAME="${QQ_GROUP_NAME:-雪主任的工坊}"

OUT_DIR="$DEFAULT_OUT"
DO_SYNC=0
DRY_RUN=0

usage() {
  cat <<'EOF'
用法: bash scripts/prepare_project_docs_kb.sh [选项]

  拉取/更新 GitHub Wiki，连同仓库 CHANGELOG 生成 knowledge/project_docs。

选项:
  --out DIR       输出目录（默认 runtime/config/knowledge/project_docs）
  --sync          生成后调用 scripts/sync_knowledge.sh
  --dry-run       与 --sync 联用时，把 --dry-run 传给 sync_knowledge
  -h, --help      显示帮助

环境变量:
  WIKI_URL        Wiki git 地址
  WIKI_CACHE      本地 wiki 缓存目录（默认 /tmp/qq-maid-bot.wiki）
  QQ_GROUP_URL    交流群链接（写入首页/摘要）
  QQ_GROUP_NAME   交流群名称
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out)
      [[ $# -ge 2 ]] || { echo "[错误] --out 需要目录参数"; exit 1; }
      OUT_DIR="$2"
      shift 2
      ;;
    --sync)
      DO_SYNC=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "[错误] 未知参数: $1"
      usage
      exit 1
      ;;
  esac
done

if [[ "$DRY_RUN" -eq 1 && "$DO_SYNC" -eq 0 ]]; then
  echo "[错误] --dry-run 只能与 --sync 一起使用"
  exit 1
fi

SYNC_DATE="$(date +%F)"

echo ">>> 更新 Wiki 缓存: $WIKI_CACHE"
if [[ -d "$WIKI_CACHE/.git" ]]; then
  git -C "$WIKI_CACHE" pull --ff-only
else
  git clone "$WIKI_URL" "$WIKI_CACHE"
fi

TMP_OUT="$(mktemp -d "${TMPDIR:-/tmp}/project-docs-kb.XXXXXX")"
cleanup() {
  rm -rf "$TMP_OUT"
}
trap cleanup EXIT

python3 - "$WIKI_CACHE" "$REPO_ROOT/CHANGELOG.md" "$TMP_OUT" "$SYNC_DATE" "$QQ_GROUP_NAME" "$QQ_GROUP_URL" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

wiki_dir = Path(sys.argv[1])
changelog_path = Path(sys.argv[2])
out_dir = Path(sys.argv[3])
sync_date = sys.argv[4]
group_name = sys.argv[5]
group_url = sys.argv[6]

out_dir.mkdir(parents=True, exist_ok=True)

pages = [
    ("HOME.md", "wiki-home.md", "小女仆机器人项目 Wiki 首页"),
    ("使用说明.md", "wiki-usage.md", "小女仆机器人使用说明"),
    ("安装手册.md", "wiki-install.md", "小女仆机器人安装手册"),
    ("配置中心.md", "wiki-config-center.md", "小女仆机器人配置中心"),
    ("Napcat接入.md", "wiki-napcat.md", "用 NapCat 接入小女仆"),
    ("ops运维命令.md", "wiki-ops.md", "用 /ops 在 QQ 里做运维"),
    ("ops-codex.md", "wiki-ops-codex.md", "用 /ops codex 跑长任务"),
    ("和风天气配置.md", "wiki-qweather.md", "和风天气配置"),
    ("开发维护文档.md", "wiki-development.md", "小女仆机器人开发维护文档"),
    ("插件开发.md", "wiki-plugins.md", "自定义 Tool 插件开发"),
]

index_lines = [
    "# 小女仆机器人项目公开文档知识库",
    "",
    f"> 同步日期：{sync_date}",
    "> 来源：GitHub Wiki + CHANGELOG",
    f"> 交流群：{group_name} {group_url}",
    "",
    "本目录存放项目公开 Wiki 与变更日志，供机器人本地知识检索使用。",
    "不包含私有 prompt、真实群聊、密钥或用户数据。",
    "",
    "## 文件清单",
    "",
]

missing = []
for src_name, dst_name, title in pages:
    src = wiki_dir / src_name
    if not src.is_file():
        missing.append(src_name)
        continue
    body = src.read_text(encoding="utf-8").lstrip()
    if body.startswith("#"):
        _, _, rest = body.partition("\n")
        content = (
            f"# {title}\n\n"
            f"> 来源：项目 Wiki `{src_name}`\n"
            f"> 同步日期：{sync_date}\n\n"
            f"{rest.lstrip()}"
        )
    else:
        content = (
            f"# {title}\n\n"
            f"> 来源：项目 Wiki `{src_name}`\n"
            f"> 同步日期：{sync_date}\n\n"
            f"{body}"
        )
    (out_dir / dst_name).write_text(content, encoding="utf-8")
    index_lines.append(f"- `{dst_name}`：{title}")

if missing:
    raise SystemExit("缺少 Wiki 页面: " + ", ".join(missing))

if not changelog_path.is_file():
    raise SystemExit(f"缺少 CHANGELOG: {changelog_path}")

changelog = changelog_path.read_text(encoding="utf-8")
headers = re.findall(r"^## \[([^\]]+)\] - (\d{4}-\d{2}-\d{2})", changelog, flags=re.M)
recent = headers[:12]

summary = [
    "# 小女仆机器人 CHANGELOG",
    "",
    "> 来源：仓库 CHANGELOG.md",
    f"> 同步日期：{sync_date}",
    "",
    "## 最近版本速览",
    "",
]
for ver, day in recent:
    summary.append(f"- {ver}（{day}）")
summary.append("")
summary.append("## 完整变更记录")
summary.append("")
cl_body = changelog
if cl_body.startswith("# "):
    cl_body = "\n".join(cl_body.splitlines()[1:]).lstrip()
summary.append(cl_body)
(out_dir / "changelog.md").write_text("\n".join(summary) + "\n", encoding="utf-8")
index_lines.append("- `changelog.md`：项目变更日志（含完整历史）")

# 轻量摘要，方便最近能力检索命中
focus = []
for ver, day in headers[:5]:
    # 抓取该版本首个 Release Focus 段落首行，失败则只写版本号
    pattern = rf"## \[{re.escape(ver)}\] - {re.escape(day)}\n(?P<body>.*?)(?=\n## |\Z)"
    m = re.search(pattern, changelog, flags=re.S)
    bullet = f"- {ver}（{day}）"
    if m:
        body = m.group("body")
        focus_m = re.search(r"^\* \*\*([^*]+)\*\*", body, flags=re.M)
        if focus_m:
            bullet += f"：{focus_m.group(1).strip()}"
    focus.append(bullet)

recent_only = [
    "# 小女仆机器人最近更新摘要",
    "",
    f"> 同步日期：{sync_date}",
    f"> 交流群：{group_name} {group_url}",
    "",
    "## 最近版本",
    "",
]
recent_only.extend(focus or ["- 暂无版本记录"])
recent_only += [
    "",
    "## 文档入口",
    "",
    "- 使用说明：wiki-usage.md",
    "- 安装手册：wiki-install.md",
    "- 配置中心：wiki-config-center.md",
    "- 完整变更：changelog.md",
    "",
]
(out_dir / "recent-updates.md").write_text("\n".join(recent_only), encoding="utf-8")
index_lines.append("- `recent-updates.md`：最近版本摘要")

index_lines += [
    "",
    "## 使用说明",
    "",
    "机器人会按问题从本目录检索相关片段，不会整份注入。",
    "更新本目录后需要重启机器人以重建知识索引。",
    "",
]
(out_dir / "README.md").write_text("\n".join(index_lines) + "\n", encoding="utf-8")

total = sum(p.stat().st_size for p in out_dir.glob("*.md"))
print(f"generated {len(list(out_dir.glob('*.md')))} markdown files, {total} bytes")
for p in sorted(out_dir.glob("*.md")):
    print(f"  {p.name}")
PY

mkdir -p "$(dirname "$OUT_DIR")"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
# 只复制 md，避免带入临时杂物
find "$TMP_OUT" -maxdepth 1 -type f -name '*.md' -exec cp -a {} "$OUT_DIR"/ \;

echo ">>> 已生成: $OUT_DIR"
find "$OUT_DIR" -maxdepth 1 -type f -name '*.md' | sort | sed 's|^|  |'

if [[ "$DO_SYNC" -eq 1 ]]; then
  echo ">>> 调用 sync_knowledge.sh"
  if [[ "$DRY_RUN" -eq 1 ]]; then
    bash "$SCRIPT_DIR/sync_knowledge.sh" --dry-run
  else
    bash "$SCRIPT_DIR/sync_knowledge.sh"
  fi
  echo
  echo "提示: 远端索引通常在重启后重建。例如:"
  echo "  ssh \"\$REMOTE_HOST\" 'cd /root/qq-maid-bot && ./botctl.sh restart'"
fi

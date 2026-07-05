#!/usr/bin/env python3
"""Summarize tracked Rust test assets for the qq-maid workspace."""

from __future__ import annotations

import argparse
import re
import subprocess
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CRATES = {
    "src/": "qq-maid-bot",
    "qq-maid-common/": "qq-maid-common",
    "qq-maid-llm/": "qq-maid-llm",
    "qq-maid-core/": "qq-maid-core",
    "qq-maid-gateway-rs/": "qq-maid-gateway-rs",
}

TEST_ATTR_RE = re.compile(r"^\s*#\s*\[\s*test\s*\]")
TOKIO_TEST_RE = re.compile(r"^\s*#\s*\[\s*tokio::test")
IGNORED_RE = re.compile(r"^\s*#\s*\[\s*ignore")
CFG_TEST_RE = re.compile(r"^\s*#\s*\[\s*cfg\s*\(\s*test\s*\)")
DOCTEST_FENCE_RE = re.compile(r"^\s*///\s*```(?:rust|no_run|ignore|should_panic)?\s*$")
FIXTURE_RE = re.compile(
    r"\b(fixture|mock|fake|stub|builder|snapshot|support)\b",
    re.IGNORECASE,
)
HIGH_RISK_PATTERNS = {
    "database/storage/migration": re.compile(
        r"\b(sqlite|database|storage|migration|transaction|rusqlite)\b",
        re.IGNORECASE,
    ),
    "network/protocol": re.compile(
        r"\b(reqwest|http|websocket|sse|stream|protocol|payload|gateway)\b",
        re.IGNORECASE,
    ),
    "time/concurrency": re.compile(
        r"\b(tokio::time|sleep|spawn|timeout|concurrent|dedupe|aggregator|reminder|recurrence)\b",
        re.IGNORECASE,
    ),
    "provider/tool-loop": re.compile(
        r"\b(provider|fallback|route|tool[_ -]?loop|openai|deepseek|bigmodel|agent_loop)\b",
        re.IGNORECASE,
    ),
    "pending/session/state": re.compile(
        r"\b(pending|session|state|confirm|resume|scope|owner_key|scope_key)\b",
        re.IGNORECASE,
    ),
}


@dataclass
class FileStats:
    path: str
    crate: str
    lines: int
    test_attrs: int
    tokio_tests: int
    ignored: int
    cfg_test_attrs: int
    doctest_fences: int
    test_lines: int
    dedicated_test_file: bool
    fixture_hits: int
    risk_hits: Counter[str] = field(default_factory=Counter)


def git_tracked_files(ref: str | None) -> list[str]:
    command = ["git", "ls-files"] if ref is None else ["git", "ls-tree", "-r", "--name-only", ref]
    output = subprocess.check_output(command, cwd=ROOT, text=True)
    return [line for line in output.splitlines() if line]


def read_tracked_text(path: str, ref: str | None) -> str:
    if ref is None:
        return (ROOT / path).read_text(encoding="utf-8")
    return subprocess.check_output(
        ["git", "show", f"{ref}:{path}"],
        cwd=ROOT,
        text=True,
    )


def crate_for(path: str) -> str | None:
    for prefix, crate in CRATES.items():
        if path.startswith(prefix):
            return crate
    return None


def is_dedicated_test_file(path: str) -> bool:
    return "/tests/" in path or path.endswith("/tests.rs") or path.startswith("tests/")


def count_inline_test_lines(lines: list[str]) -> int:
    total = 0
    index = 0
    while index < len(lines):
        if not CFG_TEST_RE.search(lines[index]):
            index += 1
            continue

        start = index
        cursor = index + 1
        while cursor < len(lines) and "{" not in lines[cursor]:
            cursor += 1
        if cursor >= len(lines):
            total += 1
            index += 1
            continue

        depth = 0
        seen_open = False
        while cursor < len(lines):
            depth += lines[cursor].count("{")
            depth -= lines[cursor].count("}")
            seen_open = seen_open or "{" in lines[cursor]
            if seen_open and depth <= 0:
                total += cursor - start + 1
                break
            cursor += 1
        else:
            total += len(lines) - start
            break
        index = cursor + 1
    return total


def module_key(path: str, crate: str) -> str:
    rel = path
    prefix = f"{crate}/src/"
    if path.startswith(prefix):
        rel = path[len(prefix) :]
    elif path.startswith("src/"):
        rel = path[len("src/") :]
    parts = rel.split("/")
    if len(parts) == 1:
        return parts[0]
    if parts[0] == "runtime" and len(parts) >= 2:
        return f"runtime/{parts[1]}"
    if parts[0] == "gateway" and len(parts) >= 2:
        return f"gateway/{parts[1]}"
    if parts[0] == "provider" and len(parts) >= 2:
        return f"provider/{parts[1]}"
    if parts[0] == "storage" and len(parts) >= 2:
        return f"storage/{parts[1]}"
    return parts[0]


def analyze_file(path: str, ref: str | None) -> FileStats | None:
    crate = crate_for(path)
    if crate is None or not path.endswith(".rs"):
        return None
    text = read_tracked_text(path, ref)
    lines = text.splitlines()
    dedicated = is_dedicated_test_file(path)
    test_lines = len(lines) if dedicated else count_inline_test_lines(lines)
    stats = FileStats(
        path=path,
        crate=crate,
        lines=len(lines),
        test_attrs=sum(1 for line in lines if TEST_ATTR_RE.search(line)),
        tokio_tests=sum(1 for line in lines if TOKIO_TEST_RE.search(line)),
        ignored=sum(1 for line in lines if IGNORED_RE.search(line)),
        cfg_test_attrs=sum(1 for line in lines if CFG_TEST_RE.search(line)),
        doctest_fences=sum(1 for line in lines if DOCTEST_FENCE_RE.search(line)),
        test_lines=test_lines,
        dedicated_test_file=dedicated,
        fixture_hits=len(FIXTURE_RE.findall(text)),
    )
    for name, pattern in HIGH_RISK_PATTERNS.items():
        hits = len(pattern.findall(text))
        if hits:
            stats.risk_hits[name] = hits
    return stats


def md_doc_rust_fences(paths: list[str], ref: str | None) -> int:
    count = 0
    for path in paths:
        if not path.endswith(".md"):
            continue
        if not (
            path == "README.md"
            or path.startswith("docs/")
            or path.endswith("/README.md")
        ):
            continue
        text = read_tracked_text(path, ref)
        count += len(re.findall(r"^```(?:rust|no_run|ignore|should_panic)\s*$", text, re.M))
    return count


def print_table(headers: list[str], rows: list[list[object]]) -> None:
    print("| " + " | ".join(headers) + " |")
    print("| " + " | ".join("---" for _ in headers) + " |")
    for row in rows:
        print("| " + " | ".join(str(value) for value in row) + " |")


def main() -> None:
    parser = argparse.ArgumentParser(description="Summarize tracked Rust test assets.")
    parser.add_argument(
        "--ref",
        help="Read files from a git ref, for example HEAD. Defaults to the working tree.",
    )
    args = parser.parse_args()

    tracked = git_tracked_files(args.ref)
    stats = [item for path in tracked if (item := analyze_file(path, args.ref)) is not None]
    by_crate: dict[str, list[FileStats]] = defaultdict(list)
    for item in stats:
        by_crate[item.crate].append(item)

    crate_rows = []
    for crate in ["qq-maid-common", "qq-maid-llm", "qq-maid-core", "qq-maid-gateway-rs", "qq-maid-bot"]:
        items = by_crate.get(crate, [])
        crate_rows.append(
            [
                crate,
                len(items),
                sum(item.lines for item in items),
                sum(item.test_attrs for item in items),
                sum(item.tokio_tests for item in items),
                sum(item.cfg_test_attrs for item in items),
                sum(item.test_lines for item in items),
                sum(1 for item in items if item.dedicated_test_file),
                sum(item.ignored for item in items),
                sum(item.doctest_fences for item in items),
            ]
        )

    module_counter: dict[tuple[str, str], Counter[str]] = defaultdict(Counter)
    for item in stats:
        key = (item.crate, module_key(item.path, item.crate))
        module_counter[key]["tests"] += item.test_attrs + item.tokio_tests
        module_counter[key]["test_lines"] += item.test_lines
        module_counter[key]["files"] += 1
        module_counter[key]["fixture_hits"] += item.fixture_hits
        for risk, hits in item.risk_hits.items():
            module_counter[key][risk] += hits

    hot_modules = sorted(
        module_counter.items(),
        key=lambda entry: (entry[1]["tests"], entry[1]["test_lines"]),
        reverse=True,
    )[:18]
    hot_files = sorted(
        stats,
        key=lambda item: (item.test_attrs + item.tokio_tests, item.test_lines),
        reverse=True,
    )[:18]
    fixture_files = sorted(stats, key=lambda item: item.fixture_hits, reverse=True)[:12]
    risk_rows = []
    for item in stats:
        if item.risk_hits:
            risk_rows.append(
                [
                    item.path,
                    item.test_attrs + item.tokio_tests,
                    item.test_lines,
                    ", ".join(item.risk_hits.keys()),
                ]
            )
    risk_rows.sort(key=lambda row: (row[1], row[2]), reverse=True)

    source = f"git ref `{args.ref}`" if args.ref else "当前工作区"
    print("# Rust 测试资产统计")
    print()
    print(f"统计范围：{source} 的 Git 已跟踪 Rust 文件；不读取运行时私有配置、日志、SQLite 或本地知识资料。")
    print()
    print("## 分 crate 概览")
    print_table(
        [
            "crate",
            "Rust 文件",
            "Rust 行",
            "#[test]",
            "#[tokio::test]",
            "#[cfg(test)]",
            "估算测试行",
            "独立测试文件",
            "ignored",
            "doc comment doctest",
        ],
        crate_rows,
    )
    print()
    print(f"Markdown 文档 Rust 代码块：{md_doc_rust_fences(tracked, args.ref)}")
    print()
    print("## 测试热点模块")
    print_table(
        ["crate", "模块", "测试属性", "估算测试行", "文件", "fixture/mock 命中"],
        [
            [
                crate,
                module,
                values["tests"],
                values["test_lines"],
                values["files"],
                values["fixture_hits"],
            ]
            for (crate, module), values in hot_modules
        ],
    )
    print()
    print("## 测试热点文件")
    print_table(
        ["文件", "测试属性", "估算测试行", "ignored", "fixture/mock 命中"],
        [
            [
                item.path,
                item.test_attrs + item.tokio_tests,
                item.test_lines,
                item.ignored,
                item.fixture_hits,
            ]
            for item in hot_files
        ],
    )
    print()
    print("## fixture / mock / helper 热点")
    print_table(
        ["文件", "命中数", "测试属性", "估算测试行"],
        [
            [
                item.path,
                item.fixture_hits,
                item.test_attrs + item.tokio_tests,
                item.test_lines,
            ]
            for item in fixture_files
            if item.fixture_hits > 0
        ],
    )
    print()
    print("## 高风险类型命中 Top 20")
    print_table(
        ["文件", "测试属性", "估算测试行", "类型"],
        risk_rows[:20],
    )


if __name__ == "__main__":
    main()

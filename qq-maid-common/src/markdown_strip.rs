//! Markdown 剥离工具。
//!
//! 将 LLM 回复中的 Markdown 修饰（标题、列表、链接、代码、加粗等）剥除，
//! 保留纯文字用于 QQ 纯文本通道。该模块是纯文本处理，不依赖业务状态，
//! 最初位于 `qq-maid-core` 的 `runtime/respond/markdown_strip.rs`，
//! 因 Gateway 普通消息分段（Issue #124）需要按段为同一原文生成纯文本 fallback
//! 也复用同一套 strip 语义，故迁移到 `qq-maid-common` 共享，避免两套实现漂移。
//!
//! 行为约束：
//! - 围栏代码块（``` ```）内容原样保留，不剥除其中的 Markdown 符号；
//! - 表格展平为"单元格1 / 单元格2"格式，移除分隔行；
//! - 链接保留标签文字，URL 以全角括号附在后面；
//! - 图片使用 alt 文本，去掉 `!` 标记；
//! - 转义符号 `\\*` `\\_` 还原为字面量；
//! - `<br>`、`</p>` 等 HTML 标签转换为换行后移除其余标签。

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use regex::Regex;

/// 完整解析 Markdown 后渲染为纯文本，适合不稳定支持 Markdown 的平台通道。
///
/// 与历史 [`strip_markdown_for_chat`] 保持独立，避免改变普通聊天 fallback 的既有
/// 展示语义。该函数会解析引用式链接和合法反斜杠转义；链接目标用全角括号保留，
/// 引用定义本身不会作为正文输出。
pub fn render_markdown_as_plain_text(markdown: &str) -> String {
    // 不启用 smart punctuation，避免普通 RSS 文本里的半角引号等字符被擅自改写。
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(markdown, options);
    let mut output = String::new();
    let mut link_destinations = Vec::new();

    for event in parser {
        match event {
            Event::Start(Tag::Paragraph | Tag::Heading { .. }) => {
                ensure_paragraph_break(&mut output)
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) => push_paragraph_break(&mut output),
            Event::Start(Tag::Item) => {
                ensure_line_break(&mut output);
                output.push_str("• ");
            }
            Event::End(TagEnd::Item) => ensure_line_break(&mut output),
            Event::Start(Tag::Link { dest_url, .. }) => {
                link_destinations.push(dest_url.into_string());
            }
            Event::End(TagEnd::Link) => {
                if let Some(destination) = link_destinations.pop()
                    && !destination.trim().is_empty()
                {
                    output.push('（');
                    output.push_str(destination.trim());
                    output.push('）');
                }
            }
            Event::Text(text) | Event::Code(text) => output.push_str(&text),
            Event::SoftBreak | Event::HardBreak => ensure_line_break(&mut output),
            Event::Rule => push_paragraph_break(&mut output),
            Event::TaskListMarker(checked) => {
                output.push_str(if checked {
                    "[已完成] "
                } else {
                    "[未完成] "
                });
            }
            Event::Html(_) | Event::InlineHtml(_) | Event::FootnoteReference(_) => {}
            Event::Start(_) | Event::End(_) | Event::InlineMath(_) | Event::DisplayMath(_) => {}
        }
    }

    output.trim().to_owned()
}

fn ensure_line_break(output: &mut String) {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
}

fn ensure_paragraph_break(output: &mut String) {
    if !output.is_empty() && !output.ends_with("\n\n") {
        ensure_line_break(output);
        output.push('\n');
    }
}

fn push_paragraph_break(output: &mut String) {
    if !output.ends_with("\n\n") {
        ensure_line_break(output);
        output.push('\n');
    }
}

/// 从文本中剥除 Markdown 修饰（标题、列表、链接、代码、加粗等），保留纯文字。
pub fn strip_markdown_for_chat(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut rows = Vec::new();
    let mut in_fence = false;

    for line in normalized.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }

        if in_fence {
            rows.push(line.to_owned());
            continue;
        }

        rows.push(strip_markdown_line(line));
    }

    let mut text = flatten_markdown_tables(&rows.join("\n"));
    text = Regex::new(r"(?i)<br\s*/?>")
        .unwrap()
        .replace_all(&text, "\n")
        .to_string();
    text = Regex::new(r"(?i)</p\s*>")
        .unwrap()
        .replace_all(&text, "\n\n")
        .to_string();
    text = Regex::new(r"(?i)<[^>]+>")
        .unwrap()
        .replace_all(&text, "")
        .to_string();
    text = Regex::new(r"\n{3,}")
        .unwrap()
        .replace_all(&text, "\n\n")
        .to_string();
    text.trim().to_owned()
}

/// 将 Markdown 表格展平为"单元格1 / 单元格2"格式，同时移除分隔行。
fn flatten_markdown_tables(text: &str) -> String {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with('|') && trimmed.ends_with('|') {
                let cells = trimmed
                    .trim_matches('|')
                    .split('|')
                    .map(str::trim)
                    .filter(|cell| !cell.is_empty())
                    .collect::<Vec<_>>();
                if cells.iter().all(|cell| {
                    cell.chars()
                        .all(|ch| ch == '-' || ch == ':' || ch.is_whitespace())
                }) {
                    return None;
                }
                return Some(cells.join(" / "));
            }
            Some(line.to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_markdown_line(line: &str) -> String {
    let trimmed = line.trim_start();
    if trimmed.starts_with('|') && trimmed.ends_with('|') {
        return strip_inline_markdown(line);
    }

    let indent = line.len() - trimmed.len();
    let mut prefix = String::new();
    let mut content = trimmed;

    if let Some(rest) = content.strip_prefix('>') {
        content = rest.trim_start();
    }

    if let Some(rest) = strip_heading_prefix(content) {
        content = rest;
    } else if let Some(rest) = strip_unordered_list_prefix(content) {
        prefix = format!("{}· ", " ".repeat(indent));
        content = rest;
    } else if let Some(rest) = strip_ordered_list_prefix(content) {
        prefix = format!("{}· ", " ".repeat(indent));
        content = rest;
    } else if indent > 0 {
        prefix = " ".repeat(indent);
    }

    let content = strip_inline_markdown(content);
    format!("{prefix}{content}")
}

fn strip_heading_prefix(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|&ch| ch == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = line.get(hashes..)?;
    rest.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then_some(rest.trim_start())
}

fn strip_unordered_list_prefix(line: &str) -> Option<&str> {
    let mut chars = line.chars();
    match chars.next()? {
        '-' | '*' | '+' => {}
        _ => return None,
    }
    let rest = chars.as_str();
    rest.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then_some(rest.trim_start())
}

fn strip_ordered_list_prefix(line: &str) -> Option<&str> {
    let digits = line.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = line.get(digits..)?;
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    rest.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then_some(rest.trim_start())
}

fn strip_inline_markdown(text: &str) -> String {
    let mut rendered = String::new();
    let mut protected = Vec::new();
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];

        if ch == '\\'
            && let Some(next) = chars.get(index + 1)
        {
            rendered.push_str(&protect_inline_literal(&mut protected, &next.to_string()));
            index += 2;
            continue;
        }

        if ch == '`' {
            let tick_count = count_run(&chars, index, '`');
            if let Some(end) = find_backtick_run(&chars, index + tick_count, tick_count) {
                rendered.extend(chars[index + tick_count..end].iter());
                index = end + tick_count;
                continue;
            }
        }

        if ch == '!'
            && chars.get(index + 1) == Some(&'[')
            && let Some((alt, url, next)) = parse_markdown_link(&chars, index + 1)
        {
            if !alt.trim().is_empty() {
                rendered.push_str(alt.trim());
                if !url.trim().is_empty() {
                    rendered.push('（');
                    rendered.push_str(&protect_inline_literal(&mut protected, url.trim()));
                    rendered.push('）');
                }
            } else {
                rendered.push_str(&protect_inline_literal(&mut protected, url.trim()));
            }
            index = next;
            continue;
        }

        if ch == '['
            && let Some((label, url, next)) = parse_markdown_link(&chars, index)
        {
            rendered.push_str(label.trim());
            if !url.trim().is_empty() {
                rendered.push('（');
                rendered.push_str(&protect_inline_literal(&mut protected, url.trim()));
                rendered.push('）');
            }
            index = next;
            continue;
        }

        rendered.push(ch);
        index += 1;
    }

    restore_inline_literals(strip_emphasis_markers(&rendered), &protected)
}

fn count_run(chars: &[char], start: usize, marker: char) -> usize {
    let mut count = 0;
    while chars.get(start + count) == Some(&marker) {
        count += 1;
    }
    count
}

fn find_backtick_run(chars: &[char], mut index: usize, tick_count: usize) -> Option<usize> {
    while index < chars.len() {
        if chars[index] == '`' && count_run(chars, index, '`') == tick_count {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn parse_markdown_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    if chars.get(start) != Some(&'[') {
        return None;
    }
    let label_end = find_closing_bracket(chars, start)?;
    let url_start = label_end + 1;
    if chars.get(url_start) != Some(&'(') {
        return None;
    }
    let url_end = find_closing_paren(chars, url_start)?;
    let label = chars[start + 1..label_end].iter().collect::<String>();
    let mut url = chars[url_start + 1..url_end].iter().collect::<String>();
    if let Some(stripped) = url
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
    {
        url = stripped.to_owned();
    }
    let next = url_end + 1;
    Some((label, url, next))
}

fn find_closing_bracket(chars: &[char], start: usize) -> Option<usize> {
    let mut index = start + 1;
    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            ']' => return Some(index),
            _ => index += 1,
        }
    }
    None
}

fn find_closing_paren(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 0;
    let mut index = start;
    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            '(' => {
                depth += 1;
                index += 1;
            }
            ')' => {
                depth -= 1;
                index += 1;
                if depth == 0 {
                    return Some(index - 1);
                }
            }
            _ => index += 1,
        }
    }
    None
}

fn strip_emphasis_markers(text: &str) -> String {
    let replacements = [
        (r"\*\*([^*\n]+)\*\*", "$1"),
        (r"__([^_\n]+)__", "$1"),
        (r"\*([^*\n]+)\*", "$1"),
        (r"_([^_\n]+)_", "$1"),
        (r"~~([^~\n]+)~~", "$1"),
    ];
    replacements
        .into_iter()
        .fold(text.to_owned(), |value, (pattern, replacement)| {
            Regex::new(pattern)
                .unwrap()
                .replace_all(&value, replacement)
                .to_string()
        })
}

fn protect_inline_literal(protected: &mut Vec<String>, value: &str) -> String {
    let token = format!("@@MD{}@@", protected.len());
    protected.push(value.to_owned());
    token
}

fn restore_inline_literals(mut text: String, protected: &[String]) -> String {
    for (index, value) in protected.iter().enumerate() {
        let token = format!("@@MD{index}@@");
        text = text.replace(&token, value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_renderer_resolves_reference_links_and_omits_definitions() {
        let markdown = "## What's Changed\n\n* by [@maid][1] in [#414][2]\n\n[1]: https://example.test/maid\n[2]: https://example.test/pull/414";

        let text = render_markdown_as_plain_text(markdown);

        assert!(text.contains("What's Changed"));
        assert!(text.contains("• by @maid（https://example.test/maid）"));
        assert!(text.contains("#414（https://example.test/pull/414）"));
        assert!(!text.contains("[1]:"));
        assert!(!text.contains("[2]:"));
    }

    #[test]
    fn plain_renderer_parses_escapes_but_preserves_code_literals() {
        let markdown = r"\#\# title \[codex\] qq\-maid\-bot `path\to\file`";

        let text = render_markdown_as_plain_text(markdown);

        assert_eq!(text, r"## title [codex] qq-maid-bot path\to\file");
    }
}

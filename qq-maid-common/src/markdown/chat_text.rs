//! 普通聊天和朗读场景的 Markdown 文本转换。

use regex::Regex;

/// 从文本中剥除 Markdown 修饰（标题、列表、链接、代码、加粗等），保留纯文字。
pub fn to_chat_text(text: &str) -> String {
    render_markdown_text(text, RenderMode::ChatFallback)
}

/// 从原始 Markdown 生成适合朗读的文字。
///
/// 与普通聊天 fallback 共用同一套 Markdown 行内、列表和表格解析；朗读模式仅在
/// 围栏代码、链接目标和图片等不宜发声的内容上采用更严格的丢弃规则。
pub(crate) fn to_speakable_text(text: &str) -> String {
    render_markdown_text(text, RenderMode::Speakable)
}

/// 没有 Markdown 时只做朗读所需的最小纯文本整理。
pub(crate) fn normalize_speakable_plain_text(text: &str) -> String {
    normalize_speakable_whitespace(remove_raw_urls(text))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    ChatFallback,
    Speakable,
}

fn render_markdown_text(text: &str, mode: RenderMode) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut rows = Vec::new();
    let mut fence_marker = None;

    for line in normalized.lines() {
        let trimmed = line.trim_start();
        if let Some(marker) = markdown_fence_marker(trimmed) {
            match fence_marker {
                None => fence_marker = Some(marker),
                Some(open_marker) if open_marker == marker => fence_marker = None,
                Some(_) => {}
            }
            continue;
        }

        if fence_marker.is_some() {
            if mode == RenderMode::ChatFallback {
                rows.push(line.to_owned());
            }
            continue;
        }

        rows.push(strip_markdown_line(line, mode));
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
    if mode == RenderMode::Speakable {
        normalize_speakable_whitespace(remove_raw_urls(&text))
    } else {
        text.trim().to_owned()
    }
}

fn markdown_fence_marker(line: &str) -> Option<char> {
    let marker = line.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    (line.chars().take_while(|ch| *ch == marker).count() >= 3).then_some(marker)
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

fn strip_markdown_line(line: &str, mode: RenderMode) -> String {
    let trimmed = line.trim_start();
    if trimmed.starts_with('|') && trimmed.ends_with('|') {
        return strip_inline_markdown(line, mode);
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

    let content = strip_inline_markdown(content, mode);
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

fn strip_inline_markdown(text: &str, mode: RenderMode) -> String {
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
            if mode == RenderMode::ChatFallback {
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
            }
            index = next;
            continue;
        }

        if ch == '['
            && let Some((label, url, next)) = parse_markdown_link(&chars, index)
        {
            rendered.push_str(label.trim());
            if mode == RenderMode::ChatFallback && !url.trim().is_empty() {
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

fn remove_raw_urls(text: &str) -> String {
    // URL 的协议和结构字符都是 ASCII；限制匹配字符集，避免 URL 后没有空格时把
    // 紧随其后的中文正文一起吞掉。
    Regex::new(r#"(?i)(?:https?://|www\.)[a-z0-9._~:/?#@!$&'()*+,;=%\[\]-]+"#)
        .unwrap()
        .replace_all(text, "")
        .to_string()
}

fn normalize_speakable_whitespace(text: String) -> String {
    let text = Regex::new(r"[ \t]+")
        .unwrap()
        .replace_all(&text, " ")
        .to_string();
    let text = Regex::new(r" *\n *")
        .unwrap()
        .replace_all(&text, "\n")
        .to_string();
    let text = Regex::new(r" +([，。！？；：,.!?;:])")
        .unwrap()
        .replace_all(&text, "$1")
        .to_string();
    Regex::new(r"\n{3,}")
        .unwrap()
        .replace_all(&text, "\n\n")
        .trim()
        .to_owned()
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

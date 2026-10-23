//! 共享文本截断工具。
//!
//! 现有调用点存在展示文本、RSS 摘要和 Todo 持久化三种边界语义，统一放在这里维护，
//! 但不强行合并为同一种输出，避免改变用户可见文本或已保存数据。

/// 清理用户可见文本中的控制字符和不可见格式字符。
///
/// 控制字符可能破坏单条回复的边界；双向控制符、零宽字符和变体选择符等不可见
/// 格式字符则可能重排或隐藏后续展示内容。普通空白和可见 Unicode 字符保持不变，
/// 由调用方按自己的展示语义处理。
pub fn sanitize_visible_text(text: &str) -> String {
    text.chars()
        .filter(|&character| !character.is_control() && !is_invisible_format(character))
        .collect()
}

/// 清理用户可见文本并折叠为单行。
///
/// 换行和制表符先替换为空格，避免去除控制字符后把相邻单词意外粘连；随后移除
/// 其余控制字符与不可见格式字符，并把连续空白折叠为一个普通空格。
pub fn sanitize_single_line_visible_text(text: &str) -> String {
    let single_line = text.replace(['\r', '\n', '\t'], " ");
    sanitize_visible_text(&single_line)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 判断字符是否属于会影响文本展示、但自身不可见的 Unicode 格式字符。
pub fn is_invisible_format(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
    )
}

/// 将字符串截断到指定字符数，超出时末尾追加"…"，并沿用 respond 展示层的 trim 语义。
pub fn truncate_chars_with_ellipsis_trimmed(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.trim().to_owned();
    }
    let keep = limit.saturating_sub(1);
    format!(
        "{}…",
        text.chars().take(keep).collect::<String>().trim_end()
    )
}

/// 将字符串截断到指定字符数，超出时末尾追加"…"，不额外清理首尾空白。
pub fn truncate_chars_with_ellipsis(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let keep = limit.saturating_sub(1);
    format!("{}…", text.chars().take(keep).collect::<String>())
}

/// 将字符串截断到指定字符数并清理首尾空白，不追加省略号。
pub fn truncate_chars_trimmed(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.trim().to_owned();
    }
    text.chars()
        .take(limit)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// 在不超过 `limit` 字符的前提下返回正文前缀，且不切断 Markdown 行内链接。
///
/// 省略号由调用方追加，因此最多保留 `limit - 1` 个字符。截断点落在
/// `[标签](URL)` 内部时回退到整条链接（或整条列表项）之前：残缺链接既不可点击
/// 也会破坏 Markdown 结构，展示层宁可少展示一条。返回值为（正文前缀，从原文
/// 保留的字符数），后者供来源展示标记判断对应片段是否仍然可见。
pub fn truncate_markdown_prefix(text: &str, limit: usize) -> (String, usize) {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= limit {
        return (text.trim().to_owned(), chars.len());
    }

    let mut kept = limit.saturating_sub(1).min(chars.len());
    for span in markdown_link_spans(&chars) {
        // 截断点正好落在链接起点时同样回退：链接一行都没保留，不应留下悬空的 `- `。
        if span.start <= kept && kept < span.end {
            kept = markdown_safe_rewind(&chars, span.start);
        }
    }
    (
        chars[..kept]
            .iter()
            .collect::<String>()
            .trim_end()
            .to_owned(),
        kept,
    )
}

#[derive(Debug, Clone, Copy)]
struct MarkdownLinkSpan {
    start: usize,
    end: usize,
}

/// 扫描完整的 Markdown 行内链接 `[标签](URL)` 的字符区间。标签内的转义方括号、
/// 嵌套方括号和 URL 中的成对圆括号按层级匹配，避免把普通文本误判成链接。
fn markdown_link_spans(chars: &[char]) -> Vec<MarkdownLinkSpan> {
    let mut spans = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] == '['
            && !is_escaped(chars, index)
            && let Some(end) = markdown_link_end(chars, index)
        {
            spans.push(MarkdownLinkSpan { start: index, end });
            index = end;
            continue;
        }
        index += 1;
    }
    spans
}

/// 返回从 `open` 处 `[` 开始、以 `)` 结束的完整链接区间的结束位置（不含）。
fn markdown_link_end(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut cursor = open;
    let label_end = loop {
        let ch = *chars.get(cursor)?;
        if !is_escaped(chars, cursor) {
            if ch == '[' {
                depth += 1;
            } else if ch == ']' {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    break cursor;
                }
            }
        }
        cursor += 1;
    };
    if chars.get(label_end + 1) != Some(&'(') {
        return None;
    }

    let mut depth = 0usize;
    let mut cursor = label_end + 1;
    loop {
        let ch = *chars.get(cursor)?;
        if !is_escaped(chars, cursor) {
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(cursor + 1);
                }
            }
        }
        cursor += 1;
    }
}

fn is_escaped(chars: &[char], index: usize) -> bool {
    let mut backslashes = 0usize;
    let mut cursor = index;
    while cursor > 0 && chars[cursor - 1] == '\\' {
        backslashes += 1;
        cursor -= 1;
    }
    backslashes % 2 == 1
}

/// 链接被整体省略时连带省略同一行的列表标记，避免输出悬空的 `- `。
fn markdown_safe_rewind(chars: &[char], link_start: usize) -> usize {
    let line_start = chars[..link_start]
        .iter()
        .rposition(|&ch| ch == '\n')
        .map_or(0, |index| index + 1);
    let is_list_marker = chars[line_start..link_start]
        .iter()
        .all(|ch| matches!(ch, '-' | '*' | '+' | '.' | ')' | ' ' | '\t') || ch.is_ascii_digit());
    if is_list_marker {
        line_start
    } else {
        link_start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_visible_text_removes_controls_and_invisible_formats() {
        let text = "前\u{0000}中\u{200b}\u{202e}\u{2066}后\u{fe0f}";

        assert_eq!(sanitize_visible_text(text), "前中后");
        assert!(is_invisible_format('\u{200b}'));
        assert!(is_invisible_format('\u{202e}'));
        assert!(is_invisible_format('\u{2066}'));
        assert!(is_invisible_format('\u{fe0f}'));
    }

    #[test]
    fn sanitize_single_line_visible_text_folds_whitespace_without_joining_words() {
        let text = " 前\n中\t后\u{0000}段\u{200b}  末 ";

        assert_eq!(sanitize_single_line_visible_text(text), "前 中 后段 末");
    }

    #[test]
    fn ellipsis_trimmed_keeps_short_and_exact_limit_text() {
        assert_eq!(truncate_chars_with_ellipsis_trimmed("短文本", 10), "短文本");
        assert_eq!(truncate_chars_with_ellipsis_trimmed("abcd", 4), "abcd");
        assert_eq!(truncate_chars_with_ellipsis_trimmed("  ab  ", 6), "ab");
    }

    #[test]
    fn ellipsis_trimmed_truncates_unicode_text() {
        assert_eq!(
            truncate_chars_with_ellipsis_trimmed("中文天气预警说明", 6),
            "中文天气预…"
        );
        assert_eq!(
            truncate_chars_with_ellipsis_trimmed("你好世界🙂再见", 6),
            "你好世界🙂…"
        );
    }

    #[test]
    fn ellipsis_trimmed_handles_empty_and_zero_limit() {
        assert_eq!(truncate_chars_with_ellipsis_trimmed("", 6), "");
        assert_eq!(truncate_chars_with_ellipsis_trimmed("abc", 0), "…");
    }

    #[test]
    fn ellipsis_without_trim_preserves_rss_boundary_semantics() {
        assert_eq!(truncate_chars_with_ellipsis("  ab  ", 6), "  ab  ");
        assert_eq!(truncate_chars_with_ellipsis("  abcd  ", 6), "  abc…");
        assert_eq!(truncate_chars_with_ellipsis("abc", 0), "…");
    }

    #[test]
    fn markdown_prefix_keeps_complete_inline_links() {
        let first_line = "摘要：\n- [来源 \\[一\\]](https://example.test/a_(b))";
        let body = format!("{first_line}\n- [来源二](https://example.test/c)");

        // 正文恰好等于预算时不做裁剪。
        assert_eq!(
            truncate_markdown_prefix(first_line, first_line.chars().count()),
            (first_line.to_owned(), first_line.chars().count())
        );

        // 截断点落在第二条链接内部：整行省略，不会留下半截 `[标签](URL)`。
        let (prefix, kept) = truncate_markdown_prefix(&body, 48);
        assert_eq!(prefix, first_line);
        assert_eq!(kept, 45);
    }

    #[test]
    fn markdown_prefix_returns_prefix_when_links_are_cut() {
        let body = format!(
            "摘要部分\n\n参考来源：\n- [来源](https://example.test/{})",
            "a".repeat(100)
        );
        let (prefix, kept) = truncate_markdown_prefix(&body, 60);

        assert_eq!(prefix, "摘要部分\n\n参考来源：");
        assert_eq!(kept, 12);
    }

    #[test]
    fn trimmed_without_ellipsis_preserves_todo_storage_semantics() {
        assert_eq!(truncate_chars_trimmed("短文本", 10), "短文本");
        assert_eq!(truncate_chars_trimmed("abcd", 4), "abcd");
        assert_eq!(
            truncate_chars_trimmed("中文天气预警说明", 6),
            "中文天气预警"
        );
        assert_eq!(truncate_chars_trimmed("你好世界🙂再见", 6), "你好世界🙂再");
        assert_eq!(truncate_chars_trimmed("", 6), "");
        assert_eq!(truncate_chars_trimmed("abc", 0), "");
    }
}

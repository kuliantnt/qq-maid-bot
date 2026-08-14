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

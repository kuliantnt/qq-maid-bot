//! 共享文本截断工具。
//!
//! 现有调用点存在展示文本、RSS 摘要和 Todo 持久化三种边界语义，统一放在这里维护，
//! 但不强行合并为同一种输出，避免改变用户可见文本或已保存数据。

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

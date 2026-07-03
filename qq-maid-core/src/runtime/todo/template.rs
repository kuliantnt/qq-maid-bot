//! Todo 推送展示模板。
//!
//! 后台提醒没有 `respond` 会话上下文，但仍应复用 Todo 自己的双通道展示规则：
//! 业务层生成 Markdown 与纯文本 fallback，通知层只负责按统一推送 payload 投递。

use qq_maid_common::text::truncate_chars_with_ellipsis_trimmed as truncate_chars;

use crate::{
    runtime::todo::{TodoItem, display_todo_time},
    util::time_context::format_todo_time_for_display,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoPushBody {
    pub text: String,
    pub markdown: String,
}

/// 单次提醒使用“闹钟”样式，标题、备注等用户输入统一转义后再进入 Markdown。
pub fn format_todo_single_reminder_push(item: &TodoItem) -> TodoPushBody {
    let reminder_label = item
        .reminder_at
        .as_deref()
        .map(format_todo_time_for_display)
        .unwrap_or_else(|| "现在".to_owned());
    let due_label = display_todo_time(item);
    let title = truncate_chars(item.title.trim(), 80);
    let mut text_lines = vec![
        "⏰ 待办提醒".to_owned(),
        format!("· {title}"),
        format!("提醒时间：{reminder_label}"),
    ];
    let mut markdown_lines = vec![
        "# ⏰ 待办提醒".to_owned(),
        String::new(),
        format!("- {}", escape_markdown_text(&title)),
        format!(
            "- **提醒时间**：{}",
            escape_markdown_inline(&reminder_label)
        ),
    ];
    if !due_label.trim().is_empty() {
        text_lines.push(format!("时间：{due_label}"));
        markdown_lines.push(format!(
            "- **时间**：{}",
            escape_markdown_inline(&due_label)
        ));
    }
    if let Some(detail) = item
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let detail = truncate_chars(detail, 120);
        text_lines.push(format!("备注：{detail}"));
        markdown_lines.push(format!("- **备注**：{}", escape_markdown_text(&detail)));
    }
    TodoPushBody {
        text: text_lines.join("\n"),
        markdown: markdown_lines.join("\n"),
    }
}

fn escape_markdown_inline(text: &str) -> String {
    let mut escaped = String::new();
    for ch in text.trim().replace(['\r', '\n'], " ").chars() {
        if matches!(
            ch,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '!'
                | '|'
                | '>'
        ) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn escape_markdown_text(text: &str) -> String {
    text.lines()
        .map(escape_markdown_inline)
        .collect::<Vec<_>>()
        .join("  \n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::todo::{TodoStatus, TodoTimePrecision};

    fn reminder_item() -> TodoItem {
        TodoItem {
            id: "1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "private:u1".to_owned(),
            title: "检查 *日志*".to_owned(),
            detail: Some("确认 [推送] 没失败".to_owned()),
            raw_text: None,
            due_date: None,
            due_at: Some("2099-01-01 10:00:00".to_owned()),
            reminder_at: Some("2099-01-01 09:30:00".to_owned()),
            time_precision: TodoTimePrecision::DateTime,
            status: TodoStatus::Pending,
            created_at: "2026-07-03T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-03T09:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
        }
    }

    #[test]
    fn single_reminder_push_uses_alarm_style_and_escapes_markdown() {
        let body = format_todo_single_reminder_push(&reminder_item());

        assert!(body.text.starts_with("⏰ 待办提醒"));
        assert!(body.markdown.starts_with("# ⏰ 待办提醒"));
        assert!(body.text.contains("检查 *日志*"));
        assert!(body.markdown.contains("检查 \\*日志\\*"));
        assert!(body.markdown.contains("确认 \\[推送\\] 没失败"));
    }
}

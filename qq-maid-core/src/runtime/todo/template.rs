//! Todo 展示模板。
//!
//! 提醒推送、Tool 回执和详情卡片都需要展示同一批核心字段：
//! 标题、时间、提醒、重复规则、状态以及详情。这里统一维护字段顺序和
//! Markdown / 纯文本双通道渲染，避免不同场景各自拼装后逐步漂移。

use qq_maid_common::text::truncate_chars_with_ellipsis_trimmed as truncate_chars;

use crate::{
    runtime::todo::{
        TodoItem, TodoRecurrenceKind, TodoStatus, preview_next_reminder_at, recurrence_label,
    },
    util::time_context::format_todo_time_chip_for_display,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoPushBody {
    pub text: String,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoRenderItem {
    pub title: String,
    pub detail: Option<String>,
    pub due_date: Option<String>,
    pub due_at: Option<String>,
    pub reminder_at: Option<String>,
    pub recurrence_kind: TodoRecurrenceKind,
    pub recurrence_interval_days: u32,
    pub status: Option<String>,
    pub next_reminder_at: Option<String>,
    pub completed_at: Option<String>,
    pub cancelled_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderFieldMode {
    Current,
    Next,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TodoCardOptions {
    pub reminder_mode: ReminderFieldMode,
    pub show_next_reminder: bool,
}

impl Default for TodoCardOptions {
    fn default() -> Self {
        Self {
            reminder_mode: ReminderFieldMode::Current,
            show_next_reminder: false,
        }
    }
}

impl TodoRenderItem {
    pub fn from_todo(item: &TodoItem) -> Self {
        Self {
            title: item.title.clone(),
            detail: item.detail.clone(),
            due_date: item.due_date.clone(),
            due_at: item.due_at.clone(),
            reminder_at: item.reminder_at.clone(),
            recurrence_kind: item.recurrence_kind.clone(),
            recurrence_interval_days: item.recurrence_interval_days,
            status: Some(status_machine_str(&item.status).to_owned()),
            next_reminder_at: preview_next_reminder_at(item).ok().flatten(),
            completed_at: item.completed_at.clone(),
            cancelled_at: item.cancelled_at.clone(),
        }
    }
}

pub fn format_todo_cards(
    header: &str,
    items: &[TodoRenderItem],
    options: TodoCardOptions,
) -> TodoPushBody {
    let mut text_lines = vec![header.to_owned()];
    let mut markdown_lines = vec![format!("# {}", escape_markdown_inline(header))];
    for (index, item) in items.iter().enumerate() {
        text_lines.push(String::new());
        markdown_lines.push(String::new());
        let numbered = items.len() > 1;
        append_todo_card_lines(&mut text_lines, item, false, index, numbered, options);
        append_todo_card_lines(&mut markdown_lines, item, true, index, numbered, options);
    }
    TodoPushBody {
        text: text_lines.join("\n"),
        markdown: markdown_lines.join("\n"),
    }
}

pub fn format_todo_single_reminder_push(item: &TodoItem) -> TodoPushBody {
    let render_item = TodoRenderItem::from_todo(item);
    format_todo_cards(
        "⏰ 待办提醒",
        &[render_item],
        TodoCardOptions {
            reminder_mode: ReminderFieldMode::Current,
            show_next_reminder: true,
        },
    )
}

fn append_todo_card_lines(
    lines: &mut Vec<String>,
    item: &TodoRenderItem,
    markdown: bool,
    index: usize,
    numbered: bool,
    options: TodoCardOptions,
) {
    let title = truncate_chars(item.title.trim(), 80);
    if markdown {
        let mut title_line = if numbered {
            format!("{}. {}", index + 1, escape_markdown_inline(&title))
        } else {
            escape_markdown_inline(&title)
        };
        if let Some(time) = detail_card_due_chip(item) {
            title_line.push_str(&format!(" · **时间**：{}", escape_markdown_inline(&time)));
        }
        lines.push(title_line);
    } else {
        let mut title_line = if numbered {
            format!("{}. {}", index + 1, title)
        } else {
            title
        };
        if let Some(time) = detail_card_due_chip(item) {
            title_line.push_str(&format!(" · 时间：{time}"));
        }
        lines.push(title_line);
    }
    if let Some(status) = item.status.as_deref().and_then(todo_status_display_label) {
        lines.push(field_line("状态", status, markdown));
    }
    if let Some(reminder) = reminder_field_value(item, options.reminder_mode) {
        let label = match options.reminder_mode {
            ReminderFieldMode::Current => "提醒",
            ReminderFieldMode::Next => "下一次提醒",
        };
        lines.push(field_line(label, &reminder, markdown));
    }
    if let Some(recurrence) = recurrence_label(&item.recurrence_kind, item.recurrence_interval_days)
    {
        lines.push(field_line("重复", &recurrence, markdown));
    }
    if options.show_next_reminder
        && let Some(next) = item
            .next_reminder_at
            .as_deref()
            .filter(|value| Some(*value) != item.reminder_at.as_deref())
            .map(format_todo_time_chip_for_display)
    {
        lines.push(field_line("下一次提醒", &next, markdown));
    }
    if let Some(detail) = item
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let detail = truncate_chars(detail, 120);
        lines.push(if markdown {
            format!("详情：\n{}", escape_markdown_text(&detail))
        } else {
            format!("详情：\n{detail}")
        });
    }
    if item.status.as_deref() == Some("completed")
        && let Some(completed_at) = item.completed_at.as_deref()
    {
        lines.push(field_line(
            "完成时间",
            &format_todo_time_chip_for_display(completed_at),
            markdown,
        ));
    }
    if item.status.as_deref() == Some("cancelled")
        && let Some(cancelled_at) = item.cancelled_at.as_deref()
    {
        lines.push(field_line(
            "取消时间",
            &format_todo_time_chip_for_display(cancelled_at),
            markdown,
        ));
    }
}

fn detail_card_due_chip(item: &TodoRenderItem) -> Option<String> {
    if item.due_date.is_none()
        && item.due_at.as_deref() == item.reminder_at.as_deref()
        && item.reminder_at.is_some()
    {
        return None;
    }
    item.due_at
        .as_deref()
        .map(format_todo_time_chip_for_display)
        .or_else(|| {
            item.due_date
                .as_deref()
                .map(format_todo_time_chip_for_display)
        })
}

fn reminder_field_value(item: &TodoRenderItem, mode: ReminderFieldMode) -> Option<String> {
    match mode {
        ReminderFieldMode::Current => item
            .reminder_at
            .as_deref()
            .map(format_todo_time_chip_for_display),
        ReminderFieldMode::Next => item
            .next_reminder_at
            .as_deref()
            .map(format_todo_time_chip_for_display)
            .or_else(|| {
                item.reminder_at
                    .as_deref()
                    .map(format_todo_time_chip_for_display)
            }),
    }
}

fn field_line(label: &str, value: &str, markdown: bool) -> String {
    if markdown {
        format!(
            "**{}**：{}",
            escape_markdown_inline(label),
            escape_markdown_inline(value)
        )
    } else {
        format!("{label}：{value}")
    }
}

fn status_machine_str(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::Completed => "completed",
        TodoStatus::Cancelled => "cancelled",
    }
}

fn todo_status_display_label(status: &str) -> Option<&'static str> {
    match status {
        "pending" => Some("进行中"),
        "completed" => Some("已完成"),
        "cancelled" => Some("已取消"),
        _ => None,
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

    fn recurring_item() -> TodoItem {
        TodoItem {
            id: "1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "private:u1".to_owned(),
            title: "检查 *日志*".to_owned(),
            detail: Some("确认 [推送] 没失败".to_owned()),
            raw_text: None,
            due_date: Some("2099-01-01".to_owned()),
            due_at: Some("2099-01-01 10:00:00".to_owned()),
            reminder_at: Some("2099-01-01 09:30:00".to_owned()),
            time_precision: TodoTimePrecision::DateTime,
            recurrence_kind: TodoRecurrenceKind::EveryNDays,
            recurrence_interval_days: 2,
            status: TodoStatus::Pending,
            created_at: "2026-07-03T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-03T09:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
        }
    }

    #[test]
    fn single_reminder_push_uses_alarm_style_and_escapes_markdown() {
        let body = format_todo_single_reminder_push(&recurring_item());

        assert!(body.text.starts_with("⏰ 待办提醒"));
        assert!(body.markdown.starts_with("# ⏰ 待办提醒"));
        assert!(body.text.contains("检查 *日志*"));
        assert!(body.markdown.contains("检查 \\*日志\\*"));
        assert!(body.markdown.contains("确认 \\[推送\\] 没失败"));
        assert!(body.text.contains("状态：进行中"));
        assert!(body.text.contains("重复：隔天"));
        assert!(body.text.contains("下一次提醒：99-01-03 9:30（六）"));
    }

    #[test]
    fn generic_card_can_render_next_reminder_as_primary_field() {
        let mut item = TodoRenderItem::from_todo(&recurring_item());
        item.reminder_at = Some("2099-01-03 09:30:00".to_owned());
        item.next_reminder_at = Some("2099-01-05 09:30:00".to_owned());

        let body = format_todo_cards(
            "✅ 已完成本次待办",
            &[item],
            TodoCardOptions {
                reminder_mode: ReminderFieldMode::Current,
                show_next_reminder: true,
            },
        );

        assert!(body.text.contains("提醒：99-01-03 9:30（六）"));
        assert!(body.text.contains("下一次提醒：99-01-05 9:30（一）"));
    }
}

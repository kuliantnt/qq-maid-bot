//! Todo 用户可见回复格式化。
//!
//! Slash 写入口已移除，本模块只保留查询列表、Tool pending 确认结果与必要提示文案。
//! 文案仍集中维护，避免结构调整影响 QQ 侧用户体验。

use crate::{
    runtime::{
        respond::{
            command_render::{escape_markdown_inline, escape_markdown_text},
            common::{CommandBody, clean_string, truncate_chars},
        },
        todo::{TodoItem, TodoStatus, display_todo_time},
    },
    util::time_context::format_todo_time_for_display,
};

pub(super) fn format_todo_write_tool_only_reply() -> CommandBody {
    CommandBody::plain(
        "待办写操作已统一改为自然语言工具调用。请直接说“帮我新增待办……”或“完成第一条待办”。",
    )
}

pub(super) fn format_todo_list_reply(items: &[TodoItem]) -> CommandBody {
    if items.is_empty() {
        return simple_todo_notice("当前没有未完成待办。");
    }
    let mut rows = vec!["待办列表：".to_owned()];
    rows.extend(format_todo_rows(items));
    let mut markdown_rows = vec!["# 待办列表".to_owned()];
    markdown_rows.extend(format_todo_rows_markdown(items, false));
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(super) fn format_todo_all_reply(items: &[TodoItem]) -> CommandBody {
    if items.is_empty() {
        return simple_todo_notice("当前没有待办。");
    }
    let mut rows = vec!["全部待办：".to_owned()];
    rows.extend(format_todo_rows_with_status(items));
    let mut markdown_rows = vec!["# 全部待办".to_owned()];
    markdown_rows.extend(format_todo_rows_markdown(items, true));
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(super) fn format_todo_done_list_reply(items: &[TodoItem]) -> CommandBody {
    if items.is_empty() {
        return simple_todo_notice("当前没有已完成待办。");
    }
    let mut rows = vec!["已完成待办：".to_owned()];
    rows.extend(format_completed_todo_rows(items));
    let mut markdown_rows = vec!["# 已完成待办".to_owned()];
    markdown_rows.extend(format_completed_todo_rows_markdown(items));
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(super) fn format_todo_cancelled_list_reply(items: &[TodoItem]) -> CommandBody {
    if items.is_empty() {
        return simple_todo_notice("当前没有已取消待办。");
    }
    let mut rows = vec!["已取消待办：".to_owned()];
    rows.extend(format_cancelled_todo_rows(items));
    let mut markdown_rows = vec!["# 已取消待办".to_owned()];
    markdown_rows.extend(format_cancelled_todo_rows_markdown(items));
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(super) fn format_todo_search_reply(items: &[TodoItem], query: &str) -> CommandBody {
    if query.trim().is_empty() {
        return format_todo_list_reply(items);
    }
    if items.is_empty() {
        return simple_todo_notice("没有找到匹配的未完成待办。");
    }
    let mut rows = vec![format!("待办搜索结果：{}", query.trim())];
    rows.extend(format_todo_rows(items));
    let mut markdown_rows = vec![format!(
        "# 待办搜索结果：{}",
        escape_markdown_inline(query.trim())
    )];
    markdown_rows.extend(format_todo_rows_markdown(items, false));
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(super) fn format_completed_todo_time_query_reply(
    items: &[TodoItem],
    source_condition: &str,
) -> CommandBody {
    if items.is_empty() {
        return simple_todo_notice("没有找到符合完成时间条件的已完成待办。");
    }
    let mut rows = vec![format!("已完成待办：{}", source_condition.trim())];
    rows.extend(format_completed_todo_rows(items));
    let mut markdown_rows = vec![format!(
        "# 已完成待办：{}",
        escape_markdown_inline(source_condition.trim())
    )];
    markdown_rows.extend(format_completed_todo_rows_markdown(items));
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

/// 通用待办行格式化：`序号. 标题`，换行后跟一行时间。内部 ID 只能留在
/// 快照映射和存储层，这里只渲染用户可读字段。
fn format_todo_rows_with_time(
    items: &[TodoItem],
    time_label: &str,
    time_value: impl Fn(&TodoItem) -> String,
) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut row = format!(
                "{}. {}\n   {}：{}",
                index + 1,
                format_todo_inline(item),
                time_label,
                time_value(item)
            );
            if let Some(detail) = item
                .detail
                .as_deref()
                .and_then(|value| clean_string(value.to_owned()))
            {
                row.push_str(&format!("\n   详情：{}", truncate_chars(&detail, 80)));
            }
            row
        })
        .collect()
}

fn format_todo_rows(items: &[TodoItem]) -> Vec<String> {
    format_todo_rows_with_time(items, "时间", display_todo_time)
}

fn format_completed_todo_rows(items: &[TodoItem]) -> Vec<String> {
    format_todo_rows_with_time(items, "完成时间", display_todo_completed_at)
}

fn format_cancelled_todo_rows(items: &[TodoItem]) -> Vec<String> {
    format_todo_rows_with_time(items, "取消时间", display_todo_cancelled_at)
}

fn format_todo_rows_with_status(items: &[TodoItem]) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let (time_label, time_text) = match item.status {
                TodoStatus::Completed => ("完成时间", display_todo_completed_at(item).to_owned()),
                _ => ("时间", display_todo_time(item)),
            };
            let mut row = format!(
                "{}. {}（{}）\n   {}：{}",
                index + 1,
                format_todo_inline(item),
                crate::runtime::todo::status::status_cn_short(&item.status),
                time_label,
                time_text
            );
            if let Some(detail) = item
                .detail
                .as_deref()
                .and_then(|value| clean_string(value.to_owned()))
            {
                row.push_str(&format!("\n   详情：{}", truncate_chars(&detail, 80)));
            }
            row
        })
        .collect()
}

pub(super) fn format_todo_inline(item: &TodoItem) -> String {
    truncate_chars(&item.title, 80)
}

fn display_todo_completed_at(item: &TodoItem) -> String {
    item.completed_at
        .as_deref()
        .map(format_todo_timestamp_for_display)
        .unwrap_or_else(|| "未知".to_owned())
}

fn display_todo_cancelled_at(item: &TodoItem) -> String {
    item.cancelled_at
        .as_deref()
        .map(format_todo_timestamp_for_display)
        .unwrap_or_else(|| "未知".to_owned())
}

fn format_todo_timestamp_for_display(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "未知".to_owned();
    }
    format_todo_time_for_display(value)
}

pub(super) fn format_todo_bulk_delete_result(
    deleted_count: usize,
    skipped_count: usize,
    source_condition: &str,
) -> CommandBody {
    format_todo_bulk_delete_result_for_status(
        TodoStatus::Completed,
        deleted_count,
        skipped_count,
        source_condition,
        None,
    )
}

pub(super) fn format_todo_bulk_delete_result_for_status(
    status: TodoStatus,
    deleted_count: usize,
    skipped_count: usize,
    source_condition: &str,
    items: Option<&[TodoItem]>,
) -> CommandBody {
    let status_label = crate::runtime::todo::status::status_cn_short(&status);
    if deleted_count == 0 {
        return simple_todo_notice(&format!("没有可删除的{status_label}待办。"));
    }
    let mut rows = vec![
        format!("已删除 {} 条{status_label}待办", deleted_count),
        format!("来源：{}", source_condition.trim()),
    ];
    if skipped_count > 0 {
        rows.push(format!(
            "跳过 {skipped_count} 条已不存在或状态已变化的待办。"
        ));
    }
    if let Some(items) = items {
        rows.extend(format_completed_todo_rows(items));
    }
    let mut markdown_rows = vec![format!("# 已删除 {} 条{status_label}待办", deleted_count)];
    markdown_rows.push(format!(
        "来源：{}",
        escape_markdown_inline(source_condition.trim())
    ));
    if skipped_count > 0 {
        markdown_rows.push(format!(
            "> 跳过 {skipped_count} 条已不存在或状态已变化的待办。"
        ));
    }
    if let Some(items) = items {
        markdown_rows.extend(format_completed_todo_rows_markdown(items));
    }
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(super) fn format_todo_pending_add_waiting_reply() -> CommandBody {
    simple_todo_notice(
        "这条新增待办还在等待确认。要新增请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。",
    )
}

pub(super) fn format_todo_pending_delete_waiting_reply() -> CommandBody {
    simple_todo_notice(
        "这条待办删除操作还在等待确认。要删除请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。",
    )
}

pub(super) fn format_todo_pending_bulk_delete_waiting_reply() -> CommandBody {
    simple_todo_notice(
        "这批待办删除操作还在等待确认。要删除请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。",
    )
}

pub(super) fn format_todo_inline_markdown(item: &TodoItem) -> String {
    format!(
        "**{}**",
        escape_markdown_text(&truncate_chars(&item.title, 80))
    )
}

pub(super) fn simple_todo_notice(text: &str) -> CommandBody {
    CommandBody::dual(text.to_owned(), escape_markdown_text(text))
}

fn format_todo_rows_markdown(items: &[TodoItem], with_status: bool) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .flat_map(|(index, item)| {
            let (time_label, time_text) = match item.status {
                TodoStatus::Completed => ("完成时间", display_todo_completed_at(item).to_owned()),
                _ => ("时间", display_todo_time(item)),
            };
            let mut lines = vec![if with_status {
                format!(
                    "{}. {}（{}）",
                    index + 1,
                    format_todo_inline_markdown(item),
                    escape_markdown_inline(crate::runtime::todo::status::status_cn_short(
                        &item.status
                    ))
                )
            } else {
                format!("{}. {}", index + 1, format_todo_inline_markdown(item))
            }];
            lines.push(format!(
                "   - **{}**：{}",
                escape_markdown_inline(time_label),
                escape_markdown_inline(&time_text)
            ));
            if let Some(detail) = item
                .detail
                .as_deref()
                .and_then(|value| clean_string(value.to_owned()))
            {
                lines.push(format!(
                    "   - **详情**：{}",
                    escape_markdown_text(&truncate_chars(&detail, 80))
                ));
            }
            lines
        })
        .collect()
}

fn format_completed_todo_rows_markdown(items: &[TodoItem]) -> Vec<String> {
    format_todo_rows_markdown(items, false)
}

fn format_cancelled_todo_rows_markdown(items: &[TodoItem]) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut rows = vec![
                format!("{}. **{}**", index + 1, format_todo_inline_markdown(item)),
                format!(
                    "   - **取消时间**：{}",
                    escape_markdown_inline(&display_todo_cancelled_at(item))
                ),
            ];
            if let Some(detail) = item
                .detail
                .as_deref()
                .and_then(|value| clean_string(value.to_owned()))
            {
                rows.push(format!(
                    "   - **详情**：{}",
                    escape_markdown_text(&truncate_chars(&detail, 80))
                ));
            }
            rows.join("\n")
        })
        .collect()
}

//! Todo 用户可见回复格式化。
//!
//! 本模块集中维护待办列表、确认提示和操作结果文案。拆分时必须保持文案、
//! 标点、排序和截断规则不变，避免结构调整影响 QQ 侧用户体验。

use crate::{
    runtime::{
        pending::PendingTodoAction,
        todo::{TodoItem, TodoItemDraft, display_draft_time, display_todo_time},
    },
    util::time_context::format_todo_time_for_display,
};

use crate::runtime::respond::common::{clean_string, truncate_chars};

pub(super) fn format_todo_numbered_item_operation_result(
    success_label: &str,
    success_items: &[(usize, TodoItem)],
    missing_label: &str,
    missing_numbers: &[usize],
) -> String {
    let mut rows = Vec::new();
    if !success_items.is_empty() {
        rows.push(format!("{success_label}："));
        rows.extend(
            success_items
                .iter()
                .map(|(number, item)| format!("[{number}] {}", truncate_chars(&item.title, 80))),
        );
    }
    if !missing_numbers.is_empty() {
        rows.push(format!(
            "{}：{}",
            missing_label,
            format_todo_number_list(missing_numbers)
        ));
    }
    if rows.is_empty() {
        rows.push(format!("{}：无", missing_label));
    }
    rows.join("\n")
}

pub(super) fn format_todo_number_usage_reply() -> String {
    "编号只能使用正整数，并用空格、逗号或中文逗号分隔。".to_owned()
}

fn format_todo_number_list(numbers: &[usize]) -> String {
    numbers
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join("、")
}

pub(super) fn format_todo_list_reply(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "当前没有未完成待办。".to_owned();
    }
    let mut rows = vec!["待办列表：".to_owned()];
    rows.extend(format_todo_rows(items));
    rows.join("\n")
}

pub(super) fn format_todo_all_reply(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "当前没有待办。".to_owned();
    }
    let mut rows = vec!["全部待办：".to_owned()];
    rows.extend(format_todo_rows_with_status(items));
    rows.join("\n")
}

pub(super) fn format_todo_done_list_reply(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "当前没有已完成待办。".to_owned();
    }
    let mut rows = vec!["已完成待办：".to_owned()];
    rows.extend(format_completed_todo_rows(items));
    rows.join("\n")
}

pub(super) fn format_todo_search_reply(items: &[TodoItem], query: &str) -> String {
    if query.trim().is_empty() {
        return format_todo_list_reply(items);
    }
    if items.is_empty() {
        return "没有找到匹配的未完成待办。".to_owned();
    }
    let mut rows = vec![format!("待办搜索结果：{}", query.trim())];
    rows.extend(format_todo_rows(items));
    rows.join("\n")
}

pub(super) fn format_todo_index_edit_hint(todo_id: &str, body: &str) -> String {
    format!(
        "看起来你想修改待办 [{}]，请使用：\n/todo edit {} 标题：{}；内容：...",
        todo_id.trim(),
        todo_id.trim(),
        body.trim()
    )
}

pub(super) fn format_completed_todo_time_query_reply(
    items: &[TodoItem],
    source_condition: &str,
) -> String {
    if items.is_empty() {
        return "没有找到符合完成时间条件的已完成待办。".to_owned();
    }
    let mut rows = vec![format!("已完成待办：{}", source_condition.trim())];
    rows.extend(format_completed_todo_rows(items));
    rows.join("\n")
}

/// 通用待办行格式化：`序号. [id] 标题`，换行后跟一行时间（标签由 `time_label` 指定，
/// 时间值由 `time_value` 计算），若有详情再追加一行。
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

fn format_todo_rows_with_status(items: &[TodoItem]) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let (time_label, time_text) = match item.status {
                crate::runtime::todo::TodoStatus::Completed => {
                    ("完成时间", display_todo_completed_at(item).to_owned())
                }
                _ => ("时间", display_todo_time(item)),
            };
            let mut row = format!(
                "{}. {}（{}）\n   {}：{}",
                index + 1,
                format_todo_inline(item),
                display_todo_status(item),
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

fn display_todo_status(item: &TodoItem) -> &'static str {
    match &item.status {
        crate::runtime::todo::TodoStatus::Pending => "未完成",
        crate::runtime::todo::TodoStatus::Completed => "已完成",
        crate::runtime::todo::TodoStatus::Cancelled => "已取消",
    }
}

pub(super) fn format_todo_inline(item: &TodoItem) -> String {
    format!("[{}] {}", item.id, truncate_chars(&item.title, 80))
}

pub(super) fn format_todo_edit_result(item: &TodoItem) -> String {
    let mut rows = vec![
        format!("已修改待办：{}", format_todo_inline(item)),
        format!("时间：{}", display_todo_time(item)),
    ];
    rows.push(format!(
        "详情：{}",
        item.detail.as_deref().unwrap_or("无").trim()
    ));
    rows.join("\n")
}

fn display_todo_completed_at(item: &TodoItem) -> String {
    item.completed_at
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

pub(super) fn format_todo_add_confirm(draft: &TodoItemDraft) -> String {
    let rows = [
        "待确认新增待办：".to_owned(),
        format!("- 标题：{}", draft.title),
        format!("- 详情：{}", draft.detail.as_deref().unwrap_or("无").trim()),
        format!("- 时间：{}", display_draft_time(draft)),
        String::new(),
        build_todo_confirm_hint(),
    ];
    rows.join("\n")
}

pub(super) fn format_todo_done_confirm(item: &TodoItem) -> String {
    format!(
        "确认完成这条待办？\n{}\n时间：{}\n\n{}",
        format_todo_inline(item),
        display_todo_time(item),
        build_todo_confirm_hint()
    )
}

pub(super) fn format_todo_delete_confirm(item: &TodoItem) -> String {
    format!(
        "确认删除这条待办？删除后会标记为已取消。\n{}\n时间：{}\n\n{}",
        format_todo_inline(item),
        display_todo_time(item),
        build_todo_confirm_hint()
    )
}

pub(super) fn format_todo_bulk_delete_summary(items: &[TodoItem]) -> String {
    let mut rows = items
        .iter()
        .take(5)
        .map(|item| format!("- {}", format_todo_inline(item)))
        .collect::<Vec<_>>();
    if items.len() > 5 {
        rows.push(format!("- 另有 {} 条", items.len() - 5));
    }
    rows.join("\n")
}

pub(super) fn format_todo_bulk_delete_confirm(
    count: usize,
    source_condition: &str,
    summary: &str,
) -> String {
    format!(
        "确认删除这 {count} 条已完成待办？来源：{}\n{}\n\n{}",
        source_condition.trim(),
        summary.trim(),
        build_todo_confirm_hint()
    )
}

pub(super) fn format_todo_bulk_delete_result(
    cancelled: &[TodoItem],
    skipped_count: usize,
    source_condition: &str,
) -> String {
    if cancelled.is_empty() {
        return "没有可删除的已完成待办。".to_owned();
    }
    let mut rows = vec![format!(
        "已删除 {} 条已完成待办。来源：{}",
        cancelled.len(),
        source_condition.trim()
    )];
    if skipped_count > 0 {
        rows.push(format!(
            "跳过 {skipped_count} 条已不存在、已取消或状态已变化的待办。"
        ));
    }
    rows.extend(format_completed_todo_rows(cancelled));
    rows.join("\n")
}

pub(super) fn format_todo_edit_confirm(before: &TodoItem, draft: &TodoItemDraft) -> String {
    let mut rows = vec![
        "待确认修改待办：".to_owned(),
        format_todo_inline(before),
        format!("- 标题：{} -> {}", before.title, draft.title),
    ];
    let before_detail = before.detail.as_deref().unwrap_or("无");
    let after_detail = draft.detail.as_deref().unwrap_or("无");
    if before_detail != after_detail {
        rows.push(format!("- 详情：{} -> {}", before_detail, after_detail));
    }
    let before_time = display_todo_time(before);
    let after_time = display_draft_time(draft);
    if before_time != after_time {
        rows.push(format!("- 时间：{} -> {}", before_time, after_time));
    }
    rows.push(String::new());
    rows.push(build_todo_confirm_hint());
    rows.join("\n")
}

pub(super) fn format_todo_pending_add_waiting_reply() -> String {
    "这条新增待办还在等待确认。要新增请回复“确认 / 可以 / 好”；要调整请直接继续说怎么改；要放弃请回复“取消 / 不要 / 算了”。"
        .to_owned()
}

pub(super) fn format_todo_pending_edit_waiting_reply() -> String {
    "这条待办还在等待确认。要执行请回复“确认 / 可以 / 好”；要调整请直接继续说怎么改；要放弃请回复“取消 / 不要 / 算了”。"
        .to_owned()
}

pub(super) fn format_todo_pending_done_waiting_reply() -> String {
    "这条待办完成操作还在等待确认。要完成请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。"
        .to_owned()
}

pub(super) fn format_todo_pending_delete_waiting_reply() -> String {
    "这条待办删除操作还在等待确认。要删除请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。"
        .to_owned()
}

pub(super) fn format_todo_pending_bulk_delete_waiting_reply() -> String {
    "这批待办删除操作还在等待确认。要删除请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。"
        .to_owned()
}

pub(super) fn format_todo_pending_select_waiting_reply() -> String {
    "待办候选还在等待选择。请回复候选编号继续，或回复“取消 / 不要 / 算了”放弃。".to_owned()
}

pub(super) fn format_todo_candidate_selection(
    action: &PendingTodoAction,
    candidates: &[TodoItem],
) -> String {
    let action_text = match action {
        PendingTodoAction::Done => "完成",
        PendingTodoAction::Edit => "修改",
        PendingTodoAction::Delete => "删除",
    };
    let mut rows = vec![format!(
        "找到多条待办，请回复编号选择要{action_text}哪一条："
    )];
    rows.extend(format_todo_candidate_rows(candidates));
    rows.push(String::new());
    rows.push("回复编号只表示选择候选；选中后还会再次确认。回复“取消”放弃。".to_owned());
    rows.join("\n")
}

fn format_todo_candidate_rows(items: &[TodoItem]) -> Vec<String> {
    format_todo_rows_with_time(items, "时间", display_todo_time)
}

pub(super) fn format_todo_no_match_reply(target: &str) -> String {
    format!(
        "没有找到匹配的未完成待办：{}",
        target.trim().trim_matches(&['[', ']'][..])
    )
}

pub(super) fn format_todo_no_list_index_reply(index: usize) -> String {
    format!("最近的待办列表里没有第 {index} 条。请先发送 /todo 查看列表，或使用 [真实ID]。")
}

pub(super) fn build_todo_confirm_hint() -> String {
    "回复“确认 / 可以 / 好”执行。\n回复“取消 / 不要 / 算了”放弃。".to_owned()
}

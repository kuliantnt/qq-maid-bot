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

use crate::runtime::respond::{
    command_render::{escape_markdown_inline, escape_markdown_text},
    common::{CommandBody, clean_string, truncate_chars},
};

pub(super) fn format_todo_numbered_item_operation_result(
    success_label: &str,
    success_items: &[(usize, TodoItem)],
    missing_label: &str,
    missing_numbers: &[usize],
) -> CommandBody {
    let mut rows = Vec::new();
    let mut markdown_rows = Vec::new();
    if !success_items.is_empty() {
        rows.push(format!("{success_label}："));
        markdown_rows.push(format!("# {}", escape_markdown_inline(success_label)));
        rows.extend(
            success_items.iter().map(|(number, item)| {
                format!("第 {number} 条：{}", truncate_chars(&item.title, 80))
            }),
        );
        markdown_rows.extend(success_items.iter().map(|(number, item)| {
            format!("- {}", format_todo_numbered_item_markdown(*number, item))
        }));
    }
    if !missing_numbers.is_empty() {
        rows.push(format!(
            "{}：{}",
            missing_label,
            format_todo_number_list(missing_numbers)
        ));
        markdown_rows.push(format!(
            "> **{}**：{}",
            escape_markdown_inline(missing_label),
            escape_markdown_inline(&format_todo_number_list(missing_numbers))
        ));
    }
    if rows.is_empty() {
        rows.push(format!("{}：无", missing_label));
        markdown_rows.push(format!(
            "> **{}**：无",
            escape_markdown_inline(missing_label)
        ));
    }
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
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

/// 通用待办行格式化：`序号. 标题`，换行后跟一行时间（标签由 `time_label` 指定，
/// 时间值由 `time_value` 计算），若有详情再追加一行。
/// 内部 ID 只能留在快照映射和存储层，这里只渲染用户可读字段。
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
    truncate_chars(&item.title, 80)
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

pub(super) fn format_todo_edit_result_body(item: &TodoItem) -> CommandBody {
    let text = format_todo_edit_result(item);
    let markdown = [
        "# 已修改待办".to_owned(),
        format!("- {}", format_todo_inline_markdown(item)),
        format!(
            "- **时间**：{}",
            escape_markdown_inline(&display_todo_time(item))
        ),
        format!(
            "- **详情**：{}",
            escape_markdown_text(item.detail.as_deref().unwrap_or("无").trim())
        ),
    ]
    .join("\n");
    CommandBody::dual(text, markdown)
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

pub(super) fn format_todo_add_confirm(draft: &TodoItemDraft) -> CommandBody {
    let rows = [
        "待确认新增待办：".to_owned(),
        format!("- 标题：{}", draft.title),
        format!("- 详情：{}", draft.detail.as_deref().unwrap_or("无").trim()),
        format!("- 时间：{}", display_draft_time(draft)),
        String::new(),
        build_todo_confirm_hint(),
    ];
    let markdown = [
        "# 待确认新增待办".to_owned(),
        format!("- **标题**：{}", escape_markdown_text(&draft.title)),
        format!(
            "- **详情**：{}",
            escape_markdown_text(draft.detail.as_deref().unwrap_or("无").trim())
        ),
        format!(
            "- **时间**：{}",
            escape_markdown_inline(&display_draft_time(draft))
        ),
        String::new(),
        build_todo_confirm_hint_markdown(),
    ]
    .join("\n");
    CommandBody::dual(rows.join("\n"), markdown)
}

pub(super) fn format_todo_done_confirm(item: &TodoItem) -> CommandBody {
    let text = format!(
        "确认完成这条待办？\n{}\n时间：{}\n\n{}",
        format_todo_inline(item),
        display_todo_time(item),
        build_todo_confirm_hint()
    );
    let markdown = [
        "# 确认完成待办".to_owned(),
        format!("- {}", format_todo_inline_markdown(item)),
        format!(
            "- **时间**：{}",
            escape_markdown_inline(&display_todo_time(item))
        ),
        String::new(),
        build_todo_confirm_hint_markdown(),
    ]
    .join("\n");
    CommandBody::dual(text, markdown)
}

pub(super) fn format_todo_delete_confirm(item: &TodoItem) -> CommandBody {
    let text = format!(
        "确认删除这条待办？删除后会标记为已取消。\n{}\n时间：{}\n\n{}",
        format_todo_inline(item),
        display_todo_time(item),
        build_todo_confirm_hint()
    );
    let markdown = [
        "# 确认删除待办".to_owned(),
        "删除后会标记为已取消。".to_owned(),
        String::new(),
        format!("- {}", format_todo_inline_markdown(item)),
        format!(
            "- **时间**：{}",
            escape_markdown_inline(&display_todo_time(item))
        ),
        String::new(),
        build_todo_confirm_hint_markdown(),
    ]
    .join("\n");
    CommandBody::dual(text, markdown)
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
) -> CommandBody {
    let text = format!(
        "确认删除这 {count} 条已完成待办？来源：{}\n{}\n\n{}",
        source_condition.trim(),
        summary.trim(),
        build_todo_confirm_hint()
    );
    let markdown = [
        format!("# 确认删除 {count} 条已完成待办"),
        format!("来源：{}", escape_markdown_inline(source_condition.trim())),
        String::new(),
        summary
            .lines()
            .map(|line| {
                if let Some(item) = line.trim().strip_prefix("- ") {
                    format!("- {}", escape_markdown_text(item))
                } else {
                    escape_markdown_text(line.trim())
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        String::new(),
        build_todo_confirm_hint_markdown(),
    ]
    .join("\n");
    CommandBody::dual(text, markdown)
}

pub(super) fn format_todo_bulk_delete_result(
    cancelled: &[TodoItem],
    skipped_count: usize,
    source_condition: &str,
) -> CommandBody {
    if cancelled.is_empty() {
        return simple_todo_notice("没有可删除的已完成待办。");
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
    let mut markdown_rows = vec![format!("# 已删除 {} 条已完成待办", cancelled.len())];
    markdown_rows.push(format!(
        "来源：{}",
        escape_markdown_inline(source_condition.trim())
    ));
    if skipped_count > 0 {
        markdown_rows.push(format!(
            "> 跳过 {skipped_count} 条已不存在、已取消或状态已变化的待办。"
        ));
    }
    markdown_rows.extend(format_completed_todo_rows_markdown(cancelled));
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(super) fn format_todo_edit_confirm(before: &TodoItem, draft: &TodoItemDraft) -> CommandBody {
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
    let mut markdown_rows = vec![
        "# 待确认修改待办".to_owned(),
        format!("- {}", format_todo_inline_markdown(before)),
        format!(
            "- **标题**：{} -> {}",
            escape_markdown_text(&before.title),
            escape_markdown_text(&draft.title)
        ),
    ];
    let before_detail = before.detail.as_deref().unwrap_or("无");
    let after_detail = draft.detail.as_deref().unwrap_or("无");
    if before_detail != after_detail {
        markdown_rows.push(format!(
            "- **详情**：{} -> {}",
            escape_markdown_text(before_detail),
            escape_markdown_text(after_detail)
        ));
    }
    if before_time != after_time {
        markdown_rows.push(format!(
            "- **时间**：{} -> {}",
            escape_markdown_inline(&before_time),
            escape_markdown_inline(&after_time)
        ));
    }
    markdown_rows.push(String::new());
    markdown_rows.push(build_todo_confirm_hint_markdown());
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(super) fn format_todo_pending_add_waiting_reply() -> CommandBody {
    simple_todo_notice(
        "这条新增待办还在等待确认。要新增请回复“确认 / 可以 / 好”；要调整请直接继续说怎么改；要放弃请回复“取消 / 不要 / 算了”。",
    )
}

pub(super) fn format_todo_pending_add_locked_waiting_reply() -> CommandBody {
    simple_todo_notice(
        "这条新增待办已完成外部数据校验，暂不支持直接修改。要新增请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。如需修改车次、站点或日期，请取消后重新发送 /todo add。",
    )
}

pub(super) fn format_todo_pending_edit_waiting_reply() -> CommandBody {
    simple_todo_notice(
        "这条待办还在等待确认。要执行请回复“确认 / 可以 / 好”；要调整请直接继续说怎么改；要放弃请回复“取消 / 不要 / 算了”。",
    )
}

pub(super) fn format_todo_pending_done_waiting_reply() -> CommandBody {
    simple_todo_notice(
        "这条待办完成操作还在等待确认。要完成请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。",
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

pub(super) fn format_todo_pending_select_waiting_reply() -> CommandBody {
    simple_todo_notice("待办候选还在等待选择。请回复候选编号继续，或回复“取消 / 不要 / 算了”放弃。")
}

pub(super) fn format_todo_candidate_selection(
    action: &PendingTodoAction,
    candidates: &[TodoItem],
) -> CommandBody {
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
    let mut markdown_rows = vec![format!(
        "# 找到多条待办，请回复编号选择要{}哪一条",
        escape_markdown_inline(action_text)
    )];
    markdown_rows.extend(format_todo_candidate_rows_markdown(candidates));
    markdown_rows.push(String::new());
    markdown_rows.push("> 回复编号只表示选择候选；选中后还会再次确认。回复“取消”放弃。".to_owned());
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

fn format_todo_candidate_rows(items: &[TodoItem]) -> Vec<String> {
    format_todo_rows_with_time(items, "时间", display_todo_time)
}

pub(super) fn format_todo_no_match_reply(target: &str) -> CommandBody {
    simple_todo_notice(&format!("没有找到匹配的未完成待办：{}", target.trim()))
}

pub(super) fn format_todo_missing_pending_index_reply(index: usize) -> CommandBody {
    simple_todo_notice(&format!(
        "当前待办序号已失效或不存在第 {index} 条，请重新执行 /todo 获取最新列表。"
    ))
}

pub(super) fn format_todo_missing_completed_index_reply(index: usize) -> CommandBody {
    simple_todo_notice(&format!(
        "当前已完成待办序号已失效或不存在第 {index} 条，请重新执行 /todo done 获取最新列表。"
    ))
}

pub(super) fn format_todo_pending_snapshot_unavailable_reply() -> CommandBody {
    simple_todo_notice("当前待办序号已失效，请重新执行 /todo 获取最新列表。")
}

pub(super) fn format_todo_completed_snapshot_unavailable_reply() -> CommandBody {
    simple_todo_notice("当前已完成待办序号已失效，请重新执行 /todo done 获取最新列表。")
}

pub(super) fn format_todo_edit_usage_reply() -> CommandBody {
    CommandBody::plain("用法：/todo edit 列表序号或关键词 新内容")
}

pub(super) fn format_todo_delete_usage_reply() -> CommandBody {
    CommandBody::plain("用法：/todo delete 列表序号或关键词；清理已完成任务用 /todo delete done")
}

pub(super) fn format_todo_inline_markdown(item: &TodoItem) -> String {
    format!(
        "**{}**",
        escape_markdown_text(&truncate_chars(&item.title, 80))
    )
}

pub(super) fn build_todo_confirm_hint() -> String {
    "回复“确认 / 可以 / 好”执行。\n回复“取消 / 不要 / 算了”放弃。".to_owned()
}

pub(super) fn build_todo_confirm_hint_markdown() -> String {
    "- 回复“确认 / 可以 / 好”执行。\n- 回复“取消 / 不要 / 算了”放弃。".to_owned()
}

pub(super) fn simple_todo_notice(text: &str) -> CommandBody {
    CommandBody::dual(text.to_owned(), escape_markdown_text(text))
}

pub(super) fn format_todo_numbered_item_markdown(number: usize, item: &TodoItem) -> String {
    format!("第 {number} 条：{}", format_todo_inline_markdown(item))
}

fn format_todo_rows_markdown(items: &[TodoItem], with_status: bool) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .flat_map(|(index, item)| {
            let (time_label, time_text) = match item.status {
                crate::runtime::todo::TodoStatus::Completed => {
                    ("完成时间", display_todo_completed_at(item).to_owned())
                }
                _ => ("时间", display_todo_time(item)),
            };
            let mut lines = vec![if with_status {
                format!(
                    "{}. {}（{}）",
                    index + 1,
                    format_todo_inline_markdown(item),
                    escape_markdown_inline(display_todo_status(item))
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

fn format_todo_candidate_rows_markdown(items: &[TodoItem]) -> Vec<String> {
    format_todo_rows_markdown(items, false)
}

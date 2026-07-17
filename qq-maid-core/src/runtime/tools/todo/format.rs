//! Todo 用户可见回复格式化。
//!
//! Slash 写入口已移除，本模块只保留查询列表、Tool pending 确认结果与必要提示文案。
//! 文案仍集中维护，避免结构调整影响 QQ 侧用户体验。

use std::borrow::Cow;

use qq_maid_common::{
    markdown::{escape_inline, escape_text},
    time_context::{format_todo_time_chip_for_display, local_date_from_timestamp},
};

use crate::runtime::{
    respond::common::{CommandBody, clean_string, truncate_chars},
    tools::todo::{TodoItem, TodoStatus, preview_next_reminder_at, recurrence_label},
};

pub(crate) const TODO_LIST_VISIBLE_LIMIT: usize = 10;

pub(crate) fn visible_todo_items(items: &[TodoItem], force_full: bool) -> &[TodoItem] {
    if force_full || items.len() <= TODO_LIST_VISIBLE_LIMIT {
        return items;
    }
    &items[..TODO_LIST_VISIBLE_LIMIT]
}

pub(crate) fn visible_todo_all_board_items(items: &[TodoItem], force_full: bool) -> &[TodoItem] {
    if force_full || items.len() <= TODO_LIST_VISIBLE_LIMIT {
        return items;
    }
    &items[..TODO_LIST_VISIBLE_LIMIT]
}

pub(crate) fn format_todo_detail_line(detail: &str, markdown: bool) -> String {
    let detail = truncate_chars(detail, 56);
    if markdown {
        format!("   {}", escape_text(&detail))
    } else {
        format!("   {detail}")
    }
}

pub(crate) fn todo_due_chip(item: &TodoItem) -> Option<String> {
    effective_due_source(item).map(format_todo_time_chip_for_display)
}

pub(crate) fn todo_timestamp_chip(value: &str) -> Option<String> {
    clean_todo_time_value(value).map(format_todo_time_chip_for_display)
}

pub(crate) fn todo_reminder_list_text(item: &TodoItem, due_time: Option<&str>) -> Option<String> {
    let reminder_at = item
        .reminder_at
        .as_deref()
        .and_then(clean_todo_time_value)?;
    let reminder = format_todo_time_chip_for_display(reminder_at);
    let Some(due_time) = due_time else {
        return Some(reminder);
    };
    let same_day = local_date_from_timestamp(due_time)
        .zip(local_date_from_timestamp(reminder_at))
        .is_some_and(|(due_date, reminder_date)| due_date == reminder_date);
    if same_day {
        Some(todo_time_of_day(reminder_at).unwrap_or(reminder))
    } else {
        Some(reminder)
    }
}

pub(crate) fn format_todo_natural_list_item(
    index: usize,
    item: &TodoItem,
    time: Option<String>,
    markdown: bool,
    status_suffix: Option<&str>,
) -> String {
    let title = if markdown {
        format_todo_inline_markdown(item)
    } else {
        format_todo_inline(item)
    };
    let mut lines = vec![format!("{}. {}", index + 1, title)];
    if let Some(status) = status_suffix
        && let Some(title_line) = lines.first_mut()
    {
        title_line.push_str(&format!("（{}）", status));
    }
    if let Some(time_line) = format_todo_time_reminder_line(item, time, markdown) {
        lines.push(time_line);
    }
    if let Some(detail) = item
        .detail
        .as_deref()
        .and_then(|value| clean_string(value.to_owned()))
    {
        lines.push(format_todo_detail_line(&detail, markdown));
    }
    lines.join("\n")
}

fn format_todo_time_reminder_line(
    item: &TodoItem,
    time: Option<String>,
    markdown: bool,
) -> Option<String> {
    let due_source = effective_due_source(item);
    let reminder = todo_reminder_list_text(item, due_source);
    let recurrence = todo_recurrence_summary_text(item);
    let mut parts = match (time, reminder) {
        (Some(time), Some(reminder)) => vec![time, format!("提醒 {reminder}")],
        (Some(time), None) => vec![time],
        (None, Some(reminder)) => vec![format!("提醒 {reminder}")],
        (None, None) => return None,
    };
    if let Some(recurrence) = recurrence {
        parts.push(recurrence);
    }
    let text = parts.join(" · ");
    if markdown {
        Some(format!("   {}", escape_inline(&text)))
    } else {
        Some(format!("   {text}"))
    }
}

fn todo_recurrence_summary_text(item: &TodoItem) -> Option<String> {
    let recurrence = recurrence_label(
        &item.recurrence_kind,
        item.recurrence_interval_days,
        item.recurrence_interval,
        &item.recurrence_unit,
    )?;
    let next = preview_next_reminder_at(item)
        .ok()
        .flatten()
        .map(|value| format_todo_time_chip_for_display(&value));
    Some(match next {
        Some(next) => format!("重复 {recurrence} · 下次 {next}"),
        None => format!("重复 {recurrence}"),
    })
}

fn todo_time_of_day(value: &str) -> Option<String> {
    let value = value.trim();
    let time_part = value
        .split_once('T')
        .map(|(_, time)| time)
        .or_else(|| value.split_once(' ').map(|(_, time)| time))?;
    let mut parts = time_part.split(':');
    let hour = parts.next()?.parse::<u32>().ok()?;
    let minute = parts.next()?.parse::<u32>().ok()?;
    Some(format!("{hour}:{minute:02}"))
}

fn clean_todo_time_value(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() || value == "未指定" {
        None
    } else {
        Some(value)
    }
}

fn effective_due_source(item: &TodoItem) -> Option<&str> {
    // 到期时间与提醒时间解耦后，到期来源只看用户显式设置的 due_at / due_date，
    // 不再因为“due_at == reminder_at”而隐藏到期展示。
    item.due_at
        .as_deref()
        .and_then(clean_todo_time_value)
        .or_else(|| item.due_date.as_deref().and_then(clean_todo_time_value))
}

pub(crate) fn format_todo_write_tool_only_reply() -> CommandBody {
    CommandBody::plain(
        "待办写操作已统一改为自然语言工具调用。请直接说“帮我新增待办……”或“完成第一条待办”。",
    )
}

pub(crate) fn format_todo_write_private_only_reply() -> CommandBody {
    CommandBody::plain(
        "群聊当前只开放待办查询，写操作请在私聊中用自然语言发起。这样可以避免工具调用长时间占用群聊回复队列。",
    )
}

pub(crate) fn format_todo_write_tool_disabled_reply() -> CommandBody {
    CommandBody::plain("当前未启用工具调用，待办写操作暂不可用；可以使用 /todo 查看现有待办。")
}

pub(crate) fn format_todo_list_reply(items: &[TodoItem], force_full: bool) -> CommandBody {
    format_todo_status_list_reply(
        items,
        TodoStatusListFormat {
            title: Cow::Borrowed("🚧 进行中"),
            empty_text: Cow::Borrowed("暂无未完成待办"),
            time_label: Cow::Borrowed("时间"),
            time_value: todo_due_chip,
            collapse_label: Cow::Borrowed("进行中待办"),
            collapse_command: Cow::Borrowed("查看全部进行中待办"),
        },
        force_full,
    )
}

pub(crate) fn format_todo_due_date_reply(
    items: &[TodoItem],
    source_condition: &str,
    force_full: bool,
) -> CommandBody {
    if items.is_empty() {
        return simple_todo_notice("这一天暂无未完成待办。");
    }
    let title = format!("🗓 计划日期：{}", source_condition.trim());
    let collapse_label = format!("{}的未完成待办", source_condition.trim());
    format_todo_status_list_reply(
        items,
        TodoStatusListFormat {
            title: Cow::Owned(title),
            empty_text: Cow::Borrowed("这一天暂无未完成待办。"),
            time_label: Cow::Borrowed("时间"),
            time_value: todo_due_chip,
            collapse_label: Cow::Owned(collapse_label),
            collapse_command: Cow::Borrowed("查看完整结果"),
        },
        force_full,
    )
}

pub(crate) fn format_todo_all_reply(items: &[TodoItem], force_full: bool) -> CommandBody {
    if items.is_empty() {
        return simple_todo_notice("当前没有待办。");
    }
    let shown = visible_todo_all_board_items(items, force_full);
    let mut rows = vec![format!("📋 全部待办 · 共 {} 项", items.len())];
    rows.extend(format_todo_all_board_rows(shown, false));
    append_todo_collapse_hint(
        &mut rows,
        items.len().saturating_sub(shown.len()),
        Some("待办"),
        "查看完整结果",
    );
    let mut markdown_rows = vec![format!("# 📋 全部待办 · 共 {} 项", items.len())];
    markdown_rows.extend(format_todo_all_board_rows(shown, true));
    append_todo_collapse_hint(
        &mut markdown_rows,
        items.len().saturating_sub(shown.len()),
        Some("待办"),
        "查看完整结果",
    );
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(crate) fn format_todo_done_list_reply(items: &[TodoItem], force_full: bool) -> CommandBody {
    format_todo_status_list_reply(
        items,
        TodoStatusListFormat {
            title: Cow::Borrowed("✅ 已完成"),
            empty_text: Cow::Borrowed("暂无已完成待办"),
            time_label: Cow::Borrowed("完成时间"),
            time_value: display_todo_completed_at,
            collapse_label: Cow::Borrowed("已完成待办"),
            collapse_command: Cow::Borrowed("查看全部已完成待办"),
        },
        force_full,
    )
}

pub(crate) fn format_todo_search_reply(
    items: &[TodoItem],
    query: &str,
    force_full: bool,
) -> CommandBody {
    if query.trim().is_empty() {
        return format_todo_list_reply(items, force_full);
    }
    if items.is_empty() {
        return simple_todo_notice("没有找到匹配的未完成待办。");
    }
    let shown = visible_todo_items(items, force_full);
    let mut rows = vec![format!("待办搜索结果：{}", query.trim())];
    rows.extend(format_todo_rows(shown));
    let collapse_label = format!("匹配“{}”的进行中待办", query.trim());
    append_todo_collapse_hint(
        &mut rows,
        items.len().saturating_sub(shown.len()),
        Some(&collapse_label),
        "查看完整结果",
    );
    let mut markdown_rows = vec![format!("# 待办搜索结果：{}", escape_inline(query.trim()))];
    markdown_rows.extend(format_todo_rows_markdown(shown, false));
    append_todo_collapse_hint(
        &mut markdown_rows,
        items.len().saturating_sub(shown.len()),
        Some(&collapse_label),
        "查看完整结果",
    );
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(crate) fn format_completed_todo_time_query_reply(
    items: &[TodoItem],
    source_condition: &str,
    force_full: bool,
) -> CommandBody {
    if items.is_empty() {
        return simple_todo_notice("没有找到符合完成时间条件的已完成待办。");
    }
    let shown = visible_todo_items(items, force_full);
    let mut rows = vec![format!("已完成待办：{}", source_condition.trim())];
    rows.extend(format_completed_todo_rows(shown));
    let collapse_label = format!("{}的已完成待办", source_condition.trim());
    append_todo_collapse_hint(
        &mut rows,
        items.len().saturating_sub(shown.len()),
        Some(&collapse_label),
        "查看完整结果",
    );
    let mut markdown_rows = vec![format!(
        "# 已完成待办：{}",
        escape_inline(source_condition.trim())
    )];
    markdown_rows.extend(format_completed_todo_rows_markdown(shown));
    append_todo_collapse_hint(
        &mut markdown_rows,
        items.len().saturating_sub(shown.len()),
        Some(&collapse_label),
        "查看完整结果",
    );
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

/// 通用待办行格式化：标题、时间/提醒和详情拆为自然清单行。内部 ID 只能留在
/// 快照映射和存储层，这里只渲染用户可读字段。
fn format_todo_rows_with_time(
    items: &[TodoItem],
    _time_label: &str,
    time_value: impl Fn(&TodoItem) -> Option<String>,
) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            format_todo_natural_list_item(index, item, time_value(item), false, None)
        })
        .collect()
}

fn format_todo_rows(items: &[TodoItem]) -> Vec<String> {
    format_todo_rows_with_time(items, "时间", todo_due_chip)
}

fn format_completed_todo_rows(items: &[TodoItem]) -> Vec<String> {
    format_todo_rows_with_time(items, "完成时间", display_todo_completed_at)
}

struct TodoStatusListFormat {
    title: Cow<'static, str>,
    empty_text: Cow<'static, str>,
    time_label: Cow<'static, str>,
    time_value: fn(&TodoItem) -> Option<String>,
    collapse_label: Cow<'static, str>,
    collapse_command: Cow<'static, str>,
}

fn format_todo_status_list_reply(
    items: &[TodoItem],
    spec: TodoStatusListFormat,
    force_full: bool,
) -> CommandBody {
    if items.is_empty() {
        return simple_todo_notice(&spec.empty_text);
    }
    let shown = visible_todo_items(items, force_full);
    // 单状态列表复用 `/todo all` 的看板式标题与无状态行格式；编号快照由调用方按
    // 同一 items 顺序写入 session，后续“第一条/第二条”才能精确对应用户刚看到的列表。
    let mut rows = vec![format!("{} · 共 {} 项", spec.title, items.len())];
    rows.extend(format_todo_rows_with_time(
        shown,
        &spec.time_label,
        spec.time_value,
    ));
    append_todo_collapse_hint(
        &mut rows,
        items.len().saturating_sub(shown.len()),
        Some(&spec.collapse_label),
        &spec.collapse_command,
    );
    let mut markdown_rows = vec![format!("# {} · 共 {} 项", spec.title, items.len())];
    markdown_rows.extend(format_todo_rows_markdown_with_time(
        shown,
        &spec.time_label,
        spec.time_value,
    ));
    append_todo_collapse_hint(
        &mut markdown_rows,
        items.len().saturating_sub(shown.len()),
        Some(&spec.collapse_label),
        &spec.collapse_command,
    );
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

pub(crate) fn append_todo_collapse_hint(
    rows: &mut Vec<String>,
    hidden_count: usize,
    _range_label: Option<&str>,
    _command: &str,
) {
    if hidden_count == 0 {
        return;
    }
    rows.push(String::new());
    rows.push(format!(
        "共找到 {} 条待办，当前展示前 {} 条。可以增加时间、状态或关键词条件缩小范围。",
        TODO_LIST_VISIBLE_LIMIT + hidden_count,
        TODO_LIST_VISIBLE_LIMIT
    ));
}

/// 共享查询页的确定性展示。查询层已经应用 limit，这里只根据真实总数提示截断。
pub(crate) fn format_todo_query_page_reply(
    page: &crate::runtime::tools::todo::TodoQueryPage,
    title: &str,
    empty_text: &str,
    condition: &str,
) -> CommandBody {
    if page.total_count == 0 {
        let suffix = if condition.trim().is_empty() {
            String::new()
        } else {
            format!("（筛选条件：{}）", condition.trim())
        };
        return simple_todo_notice(&format!("{empty_text}{suffix}"));
    }
    let mut rows = vec![format!("{title} · 共 {} 项", page.total_count)];
    rows.extend(format_todo_rows(&page.items));
    let mut markdown_rows = vec![format!("# {title} · 共 {} 项", page.total_count)];
    markdown_rows.extend(format_todo_rows_markdown(&page.items, false));
    if page.offset + page.items.len() < page.total_count {
        let hint = format!(
            "共找到 {} 条待办，当前展示前 {} 条。可以增加时间、状态或关键词条件缩小范围。",
            page.total_count,
            page.items.len()
        );
        rows.push(String::new());
        rows.push(hint.clone());
        markdown_rows.push(String::new());
        markdown_rows.push(escape_text(&hint));
    }
    CommandBody::dual(rows.join("\n"), markdown_rows.join("\n"))
}

fn format_todo_all_board_rows(items: &[TodoItem], markdown: bool) -> Vec<String> {
    let groups = [
        (TodoStatus::Pending, "🚧 进行中", "时间"),
        (TodoStatus::Completed, "✅ 已完成", "完成时间"),
    ];
    let mut rows = Vec::new();
    for (status, title, time_label) in groups {
        let group_items = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.status == status)
            .collect::<Vec<_>>();
        if group_items.is_empty() {
            continue;
        }
        if !rows.is_empty() {
            rows.push(String::new());
        }
        rows.push(if markdown {
            format!("## {title}（{} 项）", group_items.len())
        } else {
            format!("{title}（{} 项）", group_items.len())
        });
        rows.extend(format_todo_all_board_item_rows(
            &group_items,
            time_label,
            markdown,
        ));
    }
    rows
}

fn format_todo_all_board_item_rows(
    items: &[(usize, &TodoItem)],
    _time_label: &str,
    markdown: bool,
) -> Vec<String> {
    items
        .iter()
        .map(|(index, item)| {
            let time_text = match item.status {
                TodoStatus::Completed => display_todo_completed_at(item),
                _ => todo_due_chip(item),
            };
            format_todo_natural_list_item(*index, item, time_text, markdown, None)
        })
        .collect()
}

pub(crate) fn format_todo_inline(item: &TodoItem) -> String {
    truncate_chars(&item.title, 80)
}

pub(crate) fn format_todo_pending_delete_waiting_reply(status: &TodoStatus) -> CommandBody {
    let text = match status {
        TodoStatus::Pending => {
            "这条待办仍在等待取消确认。要取消请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。"
        }
        TodoStatus::Completed => {
            "这条待办仍在等待永久删除确认。要永久删除请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。"
        }
    };
    simple_todo_notice(text)
}

pub(crate) fn format_todo_pending_bulk_delete_waiting_reply() -> CommandBody {
    simple_todo_notice(
        "这批待办仍在等待永久删除确认。要永久删除请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不要 / 算了”。",
    )
}

pub(crate) fn format_todo_inline_markdown(item: &TodoItem) -> String {
    format!("**{}**", escape_text(&truncate_chars(&item.title, 80)))
}

pub(crate) fn simple_todo_notice(text: &str) -> CommandBody {
    CommandBody::dual(text.to_owned(), escape_text(text))
}

fn format_todo_rows_markdown(items: &[TodoItem], with_status: bool) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .flat_map(|(index, item)| {
            let time_text = match item.status {
                TodoStatus::Completed => display_todo_completed_at(item),
                _ => todo_due_chip(item),
            };
            let status = with_status
                .then(|| crate::runtime::tools::todo::status::status_cn_short(&item.status));
            vec![format_todo_natural_list_item(
                index, item, time_text, true, status,
            )]
        })
        .collect()
}

fn format_completed_todo_rows_markdown(items: &[TodoItem]) -> Vec<String> {
    format_todo_rows_markdown_with_time(items, "完成时间", display_todo_completed_at)
}

fn format_todo_rows_markdown_with_time(
    items: &[TodoItem],
    _time_label: &str,
    time_value: fn(&TodoItem) -> Option<String>,
) -> Vec<String> {
    items
        .iter()
        .enumerate()
        .flat_map(|(index, item)| {
            vec![format_todo_natural_list_item(
                index,
                item,
                time_value(item),
                true,
                None,
            )]
        })
        .collect()
}

fn display_todo_completed_at(item: &TodoItem) -> Option<String> {
    item.completed_at.as_deref().and_then(todo_timestamp_chip)
}

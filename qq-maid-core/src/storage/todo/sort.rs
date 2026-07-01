//! 待办排序 helper。
//!
//! 排序键的构造与 session 快照及用户可见列表顺序保持一致：
//! - pending 默认按真实待办时间升序（date-only 视为当天 00:00:00，无时间排最后）；
//! - 已完成列表按完成时间降序；
//! - 已取消 / 全部列表按创建时间降序。
//! - `/todo all` 看板按状态分组展示，其中已取消组保留全部列表里的稳定顺序。
//!
//! 改动这里要同步关注 pending / 已完成 / 已取消列表的展示语义。

use std::cmp::Ordering;

use super::{TodoItem, clean_optional};
use crate::util::time_context::is_valid_ymd_date;

/// 按截止时间 + ID 排序待处理事项。
pub(super) fn sort_todos(items: &mut [TodoItem]) {
    items.sort_by(compare_todo_order);
}

/// 按完成时间 + 截止顺序排序已完成事项。
pub(super) fn sort_completed_todos(items: &mut [TodoItem]) {
    items.sort_by(|left, right| {
        completed_todo_sort_key(left)
            .cmp(&completed_todo_sort_key(right))
            .then_with(|| compare_todo_order(left, right))
    });
}

/// 按完成时间降序排序已完成事项。
pub(super) fn sort_completed_todos_desc(items: &mut [TodoItem]) {
    items.sort_by(|left, right| {
        completed_todo_sort_key(right)
            .cmp(&completed_todo_sort_key(left))
            .then_with(|| left.id.cmp(&right.id))
    });
}

/// 按创建时间降序排序所有事项。
pub(super) fn sort_todos_by_created_desc(items: &mut [TodoItem]) {
    items.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
}

/// `/todo all` 看板的可见顺序。
///
/// 这里显式先分组再排序，确保用户看到的编号快照与看板顺序一致；已取消组不引入
/// 新的取消时间语义，只沿用 `list_all` 的创建时间稳定顺序。
pub(super) fn sort_todo_all_board(items: &mut Vec<TodoItem>) {
    let mut pending = Vec::new();
    let mut completed = Vec::new();
    let mut cancelled = Vec::new();

    for item in items.drain(..) {
        match item.status {
            super::TodoStatus::Pending => pending.push(item),
            super::TodoStatus::Completed => completed.push(item),
            super::TodoStatus::Cancelled => cancelled.push(item),
        }
    }

    sort_todos(&mut pending);
    sort_completed_todos_desc(&mut completed);

    items.extend(pending);
    items.extend(completed);
    items.extend(cancelled);
}

/// 比较两个待办事项的排列顺序：有截止时间的排前面，其次按 ID。
///
/// 搜索结果在命中得分相同时也复用该顺序，保证列表稳定可比。
pub(super) fn compare_todo_order(left: &TodoItem, right: &TodoItem) -> Ordering {
    match (todo_due_sort_key(left), todo_due_sort_key(right)) {
        (Some(left_due), Some(right_due)) => left_due
            .cmp(&right_due)
            .then_with(|| compare_todo_id(&left.id, &right.id)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => compare_todo_id(&left.id, &right.id),
    }
}

/// 已完成事项的排序键：(完成时间, ID)。
fn completed_todo_sort_key(item: &TodoItem) -> (String, String) {
    (
        item.completed_at.clone().unwrap_or_default(),
        item.id.clone(),
    )
}

/// `/todo` 默认列表按真实待办时间升序：date-only 视为当天 00:00:00，无时间排最后。
fn todo_due_sort_key(item: &TodoItem) -> Option<String> {
    if let Some(due_at) = item.due_at.as_deref().and_then(clean_optional) {
        return Some(normalize_due_at_sort_key(&due_at));
    }
    if let Some(due_date) = item.due_date.as_deref().and_then(clean_optional) {
        return Some(format!("{due_date} 00:00:00"));
    }
    None
}

/// 规范化截止时间排序键：将纯日期补全为 "YYYY-MM-DD 00:00:00"。
fn normalize_due_at_sort_key(value: &str) -> String {
    let value = value.trim().replace('T', " ");
    if value.len() == 10 && is_valid_ymd_date(&value) {
        format!("{value} 00:00:00")
    } else {
        value
    }
}

/// 按数字 ID 比较两个待办事项，无法解析为数字时按字典序比较。
fn compare_todo_id(left: &str, right: &str) -> Ordering {
    match (left.parse::<u64>(), right.parse::<u64>()) {
        (Ok(left_id), Ok(right_id)) => left_id.cmp(&right_id),
        _ => left.cmp(right),
    }
}

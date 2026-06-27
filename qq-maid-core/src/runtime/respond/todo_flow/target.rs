//! Todo 操作目标解析。
//!
//! 这里只把用户输入解析成最近列表编号或关键词；真正的完成、恢复、
//! 删除和编辑仍由主流程调用 `TodoStore` 执行，避免解析层越过 pending 保护。
//! 用户可见层不再接受真实 ID；内部 ID 只通过最近列表快照映射得到。

use std::collections::HashSet;

use crate::runtime::{
    session::{LastTodoQuery, SessionRecord, now_iso_cn},
    todo::{TodoItem, TodoOwner},
};

use crate::runtime::respond::common::{LAST_QUERY_TTL_SECONDS, query_is_fresh};

use super::format::format_todo_number_usage_reply;

/// 待办操作目标的解析结果：通过最近列表序号或关键词匹配。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TodoTarget {
    /// 最近用户实际看到的 Todo 列表里的可见序号，已映射到内部 ID。
    ListIndex {
        index: usize,
        id: String,
        source_label: String,
        source_condition: String,
    },
    /// 最近列表里没有该序号。
    MissingListIndex {
        index: usize,
        source_label: String,
        source_command: String,
    },
    /// 当前没有可复用的列表快照，或最近快照已经过期。
    ListUnavailable {
        source_label: String,
        source_command: String,
    },
    /// 使用关键词搜索匹配
    Query(String),
}

/// 用户输入编号和最近列表快照解析后的匹配结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TodoNumberResolution {
    pub(super) matched: Vec<(usize, String)>,
    pub(super) missing: Vec<usize>,
}

fn valid_last_pending_todo_query(
    session: &mut SessionRecord,
    owner: &TodoOwner,
) -> Option<LastTodoQuery> {
    let query = session.last_todo_query.clone()?;
    if query.owner_key != owner.key || !matches!(query.query_type.as_str(), "list" | "search") {
        return None;
    }
    if !query_is_fresh(&query.created_at, LAST_QUERY_TTL_SECONDS) {
        session.last_todo_query = None;
        return None;
    }
    Some(query)
}

pub(super) fn valid_last_visible_todo_query(
    session: &mut SessionRecord,
    owner: &TodoOwner,
) -> Option<LastTodoQuery> {
    let query = session.last_todo_query.clone()?;
    if query.owner_key != owner.key || !is_visible_todo_query_type(&query.query_type) {
        return None;
    }
    if !query_is_fresh(&query.created_at, LAST_QUERY_TTL_SECONDS) {
        session.last_todo_query = None;
        return None;
    }
    Some(query)
}

pub(super) fn remember_todo_query(
    session: &mut SessionRecord,
    owner: &TodoOwner,
    query_type: impl Into<String>,
    condition: impl Into<String>,
    items: &[TodoItem],
) {
    session.last_todo_query = Some(LastTodoQuery {
        owner_key: owner.key.clone(),
        query_type: query_type.into(),
        condition: condition.into(),
        result_ids: items.iter().map(|item| item.id.clone()).collect(),
        created_at: now_iso_cn(),
    });
}

pub(super) fn parse_todo_number_list(argument: &str) -> Result<Vec<usize>, String> {
    let mut numbers = Vec::new();
    let mut seen = HashSet::new();
    let mut current = String::new();

    for ch in argument.trim().chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
            continue;
        }
        if ch.is_whitespace() || matches!(ch, ',' | '，') {
            flush_todo_number_token(&mut current, &mut numbers, &mut seen)?;
            continue;
        }
        return Err(format_todo_number_usage_reply());
    }
    flush_todo_number_token(&mut current, &mut numbers, &mut seen)?;

    if numbers.is_empty() {
        return Err(format_todo_number_usage_reply());
    }
    Ok(numbers)
}

fn flush_todo_number_token(
    current: &mut String,
    numbers: &mut Vec<usize>,
    seen: &mut HashSet<usize>,
) -> Result<(), String> {
    if current.is_empty() {
        return Ok(());
    }
    let number = current
        .parse::<usize>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(format_todo_number_usage_reply)?;
    if seen.insert(number) {
        numbers.push(number);
    }
    current.clear();
    Ok(())
}

pub(super) fn resolve_todo_numbers_from_snapshot(
    query: &LastTodoQuery,
    numbers: &[usize],
) -> TodoNumberResolution {
    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for number in numbers {
        if let Some(id) = query
            .result_ids
            .get(number.saturating_sub(1))
            .filter(|_| *number > 0)
        {
            matched.push((*number, id.clone()));
        } else {
            missing.push(*number);
        }
    }
    TodoNumberResolution { matched, missing }
}

pub(super) fn resolve_todo_target(
    session: &mut SessionRecord,
    owner: &TodoOwner,
    target: &str,
    use_latest_visible_snapshot: bool,
) -> TodoTarget {
    let target = target.trim();
    if target.is_empty() {
        return TodoTarget::Query(String::new());
    }
    if !target.chars().all(|ch| ch.is_ascii_digit()) {
        return TodoTarget::Query(target.to_owned());
    }
    // 纯数字参数优先解释为最近列表里的可见序号；若 usize 解析失败，则回退为关键词匹配。
    let Ok(index) = target.parse::<usize>() else {
        return TodoTarget::Query(target.to_owned());
    };
    if use_latest_visible_snapshot {
        let Some(query) = session.last_todo_query.clone().filter(|query| {
            query.owner_key == owner.key && is_visible_todo_query_type(&query.query_type)
        }) else {
            return TodoTarget::ListUnavailable {
                source_label: "待办".to_owned(),
                source_command: "/todo".to_owned(),
            };
        };
        let source_label = todo_query_source_label(&query);
        let source_command = todo_query_source_command(&query);
        if !query_is_fresh(&query.created_at, LAST_QUERY_TTL_SECONDS) {
            session.last_todo_query = None;
            return TodoTarget::ListUnavailable {
                source_label,
                source_command,
            };
        }
        if let Some(id) = query
            .result_ids
            .get(index.saturating_sub(1))
            .filter(|_| index > 0)
        {
            return TodoTarget::ListIndex {
                index,
                id: id.clone(),
                source_label,
                source_condition: format!("{}第 {index} 条", query.condition),
            };
        }
        return TodoTarget::MissingListIndex {
            index,
            source_label,
            source_command,
        };
    }
    if let Some(query) = valid_last_pending_todo_query(session, owner) {
        if let Some(id) = query
            .result_ids
            .get(index.saturating_sub(1))
            .filter(|_| index > 0)
        {
            return TodoTarget::ListIndex {
                index,
                id: id.clone(),
                source_label: todo_query_source_label(&query),
                source_condition: format!("{}第 {index} 条", query.condition),
            };
        }
        return TodoTarget::MissingListIndex {
            index,
            source_label: todo_query_source_label(&query),
            source_command: todo_query_source_command(&query),
        };
    }
    TodoTarget::ListUnavailable {
        source_label: "待办".to_owned(),
        source_command: "/todo".to_owned(),
    }
}

pub(super) fn todo_target_label(target: &TodoTarget) -> String {
    match target {
        TodoTarget::ListIndex { index, .. } | TodoTarget::MissingListIndex { index, .. } => {
            format!("第 {index} 条")
        }
        TodoTarget::ListUnavailable { source_label, .. } => {
            format!("当前{source_label}序号")
        }
        TodoTarget::Query(query) => query.clone(),
    }
}

pub(super) fn todo_query_source_label(query: &LastTodoQuery) -> String {
    match query.query_type.as_str() {
        "list" | "search" => "待办".to_owned(),
        "all" => "全部待办".to_owned(),
        "completed-list" | "completed-time" => "已完成待办".to_owned(),
        "cancelled-list" => "已取消待办".to_owned(),
        _ => "待办".to_owned(),
    }
}

pub(super) fn todo_query_source_command(query: &LastTodoQuery) -> String {
    match query.query_type.as_str() {
        "completed-list" => "/todo done".to_owned(),
        "cancelled-list" => "/todo delete cancelled".to_owned(),
        "all" => "/todo all".to_owned(),
        "search" if !query.condition.trim().is_empty() => {
            format!("/todo search {}", query.condition.trim())
        }
        "completed-time" if !query.condition.trim().is_empty() => {
            format!("/todo {}", query.condition.trim())
        }
        _ => "/todo".to_owned(),
    }
}

fn is_visible_todo_query_type(query_type: &str) -> bool {
    matches!(
        query_type,
        "list" | "search" | "all" | "completed-list" | "completed-time" | "cancelled-list"
    )
}

pub(super) fn is_completed_todo_cleanup_target(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "done" | "completed" | "complete" | "finished" => return true,
        _ => {}
    }
    let compact = trimmed
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    matches!(
        compact.as_str(),
        "已完成" | "全部已完成" | "所有已完成" | "已完成任务" | "已完成待办"
    )
}

pub(super) fn is_cancelled_todo_cleanup_target(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "cancelled" | "canceled" => return true,
        _ => {}
    }
    let compact = trimmed
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    matches!(
        compact.as_str(),
        "已取消" | "全部已取消" | "所有已取消" | "已取消任务" | "已取消待办"
    )
}

pub(super) fn parse_todo_edit_argument(argument: &str) -> Option<(String, String)> {
    let argument = argument.trim();
    if argument.is_empty() {
        return None;
    }
    let mut parts = argument.splitn(2, char::is_whitespace);
    let first = parts.next()?.trim();
    let rest = parts.next().unwrap_or("").trim();
    if !rest.is_empty() && first.chars().all(|ch| ch.is_ascii_digit()) {
        return Some((first.to_owned(), rest.to_owned()));
    }

    for marker in ["改成", "改为", "修改为", "更新为", "调整为"] {
        if let Some(index) = argument.find(marker) {
            let target = argument[..index].trim();
            let edit_text = argument[index..].trim();
            if !target.is_empty() && !edit_text.is_empty() {
                return Some((target.to_owned(), edit_text.to_owned()));
            }
        }
    }

    if !rest.is_empty() {
        return Some((first.to_owned(), rest.to_owned()));
    }
    None
}

pub(super) fn parse_candidate_selection(text: &str) -> Option<usize> {
    text.trim()
        .trim_start_matches('#')
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
}

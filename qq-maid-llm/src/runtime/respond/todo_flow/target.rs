//! Todo 操作目标解析。
//!
//! 这里只把用户输入解析成待办 ID、最近列表编号或关键词；真正的完成、恢复、
//! 删除和编辑仍由主流程调用 `TodoStore` 执行，避免解析层越过 pending 保护。

use std::collections::HashSet;

use crate::runtime::{
    session::{LastTodoQuery, SessionRecord, now_iso_cn},
    todo::{TodoItem, TodoOwner},
};

use crate::runtime::respond::common::{LAST_QUERY_TTL_SECONDS, query_is_fresh};

use super::{
    completed_query::valid_last_completed_todo_index_query, format::format_todo_number_usage_reply,
};

/// 待办操作目标的解析结果：通过 ID、列表序号或关键词匹配。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TodoTarget {
    /// 待办列表中的待办 ID
    PendingId(String),
    /// 已完成列表中的待办 ID，附带来源条件
    CompletedId {
        id: String,
        source_condition: String,
    },
    /// 列表序号超出范围
    MissingListIndex(usize),
    /// 使用关键词搜索匹配
    Query(String),
}

/// 用户输入编号和最近列表快照解析后的匹配结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TodoNumberResolution {
    pub(super) matched: Vec<(usize, String)>,
    pub(super) missing: Vec<usize>,
}

pub(super) fn valid_last_todo_list_query(
    session: &mut SessionRecord,
    owner: &TodoOwner,
) -> Option<LastTodoQuery> {
    let query = session.last_todo_query.clone()?;
    if query.owner_key != owner.key || query.query_type != "list" {
        return None;
    }
    if !query_is_fresh(&query.created_at, LAST_QUERY_TTL_SECONDS) {
        session.last_todo_query = None;
        return None;
    }
    Some(query)
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
    allow_completed_list_index: bool,
) -> TodoTarget {
    let target = target.trim();
    if target.is_empty() {
        return TodoTarget::Query(String::new());
    }
    if is_explicit_todo_id(target) {
        return TodoTarget::PendingId(clean_todo_target_id(target));
    }
    if target.chars().all(|ch| ch.is_ascii_digit()) {
        if let Ok(index) = target.parse::<usize>()
            && let Some(query) = valid_last_pending_todo_query(session, owner)
        {
            if let Some(id) = query
                .result_ids
                .get(index.saturating_sub(1))
                .filter(|_| index > 0)
            {
                return TodoTarget::PendingId(id.clone());
            }
            return TodoTarget::MissingListIndex(index);
        }
        if let Ok(index) = target.parse::<usize>()
            && allow_completed_list_index
            && let Some(query) = valid_last_completed_todo_index_query(session, owner)
        {
            if let Some(id) = query
                .result_ids
                .get(index.saturating_sub(1))
                .filter(|_| index > 0)
            {
                return TodoTarget::CompletedId {
                    id: id.clone(),
                    source_condition: format!("{}第 {index} 条", query.condition),
                };
            }
            return TodoTarget::MissingListIndex(index);
        }
        return TodoTarget::PendingId(target.to_owned());
    }
    TodoTarget::Query(target.to_owned())
}

pub(super) fn todo_target_label(target: &TodoTarget) -> String {
    match target {
        TodoTarget::PendingId(id) => id.clone(),
        TodoTarget::CompletedId { id, .. } => id.clone(),
        TodoTarget::MissingListIndex(index) => index.to_string(),
        TodoTarget::Query(query) => query.clone(),
    }
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

fn is_explicit_todo_id(target: &str) -> bool {
    let target = target.trim();
    (target.starts_with('[') && target.ends_with(']')) || target.starts_with('#')
}

pub(super) fn clean_todo_target_id(target: &str) -> String {
    target
        .trim()
        .trim_matches(&['[', ']', '#', ' ', '\t', '\n', '\r'][..])
        .to_owned()
}

pub(super) fn parse_todo_edit_argument(argument: &str) -> Option<(String, String)> {
    let argument = argument.trim();
    if argument.is_empty() {
        return None;
    }
    let mut parts = argument.splitn(2, char::is_whitespace);
    let first = parts.next()?.trim();
    let rest = parts.next().unwrap_or("").trim();
    if !rest.is_empty()
        && (first.chars().all(|ch| ch.is_ascii_digit())
            || first.starts_with('[')
            || first.starts_with('#'))
    {
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

pub(super) fn parse_todo_index_edit_hint(argument: &str) -> Option<(String, String)> {
    let argument = argument.trim();
    let close_index = argument.find(']')?;
    let id = argument.get(1..close_index)?.trim();
    if !argument.starts_with('[') || id.is_empty() || !id.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let body = argument.get(close_index + 1..)?.trim();
    if body.is_empty() {
        return None;
    }
    Some((id.to_owned(), body.to_owned()))
}

pub(super) fn parse_candidate_selection(text: &str) -> Option<usize> {
    text.trim()
        .trim_start_matches('#')
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
}

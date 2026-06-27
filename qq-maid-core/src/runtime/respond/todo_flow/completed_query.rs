//! 已完成待办的时间条件查询。
//!
//! 完成时间条件依赖统一的北京时间上下文解析，不能在 Todo flow 内另写一套日期规则。

use chrono::NaiveDate;

use crate::runtime::{
    session::{LastTodoQuery, SessionRecord},
    todo::TodoOwner,
};

use crate::{
    runtime::respond::common::{LAST_QUERY_TTL_SECONDS, query_is_fresh},
    util::time_context::{parse_date_boundary_expression, request_time_context},
};

/// 按完成时间筛选已完成待办的查询条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompletedTodoTimeQuery {
    /// 用户输入的原始条件文本（用于回显）
    pub(super) source_condition: String,
    /// 在此日期之前（含当天）完成的待办
    pub(super) completed_before: NaiveDate,
}

pub(super) fn parse_completed_todo_time_query(text: &str) -> Option<CompletedTodoTimeQuery> {
    let source_condition = text.trim().to_owned();
    if source_condition.is_empty() {
        return None;
    }
    let compact = source_condition
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    if !compact.contains("完成") {
        return None;
    }

    let expression = compact.replace("已完成", "").replace("完成", "");
    let time_ctx = request_time_context();
    let boundary = parse_date_boundary_expression(&expression, &time_ctx)?;
    Some(CompletedTodoTimeQuery {
        source_condition,
        completed_before: boundary.before_date,
    })
}

pub(super) fn valid_last_completed_todo_bulk_query(
    session: &mut SessionRecord,
    owner: &TodoOwner,
) -> Option<LastTodoQuery> {
    valid_last_completed_todo_query(session, owner, |query_type| query_type == "completed-time")
}

fn valid_last_completed_todo_query(
    session: &mut SessionRecord,
    owner: &TodoOwner,
    query_type_matches: impl Fn(&str) -> bool,
) -> Option<LastTodoQuery> {
    let query = session.last_todo_query.clone()?;
    if query.owner_key != owner.key || !query_type_matches(&query.query_type) {
        return None;
    }
    if !query_is_fresh(&query.created_at, LAST_QUERY_TTL_SECONDS) {
        session.last_todo_query = None;
        return None;
    }
    Some(query)
}

//! 最近一次用户可见 Todo 查询的结构化重放。
//!
//! Session 只持久化不透明 JSON；status、时间字段和周期类型等 Todo 语义必须留在本域。

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::runtime::session::{LastTodoQuery, SessionRecord};

use super::{
    TODO_QUERY_MAX_LIMIT, TodoListDateField, TodoOwner, TodoQuery, TodoQueryStatus,
    TodoQueryTimeFilter, storage::validate_todo_query,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TodoQueryReplay {
    status: ReplayStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    time: Option<ReplayTimeFilter>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    keyword: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recurring: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReplayStatus {
    Pending,
    Completed,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ReplayTimeFilter {
    DateRange {
        start: String,
        end: String,
        field: ReplayDateField,
    },
    Overdue,
    NoDueDate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReplayDateField {
    Planned,
    CompletedAt,
}

/// 记录用户实际看到的编号，并附上可直接重放的 Todo 查询语义。
pub(crate) fn remember_todo_query_snapshot(
    session: &mut SessionRecord,
    owner: &TodoOwner,
    query_type: impl Into<String>,
    condition: impl Into<String>,
    query: &TodoQuery,
    result_ids: Vec<String>,
) {
    session.remember_last_todo_query(&owner.key, query_type, condition, result_ids);
    if let Some(last_query) = session.last_todo_query.as_mut() {
        last_query.replay_context = todo_query_replay_context(query);
    }
}

pub(super) fn todo_query_replay_context(query: &TodoQuery) -> Option<serde_json::Value> {
    serde_json::to_value(TodoQueryReplay::from(query)).ok()
}

/// 从 session 的不透明上下文恢复 Todo 查询，只覆盖完整结果所需的分页参数。
pub(crate) fn replay_todo_query(last_query: &LastTodoQuery) -> Option<TodoQuery> {
    let replay =
        serde_json::from_value::<TodoQueryReplay>(last_query.replay_context.as_ref()?.clone())
            .ok()?;
    let query = replay.into_query()?;
    validate_todo_query(&query).ok()?;
    Some(query)
}

pub(crate) fn todo_query_type(query: &TodoQuery) -> &'static str {
    match query.status {
        TodoQueryStatus::All => "all",
        TodoQueryStatus::Completed => "completed-list",
        TodoQueryStatus::Pending
            if matches!(query.time, Some(TodoQueryTimeFilter::DateRange { .. })) =>
        {
            "due-date"
        }
        TodoQueryStatus::Pending
            if query.keyword.is_some()
                || query.recurring.is_some()
                || matches!(
                    query.time,
                    Some(TodoQueryTimeFilter::Overdue { .. } | TodoQueryTimeFilter::NoDueDate)
                ) =>
        {
            "search"
        }
        TodoQueryStatus::Pending => "list",
    }
}

impl From<&TodoQuery> for TodoQueryReplay {
    fn from(query: &TodoQuery) -> Self {
        Self {
            status: match query.status {
                TodoQueryStatus::Pending => ReplayStatus::Pending,
                TodoQueryStatus::Completed => ReplayStatus::Completed,
                TodoQueryStatus::All => ReplayStatus::All,
            },
            time: query.time.as_ref().map(|time| match time {
                TodoQueryTimeFilter::DateRange { start, end, field } => {
                    ReplayTimeFilter::DateRange {
                        start: format_date(*start),
                        end: format_date(*end),
                        field: match field {
                            TodoListDateField::Planned => ReplayDateField::Planned,
                            TodoListDateField::CompletedAt => ReplayDateField::CompletedAt,
                        },
                    }
                }
                TodoQueryTimeFilter::Overdue { .. } => ReplayTimeFilter::Overdue,
                TodoQueryTimeFilter::NoDueDate => ReplayTimeFilter::NoDueDate,
            }),
            keyword: query.keyword.clone(),
            recurring: query.recurring,
        }
    }
}

impl TodoQueryReplay {
    fn into_query(self) -> Option<TodoQuery> {
        let time = match self.time {
            Some(ReplayTimeFilter::DateRange { start, end, field }) => {
                Some(TodoQueryTimeFilter::DateRange {
                    start: parse_date(&start)?,
                    end: parse_date(&end)?,
                    field: match field {
                        ReplayDateField::Planned => TodoListDateField::Planned,
                        ReplayDateField::CompletedAt => TodoListDateField::CompletedAt,
                    },
                })
            }
            Some(ReplayTimeFilter::Overdue) => Some(TodoQueryTimeFilter::Overdue {
                now: qq_maid_common::time_context::parse_local_datetime_for_comparison(
                    qq_maid_common::time_context::request_time_context().current_time(),
                )
                .expect("request time context must contain a valid local datetime"),
            }),
            Some(ReplayTimeFilter::NoDueDate) => Some(TodoQueryTimeFilter::NoDueDate),
            None => None,
        };
        Some(TodoQuery {
            status: match self.status {
                ReplayStatus::Pending => TodoQueryStatus::Pending,
                ReplayStatus::Completed => TodoQueryStatus::Completed,
                ReplayStatus::All => TodoQueryStatus::All,
            },
            time,
            keyword: self.keyword,
            recurring: self.recurring,
            limit: TODO_QUERY_MAX_LIMIT,
            offset: 0,
        })
    }
}

fn format_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

fn parse_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    #[test]
    fn replay_round_trip_preserves_combination_filters_and_replaces_pagination() {
        let query = TodoQuery {
            status: TodoQueryStatus::Completed,
            time: Some(TodoQueryTimeFilter::DateRange {
                start: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 7, 20).unwrap(),
                field: TodoListDateField::CompletedAt,
            }),
            keyword: Some("项目 A".to_owned()),
            recurring: Some(true),
            limit: 3,
            offset: 6,
        };
        let last_query = LastTodoQuery {
            owner_key: "u1".to_owned(),
            query_type: "completed-list".to_owned(),
            condition: "组合条件".to_owned(),
            replay_context: Some(serde_json::to_value(TodoQueryReplay::from(&query)).unwrap()),
            result_ids: Vec::new(),
            created_at: String::new(),
        };

        let restored = replay_todo_query(&last_query).unwrap();
        assert_eq!(restored.status, query.status);
        assert_eq!(restored.time, query.time);
        assert_eq!(restored.keyword, query.keyword);
        assert_eq!(restored.recurring, query.recurring);
        assert_eq!(restored.limit, TODO_QUERY_MAX_LIMIT);
        assert_eq!(restored.offset, 0);
    }
}

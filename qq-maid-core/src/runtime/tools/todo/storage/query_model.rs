//! 用户主动 Todo 查询的共享 SQL 模型。
//!
//! Slash、确定性自然语言和 Tool 都必须走这里。总数和当前页分别通过 COUNT 与
//! LIMIT/OFFSET 获取，不把 owner 下的全部 Todo 加载到内存后再截断。

use rusqlite::{params_from_iter, types::Value as SqlValue};

use super::{
    TODO_QUERY_MAX_LIMIT, TodoError, TodoListDateField, TodoQuery, TodoQueryPage, TodoQueryStatus,
    TodoQueryTimeFilter, TodoRecurrenceKind, TodoStatus, TodoStore, query::todo_item_from_row,
};

const SELECT_COLUMNS: &str = "id, user_id, scope_key, title, detail, raw_text,
    due_date, due_at, reminder_at, time_precision, recurrence_kind,
    recurrence_interval_days, recurrence_interval, recurrence_unit, status,
    created_at, updated_at, completed_at";

impl TodoStore {
    /// 执行共享 Todo 查询并返回真实总数与当前页。
    pub fn query_todos(
        &self,
        owner: &super::TodoOwner,
        query: &TodoQuery,
    ) -> Result<TodoQueryPage, TodoError> {
        validate_todo_query(query)?;
        let conn = self.connection()?;
        let (where_sql, params) = build_where(owner, query);
        let count_sql = format!("SELECT COUNT(*) FROM todos WHERE {where_sql}");
        let total_count = conn
            .query_row(&count_sql, params_from_iter(params.iter()), |row| {
                row.get::<_, i64>(0)
            })
            .map_err(TodoError::from_sql)?
            .try_into()
            .map_err(|_| TodoError::data("todo count overflow"))?;

        let limit = query.limit.min(TODO_QUERY_MAX_LIMIT);
        let order_sql = order_by_sql(query.status);
        let page_sql = format!(
            "SELECT {SELECT_COLUMNS} FROM todos WHERE {where_sql} {order_sql} LIMIT {limit} OFFSET {}",
            query.offset
        );
        let mut stmt = conn.prepare(&page_sql).map_err(TodoError::from_sql)?;
        let items = stmt
            .query_map(params_from_iter(params.iter()), todo_item_from_row)
            .map_err(TodoError::from_sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(TodoError::from_sql)?;

        Ok(TodoQueryPage {
            items,
            total_count,
            limit,
            offset: query.offset,
        })
    }
}

/// 校验 Slash、自然语言与 Tool 共用的 Todo 查询组合语义。
pub(crate) fn validate_todo_query(query: &TodoQuery) -> Result<(), TodoError> {
    if query.limit == 0 {
        return Err(TodoError::bad_request("limit 必须大于 0。"));
    }
    if let Some(TodoQueryTimeFilter::DateRange { start, end, .. }) = query.time
        && start > end
    {
        return Err(TodoError::bad_request(
            "日期范围无效，开始日期不能晚于结束日期。",
        ));
    }
    if matches!(query.time, Some(TodoQueryTimeFilter::Overdue { .. }))
        && !matches!(query.status, TodoQueryStatus::Pending)
    {
        return Err(TodoError::bad_request("逾期筛选只适用于未完成待办。"));
    }
    Ok(())
}

fn build_where(owner: &super::TodoOwner, query: &TodoQuery) -> (String, Vec<SqlValue>) {
    let mut clauses = vec!["owner_key = ?".to_owned(), "scope_key = ?".to_owned()];
    let mut params = vec![
        SqlValue::Text(owner.key.clone()),
        SqlValue::Text(owner.scope_key.clone()),
    ];
    match query.status {
        TodoQueryStatus::Pending => {
            clauses.push("status = ?".to_owned());
            params.push(SqlValue::Text(TodoStatus::Pending.as_str().to_owned()));
        }
        TodoQueryStatus::Completed => {
            clauses.push("status = ?".to_owned());
            params.push(SqlValue::Text(TodoStatus::Completed.as_str().to_owned()));
        }
        TodoQueryStatus::All => {}
    }
    if let Some(time) = &query.time {
        append_time_filter(&mut clauses, &mut params, time);
    }
    if let Some(recurring) = query.recurring {
        clauses.push(if recurring {
            "recurrence_kind <> ?".to_owned()
        } else {
            "recurrence_kind = ?".to_owned()
        });
        params.push(SqlValue::Text(TodoRecurrenceKind::None.as_str().to_owned()));
    }
    if let Some(keyword) = query
        .keyword
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        for token in keyword.split_whitespace() {
            clauses.push(
                "LOWER(COALESCE(title, '') || CHAR(10) || COALESCE(detail, '') || CHAR(10) || COALESCE(raw_text, '')) LIKE ? ESCAPE '\\'"
                    .to_owned(),
            );
            params.push(SqlValue::Text(format!(
                "%{}%",
                escape_like(token).to_lowercase()
            )));
        }
    }
    (clauses.join(" AND "), params)
}

fn append_time_filter(
    clauses: &mut Vec<String>,
    params: &mut Vec<SqlValue>,
    filter: &TodoQueryTimeFilter,
) {
    match filter {
        TodoQueryTimeFilter::DateRange { start, end, field } => {
            let expression = match field {
                TodoListDateField::Planned => planned_local_date_sql(),
                TodoListDateField::CompletedAt => local_date_sql("completed_at"),
            };
            clauses.push(format!("{expression} BETWEEN ? AND ?"));
            params.push(SqlValue::Text(start.format("%Y-%m-%d").to_string()));
            params.push(SqlValue::Text(end.format("%Y-%m-%d").to_string()));
        }
        TodoQueryTimeFilter::NoDueDate => clauses.push(
            "NULLIF(TRIM(COALESCE(due_at, '')), '') IS NULL AND NULLIF(TRIM(COALESCE(due_date, '')), '') IS NULL"
                .to_owned(),
        ),
        TodoQueryTimeFilter::Overdue { now } => {
            clauses.push("status = 'pending'".to_owned());
            clauses.push(format!(
                "((NULLIF(TRIM(COALESCE(due_at, '')), '') IS NOT NULL AND {} < ?) OR (NULLIF(TRIM(COALESCE(due_at, '')), '') IS NULL AND NULLIF(TRIM(COALESCE(due_date, '')), '') IS NOT NULL AND due_date < ?))",
                local_datetime_sql("due_at")
            ));
            params.push(SqlValue::Text(now.format("%Y-%m-%d %H:%M:%S").to_string()));
            params.push(SqlValue::Text(now.format("%Y-%m-%d").to_string()));
        }
    }
}

fn order_by_sql(status: TodoQueryStatus) -> String {
    let due = planned_local_datetime_sql();
    let completed = local_datetime_sql("completed_at");
    match status {
        TodoQueryStatus::Pending => format!(
            "ORDER BY CASE WHEN {due} IS NULL THEN 1 ELSE 0 END ASC, {due} ASC, CAST(id AS INTEGER) ASC"
        ),
        TodoQueryStatus::Completed => format!("ORDER BY {completed} DESC, CAST(id AS INTEGER) ASC"),
        TodoQueryStatus::All => format!(
            "ORDER BY CASE status WHEN 'pending' THEN 0 ELSE 1 END ASC, CASE WHEN status = 'pending' AND {due} IS NULL THEN 1 ELSE 0 END ASC, CASE WHEN status = 'pending' THEN {due} END ASC, CASE WHEN status = 'completed' THEN {completed} END DESC, CAST(id AS INTEGER) ASC"
        ),
    }
}

fn planned_local_date_sql() -> String {
    format!(
        "CASE WHEN NULLIF(TRIM(COALESCE(due_at, '')), '') IS NOT NULL THEN {} ELSE NULLIF(TRIM(COALESCE(due_date, '')), '') END",
        local_date_sql("due_at")
    )
}

fn planned_local_datetime_sql() -> String {
    format!(
        "CASE WHEN NULLIF(TRIM(COALESCE(due_at, '')), '') IS NOT NULL THEN {} WHEN NULLIF(TRIM(COALESCE(due_date, '')), '') IS NOT NULL THEN due_date || ' 00:00:00' ELSE NULL END",
        local_datetime_sql("due_at")
    )
}

fn local_date_sql(column: &str) -> String {
    format!("SUBSTR({}, 1, 10)", local_datetime_sql(column))
}

fn local_datetime_sql(column: &str) -> String {
    format!(
        "CASE WHEN {column} IS NULL OR TRIM({column}) = '' THEN NULL WHEN {column} GLOB '*[+-][0-9][0-9]:[0-9][0-9]' OR UPPER({column}) GLOB '*Z' THEN STRFTIME('%Y-%m-%d %H:%M:%S', {column}, '+8 hours') ELSE REPLACE(SUBSTR({column}, 1, 19), 'T', ' ') END"
    )
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

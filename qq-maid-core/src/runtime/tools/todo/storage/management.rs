//! 部署管理员 Todo 场景的专用持久化入口。
//!
//! 普通聊天仍只能调用 owner-scoped Repository 方法；本模块只向 Todo 管理 Service
//! 暴露全局读取，以及 Todo 与 Notification Outbox 的同库事务写入。

use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value as SqlValue};

use crate::{
    runtime::tools::todo::reminder::TODO_REMINDER_SOURCE,
    storage::{
        notification::{NotificationUpsert, cancel_by_source_on_connection, upsert_on_connection},
        session::now_iso_cn,
    },
};

use super::{
    TodoError, TodoItemDraft, TodoManagementRecord, TodoManagementScopeType,
    TodoManagementTargetCandidateFilter, TodoManagementTargetCandidatePage, TodoOwner, TodoStatus,
    TodoStore,
    id::{clean_todo_id, parse_todo_db_id},
    normalize::normalize_draft,
    query::todo_item_from_row,
    write::insert_todo_unlocked,
};

const MANAGEMENT_SELECT: &str = "id, user_id, scope_key, title, detail, raw_text,
    due_date, due_at, reminder_at, time_precision, recurrence_kind,
    recurrence_interval_days, recurrence_interval, recurrence_unit, status,
    created_at, updated_at, completed_at, owner_key";

const MANAGEMENT_TARGET_QUERY_MAX_LIMIT: usize = 100;

/// 先把 Todo 事实记录和可完整恢复的 Session 规整成相同的真实 owner/scope 行。
/// Session 的群聊候选只接受 actor interaction session；共享 conversation session
/// 不能可靠代表具体成员，不能据此为某个成员创建 Todo。
const MANAGEMENT_TARGET_CANDIDATES_CTE: &str = r#"
WITH session_group_candidates(session_scope, user_id, group_id) AS (
    SELECT TRIM(scope_key), TRIM(user_id), TRIM(group_id)
    FROM sessions
    WHERE NULLIF(TRIM(user_id), '') IS NOT NULL
      AND NULLIF(TRIM(group_id), '') IS NOT NULL
      AND SUBSTR(
            TRIM(scope_key),
            -LENGTH(':actor:' || TRIM(user_id))
          ) = ':actor:' || TRIM(user_id)
),
raw_candidates(owner_key, user_id, scope_key) AS (
    SELECT owner_key, NULLIF(TRIM(user_id), ''), TRIM(scope_key)
    FROM todos

    UNION

    SELECT
        CASE
            WHEN TRIM(scope_key) GLOB 'platform:?*:account:?*:private:?*'
                THEN TRIM(scope_key) || ':actor:' || TRIM(user_id)
            ELSE TRIM(user_id)
        END,
        TRIM(user_id),
        TRIM(scope_key)
    FROM sessions
    WHERE NULLIF(TRIM(user_id), '') IS NOT NULL
      AND (
            (
                TRIM(scope_key) GLOB 'platform:?*:account:?*:private:?*'
                AND SUBSTR(
                        TRIM(scope_key),
                        -(LENGTH(TRIM(user_id)) + LENGTH(':private:'))
                    ) = ':private:' || TRIM(user_id)
            )
            OR TRIM(scope_key) = 'private:' || TRIM(user_id)
          )

    UNION

    SELECT
        session_scope,
        user_id,
        SUBSTR(
            session_scope,
            1,
            LENGTH(session_scope) - LENGTH(':actor:' || user_id)
        )
    FROM session_group_candidates
    WHERE (
            SUBSTR(
                session_scope,
                1,
                LENGTH(session_scope) - LENGTH(':actor:' || user_id)
            ) = 'group:' || group_id
          )
       OR (
            SUBSTR(
                session_scope,
                1,
                LENGTH(session_scope) - LENGTH(':actor:' || user_id)
            ) GLOB 'platform:?*:account:?*:group:?*'
            AND SUBSTR(
                    SUBSTR(
                        session_scope,
                        1,
                        LENGTH(session_scope) - LENGTH(':actor:' || user_id)
                    ),
                    -(LENGTH(group_id) + LENGTH(':group:'))
                ) = ':group:' || group_id
          )
),
candidates(owner_key, user_id, scope_key) AS (
    SELECT DISTINCT owner_key, user_id, scope_key
    FROM raw_candidates
    WHERE NULLIF(TRIM(owner_key), '') IS NOT NULL
      AND NULLIF(TRIM(scope_key), '') IS NOT NULL
      AND owner_key = CASE
            WHEN user_id IS NULL THEN scope_key
            WHEN scope_key GLOB 'platform:?*:account:?*:*:?*'
                THEN scope_key || ':actor:' || user_id
            ELSE user_id
          END
      AND (
            (
                scope_key GLOB 'platform:?*:account:?*:private:?*'
                AND user_id IS NOT NULL
                AND SUBSTR(
                        scope_key,
                        -(LENGTH(user_id) + LENGTH(':private:'))
                    ) = ':private:' || user_id
            )
            OR scope_key = 'private:' || user_id
            OR scope_key GLOB 'platform:?*:account:?*:group:?*'
            OR scope_key GLOB 'group:?*'
          )
)
"#;

impl TodoStore {
    /// 按内部 ID 全局读取 Todo，仅供已通过部署管理员鉴权的领域 Service。
    pub(crate) fn get_todo_for_management(
        &self,
        id: &str,
    ) -> Result<Option<TodoManagementRecord>, TodoError> {
        let Some(id) = parse_todo_db_id(&clean_todo_id(id)) else {
            return Ok(None);
        };
        let conn = self.connection()?;
        get_management_record_unlocked(&conn, id)
    }

    /// 分页返回服务端已经见过的真实 Todo 目标。Todo 记录是事实来源；Session
    /// 只补充已发生过聊天但尚无 Todo 的私聊或群成员交互目标。
    pub(crate) fn management_target_candidates_page(
        &self,
        filter: &TodoManagementTargetCandidateFilter,
        limit: usize,
        offset: usize,
    ) -> Result<TodoManagementTargetCandidatePage, TodoError> {
        if limit == 0 {
            return Err(TodoError::bad_request(
                "target page size must be greater than 0",
            ));
        }
        let conn = self.connection()?;
        let (where_sql, params) = management_target_where(filter);
        let count_sql = format!(
            "{MANAGEMENT_TARGET_CANDIDATES_CTE}\nSELECT COUNT(*) FROM candidates WHERE {where_sql}"
        );
        let total_count = conn
            .query_row(&count_sql, params_from_iter(params.iter()), |row| {
                row.get::<_, i64>(0)
            })
            .map_err(TodoError::from_sql)?
            .try_into()
            .map_err(|_| TodoError::data("target candidate count overflow"))?;

        let limit = limit.min(MANAGEMENT_TARGET_QUERY_MAX_LIMIT);
        let page_sql = format!(
            "{MANAGEMENT_TARGET_CANDIDATES_CTE}
             SELECT owner_key, user_id, scope_key
             FROM candidates
             WHERE {where_sql}
             ORDER BY scope_key ASC, user_id ASC, owner_key ASC
             LIMIT {limit} OFFSET {offset}"
        );
        let mut stmt = conn.prepare(&page_sql).map_err(TodoError::from_sql)?;
        let items = stmt
            .query_map(params_from_iter(params.iter()), |row| {
                Ok(TodoOwner {
                    key: row.get(0)?,
                    user_id: row.get(1)?,
                    scope_key: row.get(2)?,
                })
            })
            .map_err(TodoError::from_sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(TodoError::from_sql)?;

        Ok(TodoManagementTargetCandidatePage {
            items,
            total_count,
            limit,
            offset,
        })
    }

    /// 管理创建与 reminder Outbox 在同一个事务内提交。
    pub(crate) fn create_todo_for_management<F>(
        &self,
        owner: &TodoOwner,
        draft: TodoItemDraft,
        notification: F,
    ) -> Result<TodoManagementRecord, TodoError>
    where
        F: FnOnce(&TodoManagementRecord) -> Result<Option<NotificationUpsert>, TodoError>,
    {
        let draft = normalize_draft(draft)?;
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let item = insert_todo_unlocked(&tx, owner, draft, &now_iso_cn())?;
        let record = TodoManagementRecord {
            owner: owner.clone(),
            item,
        };
        if let Some(request) = notification(&record)? {
            upsert_on_connection(&tx, &request).map_err(notification_error)?;
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(record)
    }

    /// 把字段、状态/周期推进和 reminder Outbox 一次性写入同一个 SQLite 事务。
    /// `expected` 是 Service 在完成所有前置校验时读取的快照；事务内若发现并发变化，
    /// 返回 conflict，不用旧计划覆盖聊天入口刚写入的新状态。
    pub(crate) fn update_todo_for_management<F>(
        &self,
        expected: &TodoManagementRecord,
        draft: TodoItemDraft,
        final_status: TodoStatus,
        notification: F,
    ) -> Result<TodoManagementRecord, TodoError>
    where
        F: FnOnce(&TodoManagementRecord) -> Result<Option<NotificationUpsert>, TodoError>,
    {
        let draft = normalize_draft(draft)?;
        let id = parse_todo_db_id(&expected.item.id)
            .ok_or_else(|| TodoError::bad_request("invalid todo id"))?;
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let current = get_management_record_unlocked(&tx, id)?
            .ok_or_else(|| TodoError::not_found("todo not found"))?;
        if current != *expected {
            return Err(TodoError::conflict("todo changed during management update"));
        }
        let now = now_iso_cn();
        let completed_at = matches!(final_status, TodoStatus::Completed).then_some(now.as_str());
        tx.execute(
            "UPDATE todos
             SET title = ?4,
                 detail = ?5,
                 raw_text = ?6,
                 due_date = ?7,
                 due_at = ?8,
                 reminder_at = ?9,
                 time_precision = ?10,
                 recurrence_kind = ?11,
                 recurrence_interval_days = ?12,
                 recurrence_interval = ?13,
                 recurrence_unit = ?14,
                 status = ?15,
                 completed = ?16,
                 updated_at = ?17,
                 completed_at = ?18
             WHERE id = ?1 AND owner_key = ?2 AND scope_key = ?3",
            params![
                id,
                expected.owner.key.as_str(),
                expected.owner.scope_key.as_str(),
                draft.title,
                draft.detail,
                draft.raw_text,
                draft.due_date,
                draft.due_at,
                draft.reminder_at,
                draft.time_precision.as_str(),
                draft.recurrence_kind.as_str(),
                i64::from(draft.recurrence_interval_days),
                i64::from(draft.recurrence_interval),
                draft.recurrence_unit.as_str(),
                final_status.as_str(),
                i64::from(matches!(final_status, TodoStatus::Completed)),
                now,
                completed_at,
            ],
        )
        .map_err(TodoError::from_sql)?;
        let updated = get_management_record_unlocked(&tx, id)?
            .ok_or_else(|| TodoError::io("todo disappeared after management update"))?;
        cancel_by_source_on_connection(&tx, TODO_REMINDER_SOURCE, &updated.item.id)
            .map_err(notification_error)?;
        if let Some(request) = notification(&updated)? {
            upsert_on_connection(&tx, &request).map_err(notification_error)?;
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(updated)
    }

    /// 管理删除仍使用真实 owner/scope，并与 reminder 取消原子提交。
    pub(crate) fn delete_todo_for_management(
        &self,
        expected: &TodoManagementRecord,
    ) -> Result<(), TodoError> {
        let id = parse_todo_db_id(&expected.item.id)
            .ok_or_else(|| TodoError::bad_request("invalid todo id"))?;
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let current = get_management_record_unlocked(&tx, id)?
            .ok_or_else(|| TodoError::not_found("todo not found"))?;
        if current != *expected {
            return Err(TodoError::conflict("todo changed during management delete"));
        }
        cancel_by_source_on_connection(&tx, TODO_REMINDER_SOURCE, &current.item.id)
            .map_err(notification_error)?;
        let deleted = tx
            .execute(
                "DELETE FROM todos WHERE id = ?1 AND owner_key = ?2 AND scope_key = ?3",
                params![
                    id,
                    current.owner.key.as_str(),
                    current.owner.scope_key.as_str()
                ],
            )
            .map_err(TodoError::from_sql)?;
        if deleted != 1 {
            return Err(TodoError::conflict("todo changed during management delete"));
        }
        tx.commit().map_err(TodoError::from_sql)
    }
}

fn get_management_record_unlocked(
    conn: &Connection,
    id: i64,
) -> Result<Option<TodoManagementRecord>, TodoError> {
    conn.query_row(
        &format!("SELECT {MANAGEMENT_SELECT} FROM todos WHERE id = ?1"),
        [id],
        |row| {
            let item = todo_item_from_row(row)?;
            Ok(TodoManagementRecord {
                owner: TodoOwner {
                    key: row.get(18)?,
                    user_id: item.user_id.clone(),
                    scope_key: item.scope_key.clone(),
                },
                item,
            })
        },
    )
    .optional()
    .map_err(TodoError::from_sql)
}

fn management_target_where(
    filter: &TodoManagementTargetCandidateFilter,
) -> (String, Vec<SqlValue>) {
    let mut clauses = vec!["1 = 1".to_owned()];
    let mut params = Vec::new();
    if let Some(platform) = &filter.platform {
        let stable_prefix = format!("platform:{}:account:%", escape_like(platform));
        if platform == "qq_official" {
            clauses.push(
                "(scope_key LIKE ? ESCAPE '\\' OR scope_key LIKE 'private:%' OR scope_key LIKE 'group:%')"
                    .to_owned(),
            );
        } else {
            clauses.push("scope_key LIKE ? ESCAPE '\\'".to_owned());
        }
        params.push(SqlValue::Text(stable_prefix));
    }
    if let Some(account_id) = &filter.account_id {
        clauses.push("scope_key LIKE ? ESCAPE '\\'".to_owned());
        params.push(SqlValue::Text(format!(
            "platform:%:account:{}:%",
            escape_like(account_id)
        )));
    }
    if let Some(scope_type) = filter.scope_type {
        let (stable_type, legacy_type) = match scope_type {
            TodoManagementScopeType::Private => ("private", "private:%"),
            TodoManagementScopeType::Group => ("group", "group:%"),
        };
        clauses.push(format!(
            "(scope_key LIKE 'platform:%:account:%:{stable_type}:%' OR scope_key LIKE '{legacy_type}')"
        ));
    }
    if let Some(user_id) = &filter.user_id {
        clauses.push("user_id = ?".to_owned());
        params.push(SqlValue::Text(user_id.clone()));
    }
    if let Some(group_id) = &filter.group_id {
        clauses.push("(scope_key = ? OR scope_key LIKE ? ESCAPE '\\')".to_owned());
        params.push(SqlValue::Text(format!("group:{group_id}")));
        params.push(SqlValue::Text(format!("%:group:{}", escape_like(group_id))));
    }
    (clauses.join(" AND "), params)
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn notification_error(error: crate::storage::notification::NotificationError) -> TodoError {
    TodoError::notification(error.message())
}

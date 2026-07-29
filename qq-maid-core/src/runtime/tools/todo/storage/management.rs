//! 部署管理员 Todo 场景的专用持久化入口。
//!
//! 普通聊天仍只能调用 owner-scoped Repository 方法；本模块只向 Todo 管理 Service
//! 暴露全局读取，以及 Todo 与 Notification Outbox 的同库事务写入。

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    identity::{group_raw_target_from_scope_key, private_raw_target_from_scope_key},
    runtime::tools::todo::reminder::TODO_REMINDER_SOURCE,
    storage::{
        notification::{NotificationUpsert, cancel_by_source_on_connection, upsert_on_connection},
        session::now_iso_cn,
    },
};

use super::{
    TodoError, TodoItemDraft, TodoManagementRecord, TodoOwner, TodoStatus, TodoStore,
    id::{clean_todo_id, parse_todo_db_id},
    normalize::normalize_draft,
    query::todo_item_from_row,
    write::insert_todo_unlocked,
};

const MANAGEMENT_SELECT: &str = "id, user_id, scope_key, title, detail, raw_text,
    due_date, due_at, reminder_at, time_precision, recurrence_kind,
    recurrence_interval_days, recurrence_interval, recurrence_unit, status,
    created_at, updated_at, completed_at, owner_key";

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

    /// 返回服务端已经见过的真实 Todo 目标。Todo 记录是事实来源；Session 只补充
    /// 已发生过聊天但尚无 Todo 的私聊或群成员交互目标。
    pub(crate) fn management_target_candidates(&self) -> Result<Vec<TodoOwner>, TodoError> {
        let conn = self.connection()?;
        let mut targets = BTreeMap::<(String, Option<String>, String), TodoOwner>::new();
        {
            let mut stmt = conn
                .prepare("SELECT DISTINCT owner_key, user_id, scope_key FROM todos")
                .map_err(TodoError::from_sql)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(TodoOwner {
                        key: row.get(0)?,
                        user_id: row.get(1)?,
                        scope_key: row.get(2)?,
                    })
                })
                .map_err(TodoError::from_sql)?;
            for target in rows {
                insert_target(&mut targets, target.map_err(TodoError::from_sql)?);
            }
        }

        let mut stmt = conn
            .prepare("SELECT DISTINCT scope_key, user_id, group_id FROM sessions")
            .map_err(TodoError::from_sql)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(TodoError::from_sql)?;
        for row in rows {
            let (session_scope, user_id, group_id) = row.map_err(TodoError::from_sql)?;
            if let Some(target) = target_from_session(&session_scope, user_id, group_id) {
                insert_target(&mut targets, target);
            }
        }
        Ok(targets.into_values().collect())
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

fn target_from_session(
    session_scope: &str,
    user_id: Option<String>,
    group_id: Option<String>,
) -> Option<TodoOwner> {
    let user_id = user_id.filter(|value| !value.trim().is_empty())?;
    if private_raw_target_from_scope_key(session_scope).is_some() {
        return Some(TodoStore::owner(Some(&user_id), session_scope));
    }
    let actor_suffix = format!(":actor:{user_id}");
    let conversation_scope = session_scope.strip_suffix(&actor_suffix)?;
    let raw_group = group_raw_target_from_scope_key(conversation_scope)?;
    if group_id.as_deref().map(str::trim) != Some(raw_group.as_str()) {
        return None;
    }
    let owner = TodoStore::owner(Some(&user_id), conversation_scope);
    (owner.key == session_scope).then_some(owner)
}

fn insert_target(
    targets: &mut BTreeMap<(String, Option<String>, String), TodoOwner>,
    target: TodoOwner,
) {
    targets
        .entry((
            target.key.clone(),
            target.user_id.clone(),
            target.scope_key.clone(),
        ))
        .or_insert(target);
}

fn notification_error(error: crate::storage::notification::NotificationError) -> TodoError {
    TodoError::notification(error.message())
}

//! 待办事项（Todo）存储模块。
//!
//! 以项目级 SQLite 存储待办事项列表，支持创建、列表（按状态和条件）、
//! 搜索、编辑、完成/取消等操作。支持中文自然语言日期推断。

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

use chrono::NaiveDate;
use qq_maid_common::text::truncate_chars_trimmed as truncate_chars;
use rusqlite::{Connection, OptionalExtension, params, types::Type};
use serde::{Deserialize, Serialize};

use crate::{
    storage::{
        database::{DatabaseError, SqliteDatabase, SqliteMigration},
        session::{now_iso_cn, redact_sensitive_text},
    },
    util::time_context::{
        self, DateInferencePrecision, RequestTimeContext, format_todo_time_for_display,
        has_valid_ymd_date_prefix, is_valid_ymd_date, local_date_from_timestamp,
    },
};

/// Todo schema migration，由应用启动时的通用数据库初始化流程统一执行。
///
/// Todo 使用 SQLite 自增整数作为稳定内部 ID；运行时结构仍以字符串展示 ID，
/// 是为了保持 session 快照、pending 序列化和用户可见 `[id]` 格式稳定。
pub const TODO_SCHEMA_V1: SqliteMigration = SqliteMigration {
    name: "todo_schema_v1",
    sql: "CREATE TABLE IF NOT EXISTS todos (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            owner_key TEXT NOT NULL,
            user_id TEXT,
            scope_key TEXT NOT NULL,
            title TEXT NOT NULL,
            detail TEXT,
            raw_text TEXT,
            due_date TEXT,
            due_at TEXT,
            time_precision TEXT NOT NULL DEFAULT 'none',
            status TEXT NOT NULL DEFAULT 'pending',
            completed INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            completed_at TEXT,
            cancelled_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_todos_owner_status
            ON todos(owner_key, scope_key, status);
        CREATE INDEX IF NOT EXISTS idx_todos_owner_due
            ON todos(owner_key, scope_key, due_at, due_date, id);
        CREATE INDEX IF NOT EXISTS idx_todos_owner_created
            ON todos(owner_key, scope_key, created_at, id);
        CREATE INDEX IF NOT EXISTS idx_todos_owner_completed
            ON todos(owner_key, scope_key, completed_at, id);",
};

pub const TODO_MIGRATIONS: &[SqliteMigration] = &[TODO_SCHEMA_V1];

/// 待办事项的状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    Completed,
    Cancelled,
}

/// 待办事项的时间精度。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoTimePrecision {
    #[default]
    None,
    Date,
    DateTime,
    Inferred,
}

/// 待办事项条目，包含标题、详情、截止时间和状态等完整信息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default)]
    pub scope_key: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    #[serde(default)]
    pub time_precision: TodoTimePrecision,
    #[serde(default)]
    pub status: TodoStatus,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<String>,
}

/// 待办事项草稿，用于创建或编辑操作。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItemDraft {
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    #[serde(default)]
    pub time_precision: TodoTimePrecision,
}

/// 待办事项所有者标识。
///
/// 作用域键（scope_key）确定归属范围，用户 ID 可选；
/// key 用于隔离 Todo 归属，优先使用用户 ID，其次用 scope_key。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoOwner {
    pub key: String,
    pub user_id: Option<String>,
    pub scope_key: String,
}

/// 待办事项存储器，基于项目通用 SQLite 连接实现。
///
/// 数据库连接由应用启动时统一打开并执行 migration；TodoStore 只接收已初始化句柄，
/// 不自行读取数据库路径，也不在业务方法中建表。
#[derive(Debug, Clone)]
pub struct TodoStore {
    database: SqliteDatabase,
}

/// 批量取消已完成的待办事项的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoBulkCancelOutcome {
    pub cancelled: Vec<TodoItem>,
    pub skipped_ids: Vec<String>,
}

/// 批量完成待办事项的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoBulkCompleteOutcome {
    pub completed: Vec<TodoItem>,
    pub skipped_ids: Vec<String>,
}

/// 批量恢复已完成待办事项的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoBulkRestoreOutcome {
    pub restored: Vec<TodoItem>,
    pub skipped_ids: Vec<String>,
}

/// 批量物理删除已取消待办事项的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoBulkDeleteOutcome {
    pub deleted_count: usize,
    pub skipped_ids: Vec<String>,
}

/// Todo reminder 可安全使用的私聊 owner 候选。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoReminderOwnerCandidate {
    pub owner_key: String,
    pub private_target_id: String,
    pub private_scope_keys: Vec<String>,
}

/// Todo reminder owner 候选被跳过的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoReminderOwnerSkipReason {
    InvalidPrivateScope,
    ConflictingPrivateTargets,
}

/// 存储层只返回结构化冲突信息；具体日志由 reminder 业务层统一处理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoReminderSkippedOwner {
    pub owner_key: String,
    pub private_scope_keys: Vec<String>,
    pub parsed_target_ids: Vec<String>,
    pub reason: TodoReminderOwnerSkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TodoReminderOwnerQueryResult {
    pub candidates: Vec<TodoReminderOwnerCandidate>,
    pub skipped: Vec<TodoReminderSkippedOwner>,
}

/// 待办操作错误类型。
#[derive(Debug, Clone)]
pub struct TodoError {
    code: &'static str,
    message: String,
}

impl TodoStore {
    /// 创建一个新的 TodoStore，复用应用级 SQLite 句柄。
    pub fn new(database: SqliteDatabase) -> Self {
        Self { database }
    }

    /// 构造所有者标识，优先使用用户 ID，其次使用 scope_key。
    pub fn owner(user_id: Option<&str>, scope_key: &str) -> TodoOwner {
        let user_id = user_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let scope_key = clean_optional(scope_key).unwrap_or_else(|| "unknown".to_owned());
        let key = user_id.clone().unwrap_or_else(|| scope_key.clone());
        TodoOwner {
            key,
            user_id,
            scope_key,
        }
    }

    /// 创建一条待办事项，自动分配 ID。
    pub fn create(&self, owner: &TodoOwner, draft: TodoItemDraft) -> Result<TodoItem, TodoError> {
        let draft = normalize_draft(draft)?;
        let now = now_iso_cn();
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO todos (
                owner_key, user_id, scope_key, title, detail, raw_text,
                due_date, due_at, time_precision, status, completed,
                created_at, updated_at, completed_at, cancelled_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11, ?12, NULL, NULL)",
            params![
                owner.key.as_str(),
                owner.user_id.as_deref(),
                owner.scope_key.as_str(),
                draft.title,
                draft.detail,
                draft.raw_text,
                draft.due_date,
                draft.due_at,
                draft.time_precision.as_str(),
                TodoStatus::Pending.as_str(),
                now,
                now,
            ],
        )
        .map_err(TodoError::from_sql)?;
        let id = conn.last_insert_rowid();
        get_by_id_unlocked(&conn, owner, id)?
            .ok_or_else(|| TodoError::io("todo disappeared after insert"))
    }

    /// 列出所有待处理（Pending）的待办事项，按截止时间排序。
    pub fn list_pending(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, TodoStatus::Pending)?;
        sort_todos(&mut items);
        Ok(items)
    }

    /// 按 owner_key + 一组私聊 scope 读取 pending。
    ///
    /// reminder 需要按 owner 聚合扫描，但同一 owner 可能保留多个历史 private scope；
    /// 这里保持一次 owner 只查一轮 pending，并继续复用既有待办排序语义。
    pub fn list_pending_for_private_scopes(
        &self,
        owner_key: &str,
        private_scope_keys: &[String],
    ) -> Result<Vec<TodoItem>, TodoError> {
        if private_scope_keys.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.connection()?;
        let mut items = query_items_by_owner_scopes_and_status(
            &conn,
            owner_key,
            private_scope_keys,
            TodoStatus::Pending,
        )?;
        sort_todos(&mut items);
        Ok(items)
    }

    /// 查询可验证私聊推送目标的 owner 列表。
    ///
    /// 这里只判断 owner 与 private target 的对应关系是否可靠，不负责日志记录；
    /// 同一 owner 若存在冲突 target 或不可解析 scope，会作为 skipped 结果返回。
    pub fn list_private_reminder_owners(&self) -> Result<TodoReminderOwnerQueryResult, TodoError> {
        let conn = self.connection()?;
        let rows = query_private_pending_owner_scopes(&conn)?;
        let mut grouped = BTreeMap::<String, Vec<String>>::new();
        for (owner_key, scope_key) in rows {
            grouped.entry(owner_key).or_default().push(scope_key);
        }

        let mut result = TodoReminderOwnerQueryResult::default();
        for (owner_key, private_scope_keys) in grouped {
            let mut parsed_target_ids = BTreeSet::new();
            let mut has_invalid_scope = false;
            for scope_key in &private_scope_keys {
                let Some(target_id) = private_target_from_scope_key(scope_key) else {
                    has_invalid_scope = true;
                    continue;
                };
                parsed_target_ids.insert(target_id);
            }

            if has_invalid_scope {
                result.skipped.push(TodoReminderSkippedOwner {
                    owner_key,
                    private_scope_keys,
                    parsed_target_ids: parsed_target_ids.into_iter().collect(),
                    reason: TodoReminderOwnerSkipReason::InvalidPrivateScope,
                });
                continue;
            }
            if parsed_target_ids.len() != 1 {
                result.skipped.push(TodoReminderSkippedOwner {
                    owner_key,
                    private_scope_keys,
                    parsed_target_ids: parsed_target_ids.into_iter().collect(),
                    reason: TodoReminderOwnerSkipReason::ConflictingPrivateTargets,
                });
                continue;
            }

            result.candidates.push(TodoReminderOwnerCandidate {
                owner_key,
                private_target_id: parsed_target_ids.into_iter().next().unwrap_or_default(),
                private_scope_keys,
            });
        }
        Ok(result)
    }

    /// 列出所有已完成的待办事项，按完成时间降序排列。
    pub fn list_completed(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, TodoStatus::Completed)?;
        sort_completed_todos_desc(&mut items);
        Ok(items)
    }

    /// 列出所有已取消的待办事项，按创建时间降序排列。
    pub fn list_cancelled(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, TodoStatus::Cancelled)?;
        sort_todos_by_created_desc(&mut items);
        Ok(items)
    }

    /// 列出所有待办事项（不限状态），按创建时间降序排列。
    pub fn list_all(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items(&conn, owner)?;
        sort_todos_by_created_desc(&mut items);
        Ok(items)
    }

    /// 按关键词搜索待处理事项，返回按匹配得分排序的结果。
    pub fn search_pending(
        &self,
        owner: &TodoOwner,
        query: &str,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut scored = query_items_by_status(&conn, owner, TodoStatus::Pending)?
            .into_iter()
            .filter_map(|item| search_score(&item, query).map(|score| (score, item)))
            .collect::<Vec<_>>();
        scored.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| compare_todo_order(left, right))
        });
        Ok(scored.into_iter().map(|(_, item)| item).collect())
    }

    /// 智能匹配待处理事项：只做标题/详情等用户可见内容匹配（至多返回 5 条）。
    /// 用户侧不再暴露内部 ID，因此这里不能再把查询词当成 ID 直连数据库项。
    pub fn match_pending(
        &self,
        owner: &TodoOwner,
        query: &str,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let items = query_items_by_status(&conn, owner, TodoStatus::Pending)?;
        let mut scored = items
            .into_iter()
            .filter_map(|item| search_score(&item, query).map(|score| (score, item)))
            .collect::<Vec<_>>();
        scored.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| compare_todo_order(left, right))
        });
        Ok(scored.into_iter().take(5).map(|(_, item)| item).collect())
    }

    /// 列出在指定日期之前完成的待办事项（基于北京时间）。
    pub fn list_completed_before(
        &self,
        owner: &TodoOwner,
        completed_before: NaiveDate,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, TodoStatus::Completed)?
            .into_iter()
            .filter(|item| {
                item.completed_at
                    .as_deref()
                    .and_then(local_date_from_timestamp)
                    .is_some_and(|date| date < completed_before)
            })
            .collect::<Vec<_>>();
        sort_completed_todos(&mut items);
        Ok(items)
    }

    /// 根据 ID 列表查找已完成的待办事项。
    pub fn list_completed_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut matched = Vec::new();
        for id in ids.iter().filter_map(|id| parse_todo_db_id(id)) {
            if let Some(item) = get_by_id_status_unlocked(&conn, owner, id, TodoStatus::Completed)?
            {
                matched.push(item);
            }
        }
        Ok(matched)
    }

    /// 根据 ID 列表查找指定状态的待办事项。
    pub fn list_by_ids_with_status(
        &self,
        owner: &TodoOwner,
        ids: &[String],
        status: TodoStatus,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut matched = Vec::new();
        for id in ids.iter().filter_map(|id| parse_todo_db_id(id)) {
            if let Some(item) = get_by_id_status_unlocked(&conn, owner, id, status.clone())? {
                matched.push(item);
            }
        }
        Ok(matched)
    }

    /// 根据内部 ID 获取当前 owner/scope 下任意状态的待办。
    pub fn get_by_id(&self, owner: &TodoOwner, id: &str) -> Result<Option<TodoItem>, TodoError> {
        let Some(id) = parse_todo_db_id(id) else {
            return Ok(None);
        };
        let conn = self.connection()?;
        get_by_id_unlocked(&conn, owner, id)
    }

    /// 将待办事项标记为已完成。
    pub fn complete(&self, owner: &TodoOwner, id: &str) -> Result<TodoItem, TodoError> {
        let id = parse_required_todo_db_id(id)?;
        let conn = self.connection()?;
        let now = now_iso_cn();
        let affected = conn
            .execute(
                "UPDATE todos
                 SET status = ?4,
                     completed = 1,
                     updated_at = ?5,
                     completed_at = ?5
                 WHERE id = ?1
                   AND owner_key = ?2
                   AND scope_key = ?3
                   AND status = ?6",
                params![
                    id,
                    owner.key.as_str(),
                    owner.scope_key.as_str(),
                    TodoStatus::Completed.as_str(),
                    now,
                    TodoStatus::Pending.as_str(),
                ],
            )
            .map_err(TodoError::from_sql)?;
        if affected == 0 {
            return Err(TodoError::not_found("todo not found"));
        }
        get_by_id_unlocked(&conn, owner, id)?
            .ok_or_else(|| TodoError::io("todo disappeared after complete"))
    }

    /// 批量完成待办事项（按 ID 列表匹配 Pending 项）。
    pub fn complete_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkCompleteOutcome, TodoError> {
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut completed = Vec::new();
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            let affected = tx
                .execute(
                    "UPDATE todos
                     SET status = ?4,
                         completed = 1,
                         updated_at = ?5,
                         completed_at = ?5
                     WHERE id = ?1
                       AND owner_key = ?2
                       AND scope_key = ?3
                       AND status = ?6",
                    params![
                        id,
                        owner.key.as_str(),
                        owner.scope_key.as_str(),
                        TodoStatus::Completed.as_str(),
                        now,
                        TodoStatus::Pending.as_str(),
                    ],
                )
                .map_err(TodoError::from_sql)?;
            if affected == 0 {
                skipped_ids.push(id_text);
            } else if let Some(item) = get_by_id_unlocked(&tx, owner, id)? {
                completed.push(item);
            }
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoBulkCompleteOutcome {
            completed,
            skipped_ids,
        })
    }

    /// 批量恢复已完成待办事项（按 ID 列表匹配 Completed 项）。
    pub fn restore_completed_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkRestoreOutcome, TodoError> {
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut restored = Vec::new();
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            // 恢复完成项时必须清空 completed_at，避免 pending 列表残留完成时间语义。
            let affected = tx
                .execute(
                    "UPDATE todos
                     SET status = ?4,
                         completed = 0,
                         updated_at = ?5,
                         completed_at = NULL
                     WHERE id = ?1
                       AND owner_key = ?2
                       AND scope_key = ?3
                       AND status = ?6",
                    params![
                        id,
                        owner.key.as_str(),
                        owner.scope_key.as_str(),
                        TodoStatus::Pending.as_str(),
                        now,
                        TodoStatus::Completed.as_str(),
                    ],
                )
                .map_err(TodoError::from_sql)?;
            if affected == 0 {
                skipped_ids.push(id_text);
            } else if let Some(item) = get_by_id_unlocked(&tx, owner, id)? {
                restored.push(item);
            }
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoBulkRestoreOutcome {
            restored,
            skipped_ids,
        })
    }

    /// 批量恢复已取消待办事项（按 ID 列表匹配 Cancelled 项）。
    pub fn restore_cancelled_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkRestoreOutcome, TodoError> {
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut restored = Vec::new();
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            // 恢复取消项时必须清空 cancelled_at，避免 pending 列表残留取消时间语义。
            let affected = tx
                .execute(
                    "UPDATE todos
                     SET status = ?4,
                         completed = 0,
                         updated_at = ?5,
                         cancelled_at = NULL
                     WHERE id = ?1
                       AND owner_key = ?2
                       AND scope_key = ?3
                       AND status = ?6",
                    params![
                        id,
                        owner.key.as_str(),
                        owner.scope_key.as_str(),
                        TodoStatus::Pending.as_str(),
                        now,
                        TodoStatus::Cancelled.as_str(),
                    ],
                )
                .map_err(TodoError::from_sql)?;
            if affected == 0 {
                skipped_ids.push(id_text);
            } else if let Some(item) = get_by_id_unlocked(&tx, owner, id)? {
                restored.push(item);
            }
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoBulkRestoreOutcome {
            restored,
            skipped_ids,
        })
    }

    /// 批量取消已完成的待办事项（按 ID 列表匹配）。
    pub fn cancel_completed_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkCancelOutcome, TodoError> {
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut cancelled = Vec::new();
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            let affected = tx
                .execute(
                    "UPDATE todos
                     SET status = ?4,
                         completed = 0,
                         updated_at = ?5,
                         cancelled_at = ?5
                     WHERE id = ?1
                       AND owner_key = ?2
                       AND scope_key = ?3
                       AND status = ?6",
                    params![
                        id,
                        owner.key.as_str(),
                        owner.scope_key.as_str(),
                        TodoStatus::Cancelled.as_str(),
                        now,
                        TodoStatus::Completed.as_str(),
                    ],
                )
                .map_err(TodoError::from_sql)?;
            if affected == 0 {
                skipped_ids.push(id_text);
            } else if let Some(item) = get_by_id_unlocked(&tx, owner, id)? {
                cancelled.push(item);
            }
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoBulkCancelOutcome {
            cancelled,
            skipped_ids,
        })
    }

    /// 物理删除已完成待办事项（按 ID 列表匹配）。
    ///
    /// 已完成与已取消都是终态；用户再次删除时应清理记录本身，而不是改成另一种终态。
    pub fn delete_completed_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkDeleteOutcome, TodoError> {
        self.delete_by_ids_with_status(owner, ids, TodoStatus::Completed)
    }

    /// 物理删除已取消待办事项（按 ID 列表匹配）。
    ///
    /// 清理已取消项与普通删除不同：普通删除保持软删除语义，这里只允许删除
    /// 已经处于 Cancelled 状态的记录，并在同一事务内校验 owner、scope 和 status。
    pub fn delete_cancelled_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkDeleteOutcome, TodoError> {
        self.delete_by_ids_with_status(owner, ids, TodoStatus::Cancelled)
    }

    /// 按指定终态物理删除记录，并在事务内校验 owner、scope 和 status。
    fn delete_by_ids_with_status(
        &self,
        owner: &TodoOwner,
        ids: &[String],
        status: TodoStatus,
    ) -> Result<TodoBulkDeleteOutcome, TodoError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut deleted_count = 0usize;
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            let affected = tx
                .execute(
                    "DELETE FROM todos
                     WHERE id = ?1
                       AND owner_key = ?2
                       AND scope_key = ?3
                       AND status = ?4",
                    params![
                        id,
                        owner.key.as_str(),
                        owner.scope_key.as_str(),
                        status.as_str(),
                    ],
                )
                .map_err(TodoError::from_sql)?;
            if affected == 0 {
                skipped_ids.push(id_text);
            } else {
                deleted_count += affected;
            }
        }
        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoBulkDeleteOutcome {
            deleted_count,
            skipped_ids,
        })
    }

    /// 取消一条待办事项（将状态设为 Cancelled）。
    pub fn cancel(&self, owner: &TodoOwner, id: &str) -> Result<TodoItem, TodoError> {
        let id = parse_required_todo_db_id(id)?;
        let conn = self.connection()?;
        let now = now_iso_cn();
        let affected = conn
            .execute(
                "UPDATE todos
                 SET status = ?4,
                     completed = 0,
                     updated_at = ?5,
                     cancelled_at = ?5
                 WHERE id = ?1
                   AND owner_key = ?2
                   AND scope_key = ?3
                   AND status = ?6",
                params![
                    id,
                    owner.key.as_str(),
                    owner.scope_key.as_str(),
                    TodoStatus::Cancelled.as_str(),
                    now,
                    TodoStatus::Pending.as_str(),
                ],
            )
            .map_err(TodoError::from_sql)?;
        if affected == 0 {
            return Err(TodoError::not_found("todo not found"));
        }
        get_by_id_unlocked(&conn, owner, id)?
            .ok_or_else(|| TodoError::io("todo disappeared after cancel"))
    }

    /// 编辑一条待办事项（替换标题、详情、截止时间等字段）。
    pub fn edit(
        &self,
        owner: &TodoOwner,
        id: &str,
        draft: TodoItemDraft,
    ) -> Result<TodoItem, TodoError> {
        let id = parse_required_todo_db_id(id)?;
        let draft = normalize_draft(draft)?;
        let conn = self.connection()?;
        let now = now_iso_cn();
        let affected = conn
            .execute(
                "UPDATE todos
                 SET title = ?4,
                     detail = ?5,
                     raw_text = ?6,
                     due_date = ?7,
                     due_at = ?8,
                     time_precision = ?9,
                     updated_at = ?10
                 WHERE id = ?1
                   AND owner_key = ?2
                   AND scope_key = ?3
                   AND status = ?11",
                params![
                    id,
                    owner.key.as_str(),
                    owner.scope_key.as_str(),
                    draft.title,
                    draft.detail,
                    draft.raw_text,
                    draft.due_date,
                    draft.due_at,
                    draft.time_precision.as_str(),
                    now,
                    TodoStatus::Pending.as_str(),
                ],
            )
            .map_err(TodoError::from_sql)?;
        if affected == 0 {
            return Err(TodoError::not_found("todo not found"));
        }
        get_by_id_unlocked(&conn, owner, id)?
            .ok_or_else(|| TodoError::io("todo disappeared after edit"))
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, TodoError> {
        self.database.connection().map_err(TodoError::from_database)
    }

    #[cfg(test)]
    pub fn set_items_for_test(
        &self,
        owner: &TodoOwner,
        items: &[TodoItem],
    ) -> Result<(), TodoError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        tx.execute(
            "DELETE FROM todos WHERE owner_key = ?1 AND scope_key = ?2",
            params![owner.key.as_str(), owner.scope_key.as_str()],
        )
        .map_err(TodoError::from_sql)?;
        for item in items {
            let id = parse_required_todo_db_id(&item.id)?;
            let user_id = item.user_id.as_deref().or(owner.user_id.as_deref());
            tx.execute(
                "INSERT INTO todos (
                    id, owner_key, user_id, scope_key, title, detail, raw_text,
                    due_date, due_at, time_precision, status, completed,
                    created_at, updated_at, completed_at, cancelled_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    id,
                    owner.key.as_str(),
                    user_id,
                    owner.scope_key.as_str(),
                    item.title,
                    item.detail,
                    item.raw_text,
                    item.due_date,
                    item.due_at,
                    item.time_precision.as_str(),
                    item.status.as_str(),
                    item.status.completed_flag(),
                    item.created_at,
                    item.updated_at,
                    item.completed_at,
                    item.cancelled_at,
                ],
            )
            .map_err(TodoError::from_sql)?;
        }
        tx.commit().map_err(TodoError::from_sql)
    }
}

impl TodoError {
    /// 获取错误码。
    pub fn code(&self) -> &str {
        self.code
    }

    /// 获取错误消息。
    pub fn message(&self) -> &str {
        &self.message
    }

    /// 构造请求参数错误。
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: "bad_request",
            message: message.into(),
        }
    }

    /// 构造资源未找到错误。
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found",
            message: message.into(),
        }
    }

    /// 构造 I/O 错误。
    fn io(message: impl Into<String>) -> Self {
        Self {
            code: "io_error",
            message: message.into(),
        }
    }

    fn data(message: impl Into<String>) -> Self {
        Self {
            code: "data_error",
            message: message.into(),
        }
    }

    fn from_database(err: DatabaseError) -> Self {
        Self {
            code: err.code(),
            message: err.message().to_owned(),
        }
    }

    fn from_sql(err: rusqlite::Error) -> Self {
        match err {
            rusqlite::Error::FromSqlConversionFailure(_, _, inner) => {
                Self::data(format!("sqlite data mapping failed: {inner}"))
            }
            other => Self::io(format!("sqlite failed: {other}")),
        }
    }
}

impl TodoStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    #[cfg(test)]
    fn completed_flag(&self) -> i64 {
        i64::from(matches!(self, Self::Completed))
    }

    fn from_db(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(format!("invalid todo status `{other}`")),
        }
    }
}

impl TodoTimePrecision {
    fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Date => "date",
            Self::DateTime => "date_time",
            Self::Inferred => "inferred",
        }
    }

    fn from_db(value: &str) -> Result<Self, String> {
        match value {
            "none" => Ok(Self::None),
            "date" => Ok(Self::Date),
            "date_time" => Ok(Self::DateTime),
            "inferred" => Ok(Self::Inferred),
            other => Err(format!("invalid todo time precision `{other}`")),
        }
    }
}

impl TodoItemDraft {
    /// 从已有的 TodoItem 构造编辑草稿，保留原字段并更新 raw_text。
    pub fn from_item(item: &TodoItem, raw_text: impl Into<String>) -> Self {
        Self {
            title: item.title.clone(),
            detail: item.detail.clone(),
            raw_text: clean_optional(&raw_text.into()).or_else(|| item.raw_text.clone()),
            due_date: item.due_date.clone(),
            due_at: item.due_at.clone(),
            time_precision: item.time_precision.clone(),
        }
    }
}

fn query_items(conn: &Connection, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, user_id, scope_key, title, detail, raw_text,
                    due_date, due_at, time_precision, status,
                    created_at, updated_at, completed_at, cancelled_at
             FROM todos
             WHERE owner_key = ?1 AND scope_key = ?2",
        )
        .map_err(TodoError::from_sql)?;
    let rows = stmt
        .query_map(
            params![owner.key.as_str(), owner.scope_key.as_str()],
            todo_item_from_row,
        )
        .map_err(TodoError::from_sql)?;
    collect_rows(rows)
}

fn query_items_by_status(
    conn: &Connection,
    owner: &TodoOwner,
    status: TodoStatus,
) -> Result<Vec<TodoItem>, TodoError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, user_id, scope_key, title, detail, raw_text,
                    due_date, due_at, time_precision, status,
                    created_at, updated_at, completed_at, cancelled_at
             FROM todos
             WHERE owner_key = ?1 AND scope_key = ?2 AND status = ?3",
        )
        .map_err(TodoError::from_sql)?;
    let rows = stmt
        .query_map(
            params![
                owner.key.as_str(),
                owner.scope_key.as_str(),
                status.as_str()
            ],
            todo_item_from_row,
        )
        .map_err(TodoError::from_sql)?;
    collect_rows(rows)
}

fn query_items_by_owner_scopes_and_status(
    conn: &Connection,
    owner_key: &str,
    scope_keys: &[String],
    status: TodoStatus,
) -> Result<Vec<TodoItem>, TodoError> {
    let placeholders = std::iter::repeat_n("?", scope_keys.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT id, user_id, scope_key, title, detail, raw_text,
                due_date, due_at, time_precision, status,
                created_at, updated_at, completed_at, cancelled_at
         FROM todos
         WHERE owner_key = ? AND status = ? AND scope_key IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql).map_err(TodoError::from_sql)?;
    let status = status.as_str();
    let mut params = Vec::with_capacity(scope_keys.len() + 2);
    params.push(owner_key);
    params.push(status);
    params.extend(scope_keys.iter().map(String::as_str));
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params), todo_item_from_row)
        .map_err(TodoError::from_sql)?;
    collect_rows(rows)
}

fn query_private_pending_owner_scopes(
    conn: &Connection,
) -> Result<Vec<(String, String)>, TodoError> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT owner_key, scope_key
             FROM todos
             WHERE status = ?1
               AND scope_key LIKE 'private:%'
             ORDER BY owner_key ASC, scope_key ASC",
        )
        .map_err(TodoError::from_sql)?;
    let rows = stmt
        .query_map(params![TodoStatus::Pending.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(TodoError::from_sql)?;
    collect_rows(rows)
}

fn get_by_id_unlocked(
    conn: &Connection,
    owner: &TodoOwner,
    id: i64,
) -> Result<Option<TodoItem>, TodoError> {
    conn.query_row(
        "SELECT id, user_id, scope_key, title, detail, raw_text,
                due_date, due_at, time_precision, status,
                created_at, updated_at, completed_at, cancelled_at
         FROM todos
         WHERE id = ?1 AND owner_key = ?2 AND scope_key = ?3",
        params![id, owner.key.as_str(), owner.scope_key.as_str()],
        todo_item_from_row,
    )
    .optional()
    .map_err(TodoError::from_sql)
}

fn get_by_id_status_unlocked(
    conn: &Connection,
    owner: &TodoOwner,
    id: i64,
    status: TodoStatus,
) -> Result<Option<TodoItem>, TodoError> {
    conn.query_row(
        "SELECT id, user_id, scope_key, title, detail, raw_text,
                due_date, due_at, time_precision, status,
                created_at, updated_at, completed_at, cancelled_at
         FROM todos
         WHERE id = ?1 AND owner_key = ?2 AND scope_key = ?3 AND status = ?4",
        params![
            id,
            owner.key.as_str(),
            owner.scope_key.as_str(),
            status.as_str()
        ],
        todo_item_from_row,
    )
    .optional()
    .map_err(TodoError::from_sql)
}

fn todo_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TodoItem> {
    let time_precision = row.get::<_, String>(8)?;
    let time_precision = TodoTimePrecision::from_db(&time_precision)
        .map_err(|message| from_sql_text_error(8, message))?;
    let status = row.get::<_, String>(9)?;
    let status = TodoStatus::from_db(&status).map_err(|message| from_sql_text_error(9, message))?;
    Ok(TodoItem {
        id: row.get::<_, i64>(0)?.to_string(),
        user_id: row.get(1)?,
        scope_key: row.get(2)?,
        title: row.get(3)?,
        detail: row.get(4)?,
        raw_text: row.get(5)?,
        due_date: row.get(6)?,
        due_at: row.get(7)?,
        time_precision,
        status,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        completed_at: row.get(12)?,
        cancelled_at: row.get(13)?,
    })
}

fn from_sql_text_error(index: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, TodoError> {
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(TodoError::from_sql)
}

/// 从用户文本中推断截止时间并填充到草稿中（仅当草稿尚未设置截止时间时生效）。
pub fn enrich_draft_time_from_text(
    draft: &mut TodoItemDraft,
    user_text: &str,
    ctx: &RequestTimeContext,
) {
    if draft.due_date.is_some() || draft.due_at.is_some() {
        return;
    }
    if let Some((date, precision)) = infer_due_date_from_text(user_text, ctx) {
        draft.due_date = Some(date);
        draft.time_precision = precision;
    }
}

pub fn infer_due_date_from_text(
    text: &str,
    ctx: &RequestTimeContext,
) -> Option<(String, TodoTimePrecision)> {
    let inferred = time_context::infer_due_date_from_text(text, ctx)?;
    let precision = match inferred.precision {
        DateInferencePrecision::Date => TodoTimePrecision::Date,
        DateInferencePrecision::Inferred => TodoTimePrecision::Inferred,
    };
    Some((inferred.date, precision))
}

pub fn display_todo_time(item: &TodoItem) -> String {
    display_time_parts(item.due_date.as_deref(), item.due_at.as_deref())
}

pub fn display_draft_time(draft: &TodoItemDraft) -> String {
    display_time_parts(draft.due_date.as_deref(), draft.due_at.as_deref())
}

fn display_time_parts(due_date: Option<&str>, due_at: Option<&str>) -> String {
    due_at
        .and_then(clean_optional)
        .or_else(|| due_date.and_then(clean_optional))
        .map(|value| format_todo_time_for_display(&value))
        .unwrap_or_else(|| "未指定".to_owned())
}

/// 规范化待办草稿：校验必填字段、脱敏敏感文本、截断超长文本。
fn normalize_draft(mut draft: TodoItemDraft) -> Result<TodoItemDraft, TodoError> {
    let title = clean_optional(&draft.title)
        .ok_or_else(|| TodoError::bad_request("todo title is required"))?;
    draft.title = truncate_chars(&redact_sensitive_text(title), 120);
    draft.detail = draft
        .detail
        .as_deref()
        .and_then(clean_optional)
        .map(redact_sensitive_text)
        .map(|text| truncate_chars(&text, 500));
    draft.raw_text = draft
        .raw_text
        .as_deref()
        .and_then(clean_optional)
        .map(redact_sensitive_text)
        .map(|text| truncate_chars(&text, 500));
    draft.due_date = draft
        .due_date
        .as_deref()
        .and_then(clean_optional)
        .filter(|value| is_valid_ymd_date(value));
    draft.due_at = draft
        .due_at
        .as_deref()
        .and_then(clean_optional)
        .filter(|value| has_valid_ymd_date_prefix(value));
    if draft.due_at.is_some() && matches!(draft.time_precision, TodoTimePrecision::None) {
        draft.time_precision = TodoTimePrecision::DateTime;
    } else if draft.due_date.is_some() && matches!(draft.time_precision, TodoTimePrecision::None) {
        draft.time_precision = TodoTimePrecision::Date;
    } else if draft.due_at.is_none() && draft.due_date.is_none() {
        draft.time_precision = TodoTimePrecision::None;
    }
    Ok(draft)
}

/// 计算待办事项与查询关键词的匹配得分（标题 > 详情 > 原文）。
fn search_score(item: &TodoItem, query: &str) -> Option<i32> {
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return Some(1);
    }
    let title = item.title.to_ascii_lowercase();
    let detail = item.detail.clone().unwrap_or_default().to_ascii_lowercase();
    let raw = item
        .raw_text
        .clone()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let haystack = format!("{title}\n{detail}\n{raw}");
    let tokens = query.split_whitespace().collect::<Vec<_>>();
    if !tokens.is_empty() && !tokens.iter().all(|token| haystack.contains(token)) {
        return None;
    }
    if !tokens.is_empty() {
        return Some(if title.contains(&query) {
            80
        } else if detail.contains(&query) {
            55
        } else {
            45
        });
    }
    if title == query {
        Some(100)
    } else if title.contains(&query) {
        Some(80)
    } else if detail.contains(&query) {
        Some(55)
    } else if raw.contains(&query) {
        Some(45)
    } else {
        None
    }
}

/// 按截止时间 + ID 排序待处理事项。
fn sort_todos(items: &mut [TodoItem]) {
    items.sort_by(compare_todo_order);
}

/// 按完成时间 + 截止顺序排序已完成事项。
fn sort_completed_todos(items: &mut [TodoItem]) {
    items.sort_by(|left, right| {
        completed_todo_sort_key(left)
            .cmp(&completed_todo_sort_key(right))
            .then_with(|| compare_todo_order(left, right))
    });
}

/// 按完成时间降序排序已完成事项。
fn sort_completed_todos_desc(items: &mut [TodoItem]) {
    items.sort_by(|left, right| {
        completed_todo_sort_key(right)
            .cmp(&completed_todo_sort_key(left))
            .then_with(|| left.id.cmp(&right.id))
    });
}

/// 按创建时间降序排序所有事项。
fn sort_todos_by_created_desc(items: &mut [TodoItem]) {
    items.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
}

/// 比较两个待办事项的排列顺序：有截止时间的排前面，其次按 ID。
fn compare_todo_order(left: &TodoItem, right: &TodoItem) -> Ordering {
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

/// 清理待办 ID：去除首尾空格和括号标记。
fn clean_todo_id(value: &str) -> String {
    value
        .trim()
        .trim_matches(&['[', ']', '#', ' ', '\t', '\n', '\r'][..])
        .to_owned()
}

fn parse_todo_db_id(value: &str) -> Option<i64> {
    clean_todo_id(value)
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
}

fn parse_required_todo_db_id(value: &str) -> Result<i64, TodoError> {
    parse_todo_db_id(value).ok_or_else(|| TodoError::not_found("todo not found"))
}

/// 清理可选字符串字段。
fn clean_optional(value: &str) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

fn private_target_from_scope_key(value: &str) -> Option<String> {
    value.strip_prefix("private:").and_then(clean_optional)
}

#[cfg(test)]
mod tests;

//! 待办事项（Todo）存储模块。
//!
//! 以项目级 SQLite 存储待办事项列表，支持创建、列表（按状态和条件）、
//! 搜索、编辑、完成/取消等操作。支持中文自然语言日期推断。

use std::cmp::Ordering;

use chrono::NaiveDate;
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

    /// 列出所有已完成的待办事项，按完成时间降序排列。
    pub fn list_completed(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, TodoStatus::Completed)?;
        sort_completed_todos_desc(&mut items);
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

/// 截断字符串到指定字符数（基于 Unicode 字符，非字节）。
fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.trim().to_owned();
    }
    text.chars()
        .take(limit)
        .collect::<String>()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, FixedOffset, TimeZone};

    fn test_store() -> TodoStore {
        TodoStore::new(SqliteDatabase::open_temp("qq-maid-todo-test", TODO_MIGRATIONS).unwrap())
    }

    fn fixed_context() -> RequestTimeContext {
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 6, 10, 9, 0, 0).unwrap())
    }

    fn completed_at_on(date: NaiveDate, hour: u32) -> String {
        format!("{}T{hour:02}:00:00+08:00", date.format("%Y-%m-%d"))
    }

    #[test]
    fn infers_common_chinese_dates() {
        let ctx = fixed_context();

        assert_eq!(
            infer_due_date_from_text("三天后检查日志", &ctx).unwrap(),
            ("2026-06-13".to_owned(), TodoTimePrecision::Date)
        );
        assert_eq!(
            infer_due_date_from_text("下周一处理", &ctx).unwrap(),
            ("2026-06-15".to_owned(), TodoTimePrecision::Date)
        );
        assert_eq!(
            infer_due_date_from_text("周五提交", &ctx).unwrap(),
            ("2026-06-12".to_owned(), TodoTimePrecision::Inferred)
        );
        assert_eq!(
            infer_due_date_from_text("6月15号提醒", &ctx).unwrap(),
            ("2026-06-15".to_owned(), TodoTimePrecision::Date)
        );
        assert_eq!(
            infer_due_date_from_text("月底复盘", &ctx).unwrap(),
            ("2026-06-30".to_owned(), TodoTimePrecision::Inferred)
        );
    }

    #[test]
    fn store_isolates_owners_and_soft_deletes() {
        let store = test_store();
        let owner_a = TodoStore::owner(Some("u1"), "group:g1");
        let owner_b = TodoStore::owner(Some("u2"), "group:g1");
        let item = store
            .create(
                &owner_a,
                TodoItemDraft {
                    title: "检查日志".to_owned(),
                    detail: Some("交通查询".to_owned()),
                    raw_text: Some("/todo add 检查日志".to_owned()),
                    due_date: Some("2026-06-15".to_owned()),
                    due_at: None,
                    time_precision: TodoTimePrecision::Date,
                },
            )
            .unwrap();

        assert_eq!(item.id, "1");
        assert_eq!(store.list_pending(&owner_b).unwrap().len(), 0);
        assert_eq!(store.search_pending(&owner_a, "交通").unwrap()[0].id, "1");

        store.cancel(&owner_a, "1").unwrap();
        assert!(store.list_pending(&owner_a).unwrap().is_empty());
        let all_items = store.list_all(&owner_a).unwrap();
        assert_eq!(all_items.len(), 1);
        assert_eq!(all_items[0].status, TodoStatus::Cancelled);
        let cancelled = all_items.iter().find(|item| item.id == "1").unwrap();
        assert_eq!(cancelled.status, TodoStatus::Cancelled);
        assert!(cancelled.cancelled_at.is_some());
    }

    #[test]
    fn sqlite_ids_are_stable_and_not_reused_after_soft_delete() {
        let store = test_store();
        let owner = TodoStore::owner(Some("u1"), "group:g1");
        let first = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "第一条".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            )
            .unwrap();
        store.cancel(&owner, &first.id).unwrap();
        let second = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "第二条".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            )
            .unwrap();

        assert_ne!(first.id, second.id);
        assert!(second.id.parse::<i64>().unwrap() > first.id.parse::<i64>().unwrap());
    }

    #[test]
    fn sqlite_store_persists_after_reopen_without_json_todo_dir() {
        let base =
            std::env::temp_dir().join(format!("qq-maid-todo-reopen-{}", uuid::Uuid::new_v4()));
        let path = base.join("app.db");
        let owner = TodoStore::owner(Some("u1"), "group:g1");
        let database = SqliteDatabase::open(&path, TODO_MIGRATIONS).unwrap();
        let store = TodoStore::new(database);
        let created = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "重开后仍存在".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            )
            .unwrap();
        drop(store);

        let reopened = TodoStore::new(SqliteDatabase::open(&path, TODO_MIGRATIONS).unwrap());
        assert_eq!(reopened.list_pending(&owner).unwrap()[0].id, created.id);

        let legacy_todo_dir = base.join("todos");
        assert!(!legacy_todo_dir.exists());
    }

    #[test]
    fn pending_list_sorts_by_due_time_then_id_without_changing_all_view() {
        let store = test_store();
        let owner = TodoStore::owner(Some("u1"), "group:g1");

        let no_time = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "无时间".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            )
            .unwrap();
        let later_datetime = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "15号中午".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: Some("2026-06-15 12:30:00".to_owned()),
                    time_precision: TodoTimePrecision::DateTime,
                },
            )
            .unwrap();
        let earlier_date = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "14号全天".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: Some("2026-06-14".to_owned()),
                    due_at: None,
                    time_precision: TodoTimePrecision::Date,
                },
            )
            .unwrap();
        let same_day_date_only = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "15号全天".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: Some("2026-06-15".to_owned()),
                    due_at: None,
                    time_precision: TodoTimePrecision::Date,
                },
            )
            .unwrap();
        let same_time_a = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "同时间 A".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: Some("2026-06-15 12:30:00".to_owned()),
                    time_precision: TodoTimePrecision::DateTime,
                },
            )
            .unwrap();
        let same_time_b = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "同时间 B".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: Some("2026-06-15 12:30:00".to_owned()),
                    time_precision: TodoTimePrecision::DateTime,
                },
            )
            .unwrap();

        let mut items = store.list_all(&owner).unwrap();
        for item in &mut items {
            item.created_at = match item.id.as_str() {
                id if id == no_time.id => "2026-06-01T00:00:00+08:00",
                id if id == later_datetime.id => "2026-06-06T00:00:00+08:00",
                id if id == earlier_date.id => "2026-06-05T00:00:00+08:00",
                id if id == same_day_date_only.id => "2026-06-04T00:00:00+08:00",
                id if id == same_time_a.id => "2026-06-03T00:00:00+08:00",
                id if id == same_time_b.id => "2026-06-02T00:00:00+08:00",
                _ => unreachable!("unexpected todo id"),
            }
            .to_owned();
            item.updated_at = item.created_at.clone();
        }
        store.set_items_for_test(&owner, &items).unwrap();

        let pending = store.list_pending(&owner).unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                earlier_date.id.as_str(),
                same_day_date_only.id.as_str(),
                later_datetime.id.as_str(),
                same_time_a.id.as_str(),
                same_time_b.id.as_str(),
                no_time.id.as_str()
            ]
        );

        let all = store.list_all(&owner).unwrap();
        assert_eq!(
            all.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            vec![
                later_datetime.id.as_str(),
                earlier_date.id.as_str(),
                same_day_date_only.id.as_str(),
                same_time_a.id.as_str(),
                same_time_b.id.as_str(),
                no_time.id.as_str()
            ]
        );
    }

    #[test]
    fn completed_at_filter_uses_shanghai_date_and_bulk_cancel_preserves_completed_at() {
        let store = test_store();
        let owner = TodoStore::owner(Some("u1"), "group:g1");
        let today = fixed_context().local_date();
        let yesterday = today - Duration::days(1);
        let before_yesterday = today - Duration::days(2);

        let old = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "旧完成".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            )
            .unwrap();
        let local_yesterday = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "上海昨天完成".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            )
            .unwrap();
        let due_old_completed_today = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "截止早但今天完成".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: Some("2026-01-01".to_owned()),
                    due_at: None,
                    time_precision: TodoTimePrecision::Date,
                },
            )
            .unwrap();
        let missing_completed_at = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "缺完成时间".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            )
            .unwrap();
        let already_cancelled = store
            .create(
                &owner,
                TodoItemDraft {
                    title: "已取消".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                },
            )
            .unwrap();

        for item in [
            &old,
            &local_yesterday,
            &due_old_completed_today,
            &missing_completed_at,
            &already_cancelled,
        ] {
            store.complete(&owner, &item.id).unwrap();
        }

        let mut items = store.list_all(&owner).unwrap();
        for item in &mut items {
            // 本测试关注完成时间过滤和软删除语义；created_at 固定为同一值，
            // 避免测试运行跨秒时影响 list_all 的创建时间倒序断言。
            item.created_at = "2026-06-10T00:00:00+08:00".to_owned();
            item.updated_at = item.created_at.clone();
            if item.id == old.id {
                item.completed_at = Some(completed_at_on(before_yesterday, 8));
            } else if item.id == local_yesterday.id {
                item.completed_at = Some("2026-06-08T20:30:00+00:00".to_owned());
            } else if item.id == due_old_completed_today.id {
                item.completed_at = Some(completed_at_on(today, 1));
            } else if item.id == missing_completed_at.id {
                item.completed_at = None;
            } else if item.id == already_cancelled.id {
                item.status = TodoStatus::Cancelled;
                item.completed_at = Some(completed_at_on(before_yesterday, 9));
            }
        }
        store.set_items_for_test(&owner, &items).unwrap();

        let yesterday_before = store.list_completed_before(&owner, yesterday).unwrap();
        assert_eq!(
            yesterday_before
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec![old.id.as_str()]
        );

        let up_to_yesterday = store.list_completed_before(&owner, today).unwrap();
        assert_eq!(
            up_to_yesterday
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec![old.id.as_str(), local_yesterday.id.as_str()]
        );

        let completed = store.list_completed(&owner).unwrap();
        assert_eq!(
            completed
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                due_old_completed_today.id.as_str(),
                local_yesterday.id.as_str(),
                old.id.as_str(),
                missing_completed_at.id.as_str()
            ]
        );
        assert!(
            completed
                .iter()
                .all(|item| item.status == TodoStatus::Completed)
        );

        let original_completed_at = up_to_yesterday[0].completed_at.clone();
        let outcome = store
            .cancel_completed_by_ids(
                &owner,
                &[
                    old.id.clone(),
                    already_cancelled.id.clone(),
                    "999".to_owned(),
                ],
            )
            .unwrap();
        assert_eq!(outcome.cancelled.len(), 1);
        assert_eq!(outcome.cancelled[0].id, old.id);
        assert_eq!(outcome.skipped_ids.len(), 2);

        let all = store.list_all(&owner).unwrap();
        let cancelled = all.iter().find(|item| item.id == old.id).unwrap();
        assert_eq!(cancelled.status, TodoStatus::Cancelled);
        assert_eq!(cancelled.completed_at, original_completed_at);
        assert!(cancelled.cancelled_at.is_some());
        let listed_all = store.list_all(&owner).unwrap();
        assert!(
            listed_all
                .iter()
                .any(|item| item.status == TodoStatus::Cancelled)
        );
        assert_eq!(
            listed_all
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                old.id.as_str(),
                local_yesterday.id.as_str(),
                due_old_completed_today.id.as_str(),
                missing_completed_at.id.as_str(),
                already_cancelled.id.as_str()
            ]
        );
    }
}

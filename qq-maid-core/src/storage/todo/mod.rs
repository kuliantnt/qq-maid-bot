//! 待办事项（Todo）存储模块。
//!
//! 以项目级 SQLite 存储待办事项列表，支持创建、列表（按状态和条件）、
//! 搜索、编辑、完成/取消等操作。支持中文自然语言日期推断。

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::{
    identity::{owner_scope_key, parse_stable_scope_key},
    storage::{
        database::{DatabaseError, SqliteDatabase, SqliteMigration},
        session::now_iso_cn,
    },
    util::time_context::local_date_from_timestamp,
};

// 拆分出的纯 helper 子模块：均不改变 schema 与对外 API。
mod id;
mod normalize;
mod query;
mod recurrence;
mod search;
mod sort;
mod time;

// 时间相关 helper 是 storage::todo 的对外公开 API（经由 runtime::todo 的 glob 再导出）。
pub use recurrence::{
    advance_after_completion, apply_recurrence_patch_to_draft, is_recurring,
    preview_next_reminder_at, recurrence_interval, recurrence_label,
};
pub use time::{
    display_draft_time, display_todo_time, enrich_draft_time_from_text, infer_due_date_from_text,
};

use self::id::{
    clean_todo_id, parse_required_todo_db_id, parse_todo_db_id, private_target_from_scope_key,
};
use normalize::normalize_draft;
use query::{
    get_by_id_status_unlocked, get_by_id_unlocked, query_items,
    query_items_by_owner_scopes_and_status, query_items_by_status,
    query_private_pending_owner_scopes,
};
use search::search_score;
use sort::{
    compare_todo_order, sort_completed_todos, sort_completed_todos_desc, sort_todo_all_board,
    sort_todos, sort_todos_by_created_desc,
};

const EXPLICIT_NO_RECURRENCE_INTERVAL_DAYS: u32 = u32::MAX;

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

pub const TODO_REMINDER_SCHEMA_V2: SqliteMigration = SqliteMigration {
    name: "todo_reminder_schema_v2",
    sql: "ALTER TABLE todos ADD COLUMN reminder_at TEXT;
          CREATE INDEX IF NOT EXISTS idx_todos_owner_reminder
              ON todos(owner_key, scope_key, reminder_at, id);",
};

pub const TODO_RECURRENCE_SCHEMA_V3: SqliteMigration = SqliteMigration {
    name: "todo_recurrence_schema_v3",
    sql: "ALTER TABLE todos ADD COLUMN recurrence_kind TEXT NOT NULL DEFAULT 'none';
          ALTER TABLE todos ADD COLUMN recurrence_interval_days INTEGER NOT NULL DEFAULT 0;
          CREATE INDEX IF NOT EXISTS idx_todos_owner_recurrence
              ON todos(owner_key, scope_key, recurrence_kind, recurrence_interval_days, id);",
};

pub const TODO_RECURRENCE_RULE_SCHEMA_V4: SqliteMigration = SqliteMigration {
    name: "todo_recurrence_rule_schema_v4",
    sql: "ALTER TABLE todos ADD COLUMN recurrence_interval INTEGER NOT NULL DEFAULT 0;
          ALTER TABLE todos ADD COLUMN recurrence_unit TEXT NOT NULL DEFAULT 'day';
          CREATE INDEX IF NOT EXISTS idx_todos_owner_recurrence_rule
              ON todos(owner_key, scope_key, recurrence_unit, recurrence_interval, id);",
};

pub const TODO_MIGRATIONS: &[SqliteMigration] = &[
    TODO_SCHEMA_V1,
    TODO_REMINDER_SCHEMA_V2,
    TODO_RECURRENCE_SCHEMA_V3,
    TODO_RECURRENCE_RULE_SCHEMA_V4,
];

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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoTimePrecision {
    #[default]
    None,
    Date,
    DateTime,
    Inferred,
}

/// 待办事项的重复规则类型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoRecurrenceKind {
    #[default]
    None,
    Daily,
    EveryNDays,
    Weekly,
    EveryNWeeks,
    Monthly,
    EveryNMonths,
    Yearly,
    EveryNYears,
}

/// 待办事项的重复间隔单位。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoRecurrenceUnit {
    #[default]
    Day,
    Week,
    Month,
    Year,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder_at: Option<String>,
    #[serde(default)]
    pub time_precision: TodoTimePrecision,
    #[serde(default)]
    pub recurrence_kind: TodoRecurrenceKind,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub recurrence_interval_days: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub recurrence_interval: u32,
    #[serde(default, skip_serializing_if = "is_default_recurrence_unit")]
    pub recurrence_unit: TodoRecurrenceUnit,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder_at: Option<String>,
    #[serde(default)]
    pub time_precision: TodoTimePrecision,
    #[serde(default)]
    pub recurrence_kind: TodoRecurrenceKind,
    #[serde(default)]
    pub recurrence_interval_days: u32,
    #[serde(default)]
    pub recurrence_interval: u32,
    #[serde(default)]
    pub recurrence_unit: TodoRecurrenceUnit,
}

/// 编辑补丁里的 recurrence 字段集合，供 runtime edit patch 复用 storage 归一语义。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoEditRecurrencePatch {
    pub kind: Option<TodoRecurrenceKind>,
    pub interval_days: Option<u32>,
    pub interval: Option<u32>,
    pub unit: Option<TodoRecurrenceUnit>,
}

/// list_todos 时间范围实际使用的业务字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoListDateField {
    Planned,
    CompletedAt,
    CancelledAt,
}

impl TodoListDateField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::CompletedAt => "completed_at",
            Self::CancelledAt => "cancelled_at",
        }
    }
}

/// list_todos 归一化后的时间范围筛选条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TodoListDateFilter {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub field: TodoListDateField,
}

/// 根据查询状态和参数决定时间范围筛哪个业务字段。
pub fn resolve_todo_list_date_filter(
    status: Option<TodoStatus>,
    due_date: Option<NaiveDate>,
    date_range: Option<(NaiveDate, NaiveDate)>,
) -> Result<Option<TodoListDateFilter>, TodoError> {
    if due_date.is_some() && date_range.is_some() {
        return Err(TodoError::bad_request(
            "date_range_text 和 due_date 不能同时传入。",
        ));
    }
    if let Some(date) = due_date {
        return Ok(Some(TodoListDateFilter {
            start: date,
            end: date,
            field: TodoListDateField::Planned,
        }));
    }
    let Some((start, end)) = date_range else {
        return Ok(None);
    };
    if start > end {
        return Err(TodoError::bad_request(
            "日期范围无效，开始日期不能晚于结束日期。",
        ));
    }
    let field = match status {
        Some(TodoStatus::Completed) => TodoListDateField::CompletedAt,
        Some(TodoStatus::Cancelled) => TodoListDateField::CancelledAt,
        Some(TodoStatus::Pending) | None => TodoListDateField::Planned,
    };
    Ok(Some(TodoListDateFilter { start, end, field }))
}

/// 批量“完成本次待办”的事务结果：一次性待办进入 completed，重复待办推进下一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoCompleteProgressOutcome {
    pub completed: Vec<TodoItem>,
    pub advanced: Vec<TodoItem>,
    pub skipped_ids: Vec<String>,
}

impl TodoCompleteProgressOutcome {
    pub fn all_changed(&self) -> Vec<TodoItem> {
        let mut items = self.completed.clone();
        items.extend(self.advanced.clone());
        items
    }
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

/// 批量取消待办事项的结果。
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

/// 批量物理删除待办事项的结果。
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
    pub primary_private_scope_key: String,
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

    /// 构造所有者标识。
    ///
    /// 新跨平台 scope 下 owner_key 纳入会话 scope 与 actor，避免跨平台同名用户串号；
    /// 旧 `private:` / `group:` scope 只保留给测试和未归一的受控入口，不在运行时做别名查询。
    pub fn owner(user_id: Option<&str>, scope_key: &str) -> TodoOwner {
        let user_id = user_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let scope_key = clean_optional(scope_key).unwrap_or_else(|| "unknown".to_owned());
        let key = if parse_stable_scope_key(&scope_key).is_some() {
            owner_scope_key(user_id.as_deref(), &scope_key)
        } else {
            user_id.clone().unwrap_or_else(|| scope_key.clone())
        };
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
        insert_todo_unlocked(&conn, owner, draft, &now)
    }

    /// 批量创建待办事项，整批在同一事务中提交。
    ///
    /// Tool Loop 可一次提交多条创建意图；任一条规范化或 SQLite 写入失败时，
    /// 已经插入的同批记录必须随事务回滚，避免用户看到失败但数据库留下半批结果。
    pub fn create_many(
        &self,
        owner: &TodoOwner,
        drafts: Vec<TodoItemDraft>,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut created = Vec::with_capacity(drafts.len());

        for draft in drafts {
            let draft = normalize_draft(draft)?;
            created.push(insert_todo_unlocked(&tx, owner, draft, &now)?);
        }

        tx.commit().map_err(TodoError::from_sql)?;
        Ok(created)
    }

    /// 列出所有待处理（Pending）的待办事项，按截止时间排序。
    pub fn list_pending(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, TodoStatus::Pending)?;
        sort_todos(&mut items);
        Ok(items)
    }

    /// 按计划日期列出指定状态的待办，日期按请求本地自然日解释。
    pub fn list_by_due_date(
        &self,
        owner: &TodoOwner,
        status: TodoStatus,
        due_date: NaiveDate,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, status.clone())?
            .into_iter()
            .filter(|item| todo_due_matches_local_date(item, due_date))
            .collect::<Vec<_>>();
        sort_items_for_status(&mut items, &status);
        Ok(items)
    }

    /// 按计划日期闭区间列出指定状态的待办，日期按请求本地自然日解释。
    pub fn list_by_due_date_range(
        &self,
        owner: &TodoOwner,
        status: TodoStatus,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<TodoItem>, TodoError> {
        if start > end {
            return Err(TodoError::bad_request(
                "日期范围无效，开始日期不能晚于结束日期。",
            ));
        }
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, status.clone())?
            .into_iter()
            .filter(|item| {
                todo_due_local_date(item).is_some_and(|date| start <= date && date <= end)
            })
            .collect::<Vec<_>>();
        sort_items_for_status(&mut items, &status);
        Ok(items)
    }

    /// 按归一化后的业务时间字段列出指定状态的待办。
    pub fn list_by_date_filter(
        &self,
        owner: &TodoOwner,
        status: TodoStatus,
        filter: TodoListDateFilter,
    ) -> Result<Vec<TodoItem>, TodoError> {
        if filter.start > filter.end {
            return Err(TodoError::bad_request(
                "日期范围无效，开始日期不能晚于结束日期。",
            ));
        }
        let conn = self.connection()?;
        let mut items = query_items_by_status(&conn, owner, status.clone())?
            .into_iter()
            .filter(|item| todo_list_date_matches_range(item, filter))
            .collect::<Vec<_>>();
        sort_items_for_status(&mut items, &status);
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
                primary_private_scope_key: private_scope_keys.first().cloned().unwrap_or_default(),
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

    /// 列出 `/todo all` 看板使用的全部待办，按用户可见分组顺序排列。
    pub fn list_all_for_board(&self, owner: &TodoOwner) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items(&conn, owner)?;
        // 先复用原全部列表顺序，后续分组时让已取消组自然保留既有稳定顺序。
        sort_todos_by_created_desc(&mut items);
        sort_todo_all_board(&mut items);
        Ok(items)
    }

    /// 按计划日期列出全部状态待办，排序与 `/todo all` 看板保持一致。
    pub fn list_all_by_due_date_for_board(
        &self,
        owner: &TodoOwner,
        due_date: NaiveDate,
    ) -> Result<Vec<TodoItem>, TodoError> {
        let conn = self.connection()?;
        let mut items = query_items(&conn, owner)?
            .into_iter()
            .filter(|item| todo_due_matches_local_date(item, due_date))
            .collect::<Vec<_>>();
        sort_todos_by_created_desc(&mut items);
        sort_todo_all_board(&mut items);
        Ok(items)
    }

    /// 按计划日期闭区间列出全部状态待办，排序与 `/todo all` 看板保持一致。
    pub fn list_all_by_due_date_range_for_board(
        &self,
        owner: &TodoOwner,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<TodoItem>, TodoError> {
        if start > end {
            return Err(TodoError::bad_request(
                "日期范围无效，开始日期不能晚于结束日期。",
            ));
        }
        let conn = self.connection()?;
        let mut items = query_items(&conn, owner)?
            .into_iter()
            .filter(|item| {
                todo_due_local_date(item).is_some_and(|date| start <= date && date <= end)
            })
            .collect::<Vec<_>>();
        sort_todos_by_created_desc(&mut items);
        sort_todo_all_board(&mut items);
        Ok(items)
    }

    /// 按归一化后的业务时间字段列出全部状态待办，排序与 `/todo all` 看板保持一致。
    pub fn list_all_by_date_filter_for_board(
        &self,
        owner: &TodoOwner,
        filter: TodoListDateFilter,
    ) -> Result<Vec<TodoItem>, TodoError> {
        if filter.start > filter.end {
            return Err(TodoError::bad_request(
                "日期范围无效，开始日期不能晚于结束日期。",
            ));
        }
        let conn = self.connection()?;
        let mut items = query_items(&conn, owner)?
            .into_iter()
            .filter(|item| todo_list_date_matches_range(item, filter))
            .collect::<Vec<_>>();
        sort_todos_by_created_desc(&mut items);
        sort_todo_all_board(&mut items);
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

    /// 批量完成本次待办，普通待办完成，重复待办推进下一次；整批在同一事务提交。
    ///
    /// 所有重复待办会先完成推进计划计算和草稿归一化，确认无错误后才写库。
    /// 因此任一重复规则或时间字段非法时，前面的普通待办也不会被部分完成。
    pub fn complete_by_ids_with_recurrence(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoCompleteProgressOutcome, TodoError> {
        enum CompletePlan {
            Complete { id: i64, id_text: String },
            Advance { id: i64, draft: TodoItemDraft },
        }

        let mut conn = self.connection()?;
        let now = now_iso_cn();
        let tx = conn.transaction().map_err(TodoError::from_sql)?;
        let mut plans = Vec::new();
        let mut skipped_ids = Vec::new();

        for id_text in ids.iter().map(|id| clean_todo_id(id)) {
            let Some(id) = parse_todo_db_id(&id_text) else {
                if !id_text.is_empty() {
                    skipped_ids.push(id_text);
                }
                continue;
            };
            let Some(item) = get_by_id_unlocked(&tx, owner, id)? else {
                skipped_ids.push(id_text);
                continue;
            };
            if item.status != TodoStatus::Pending {
                skipped_ids.push(id_text);
                continue;
            }
            if recurrence::is_recurring(&item) {
                let draft = normalize_draft(recurrence::advance_after_completion(&item)?)?;
                plans.push(CompletePlan::Advance { id, draft });
            } else {
                plans.push(CompletePlan::Complete { id, id_text });
            }
        }

        let mut completed = Vec::new();
        let mut advanced = Vec::new();
        for plan in plans {
            match plan {
                CompletePlan::Complete { id, id_text } => {
                    match complete_pending_unlocked(&tx, owner, id, &now)? {
                        Some(item) => completed.push(item),
                        None => skipped_ids.push(id_text),
                    }
                }
                CompletePlan::Advance { id, draft } => {
                    match update_pending_todo_unlocked(&tx, owner, id, draft, &now)? {
                        Some(item) => advanced.push(item),
                        None => skipped_ids.push(id.to_string()),
                    }
                }
            }
        }

        tx.commit().map_err(TodoError::from_sql)?;
        Ok(TodoCompleteProgressOutcome {
            completed,
            advanced,
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
        self.cancel_by_ids_with_status(owner, ids, TodoStatus::Completed)
    }

    /// 批量取消未完成待办事项（按 ID 列表匹配 Pending 项）。
    pub fn cancel_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkCancelOutcome, TodoError> {
        self.cancel_by_ids_with_status(owner, ids, TodoStatus::Pending)
    }

    fn cancel_by_ids_with_status(
        &self,
        owner: &TodoOwner,
        ids: &[String],
        expected_status: TodoStatus,
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
                        expected_status.as_str(),
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

    /// 物理删除进行中的待办事项（按 ID 列表匹配）。
    ///
    /// 删除确认只能删除发起确认时仍处于 Pending 的记录；如果确认期间记录状态变化，
    /// 这里会按 skipped 处理，避免过期确认越过用户当前状态授权。
    pub fn delete_pending_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
    ) -> Result<TodoBulkDeleteOutcome, TodoError> {
        self.delete_by_ids_with_status(owner, ids, TodoStatus::Pending)
    }

    /// 按 ID 物理删除任意状态的待办事项。
    ///
    /// 仅用于调用方明确授权删除任意当前状态的场景；确认链路应使用带状态条件的
    /// `delete_*_by_ids`，避免过期确认越过发起确认时的状态边界。
    pub fn delete_by_ids(
        &self,
        owner: &TodoOwner,
        ids: &[String],
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
                       AND scope_key = ?3",
                    params![id, owner.key.as_str(), owner.scope_key.as_str()],
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
        update_pending_todo_unlocked(&conn, owner, id, draft, &now)?
            .ok_or_else(|| TodoError::not_found("todo not found"))
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
                    due_date, due_at, reminder_at, time_precision, recurrence_kind,
                    recurrence_interval_days, recurrence_interval, recurrence_unit,
                    status, completed, created_at, updated_at,
                    completed_at, cancelled_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
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
                    item.reminder_at,
                    item.time_precision.as_str(),
                    item.recurrence_kind.as_str(),
                    i64::from(item.recurrence_interval_days),
                    i64::from(item.recurrence_interval),
                    item.recurrence_unit.as_str(),
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

fn sort_items_for_status(items: &mut [TodoItem], status: &TodoStatus) {
    match status {
        TodoStatus::Pending => sort_todos(items),
        TodoStatus::Completed => sort_completed_todos_desc(items),
        TodoStatus::Cancelled => sort_todos_by_created_desc(items),
    }
}

fn todo_due_matches_local_date(item: &TodoItem, date: NaiveDate) -> bool {
    todo_due_local_date(item).is_some_and(|due_date| due_date == date)
}

fn todo_list_date_matches_range(item: &TodoItem, filter: TodoListDateFilter) -> bool {
    let date = match filter.field {
        TodoListDateField::Planned => todo_due_local_date(item),
        TodoListDateField::CompletedAt => item
            .completed_at
            .as_deref()
            .and_then(local_date_from_timestamp),
        TodoListDateField::CancelledAt => item
            .cancelled_at
            .as_deref()
            .and_then(local_date_from_timestamp),
    };
    date.is_some_and(|date| filter.start <= date && date <= filter.end)
}

fn todo_due_local_date(item: &TodoItem) -> Option<NaiveDate> {
    // due_at 是最精确的计划时间；只有不存在 due_at 时才回退 due_date。
    if let Some(due_at) = item.due_at.as_deref().and_then(clean_optional) {
        return local_date_from_timestamp(&due_at);
    }
    item.due_date
        .as_deref()
        .and_then(clean_optional)
        .and_then(|due_date| local_date_from_timestamp(&due_date))
}

fn insert_todo_unlocked(
    conn: &Connection,
    owner: &TodoOwner,
    draft: TodoItemDraft,
    now: &str,
) -> Result<TodoItem, TodoError> {
    conn.execute(
        "INSERT INTO todos (
            owner_key, user_id, scope_key, title, detail, raw_text,
            due_date, due_at, reminder_at, time_precision, recurrence_kind,
            recurrence_interval_days, recurrence_interval, recurrence_unit,
            status, completed, created_at, updated_at,
            completed_at, cancelled_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 0, ?16, ?17, NULL, NULL)",
        params![
            owner.key.as_str(),
            owner.user_id.as_deref(),
            owner.scope_key.as_str(),
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
            TodoStatus::Pending.as_str(),
            now,
            now,
        ],
    )
    .map_err(TodoError::from_sql)?;
    let id = conn.last_insert_rowid();
    get_by_id_unlocked(conn, owner, id)?
        .ok_or_else(|| TodoError::io("todo disappeared after insert"))
}

fn complete_pending_unlocked(
    conn: &Connection,
    owner: &TodoOwner,
    id: i64,
    now: &str,
) -> Result<Option<TodoItem>, TodoError> {
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
        return Ok(None);
    }
    get_by_id_unlocked(conn, owner, id)?
        .map(Some)
        .ok_or_else(|| TodoError::io("todo disappeared after complete"))
}

fn update_pending_todo_unlocked(
    conn: &Connection,
    owner: &TodoOwner,
    id: i64,
    draft: TodoItemDraft,
    now: &str,
) -> Result<Option<TodoItem>, TodoError> {
    let affected = conn
        .execute(
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
                 updated_at = ?15
             WHERE id = ?1
               AND owner_key = ?2
               AND scope_key = ?3
               AND status = ?16",
            params![
                id,
                owner.key.as_str(),
                owner.scope_key.as_str(),
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
                now,
                TodoStatus::Pending.as_str(),
            ],
        )
        .map_err(TodoError::from_sql)?;
    if affected == 0 {
        return Ok(None);
    }
    get_by_id_unlocked(conn, owner, id)?
        .map(Some)
        .ok_or_else(|| TodoError::io("todo disappeared after edit"))
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

impl TodoRecurrenceKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Daily => "daily",
            Self::EveryNDays => "every_n_days",
            Self::Weekly => "weekly",
            Self::EveryNWeeks => "every_n_weeks",
            Self::Monthly => "monthly",
            Self::EveryNMonths => "every_n_months",
            Self::Yearly => "yearly",
            Self::EveryNYears => "every_n_years",
        }
    }

    fn from_db(value: &str) -> Result<Self, String> {
        match value {
            "none" => Ok(Self::None),
            "daily" => Ok(Self::Daily),
            "every_n_days" => Ok(Self::EveryNDays),
            "weekly" => Ok(Self::Weekly),
            "every_n_weeks" => Ok(Self::EveryNWeeks),
            "monthly" => Ok(Self::Monthly),
            "every_n_months" => Ok(Self::EveryNMonths),
            "yearly" => Ok(Self::Yearly),
            "every_n_years" => Ok(Self::EveryNYears),
            other => Err(format!("invalid todo recurrence kind `{other}`")),
        }
    }
}

impl TodoRecurrenceUnit {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    fn from_db(value: &str) -> Result<Self, String> {
        match value {
            "day" => Ok(Self::Day),
            "week" => Ok(Self::Week),
            "month" => Ok(Self::Month),
            "year" => Ok(Self::Year),
            other => Err(format!("invalid todo recurrence unit `{other}`")),
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
            reminder_at: item.reminder_at.clone(),
            time_precision: item.time_precision,
            recurrence_kind: item.recurrence_kind.clone(),
            recurrence_interval_days: item.recurrence_interval_days,
            recurrence_interval: item.recurrence_interval,
            recurrence_unit: item.recurrence_unit,
        }
    }

    /// 标记本次编辑显式清除重复规则，供归一化阶段跳过 raw_text 的重复规则推断。
    pub(crate) fn mark_explicit_no_recurrence(&mut self) {
        self.recurrence_kind = TodoRecurrenceKind::None;
        self.recurrence_interval_days = EXPLICIT_NO_RECURRENCE_INTERVAL_DAYS;
        self.recurrence_interval = 0;
        self.recurrence_unit = TodoRecurrenceUnit::Day;
    }

    pub(crate) fn take_explicit_no_recurrence_marker(&mut self) -> bool {
        if matches!(self.recurrence_kind, TodoRecurrenceKind::None)
            && self.recurrence_interval_days == EXPLICIT_NO_RECURRENCE_INTERVAL_DAYS
        {
            self.recurrence_interval_days = 0;
            self.recurrence_interval = 0;
            self.recurrence_unit = TodoRecurrenceUnit::Day;
            return true;
        }
        false
    }
}

/// 清理可选字符串字段。
///
/// 保留在 mod 顶层是因为 `TodoStore::owner` 与各子模块（排序、时间显示、草稿规范化、
/// private scope 解析）都需要复用同一套空白/空串归一语义。
fn clean_optional(value: &str) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_default_recurrence_unit(value: &TodoRecurrenceUnit) -> bool {
    matches!(value, TodoRecurrenceUnit::Day)
}

#[cfg(test)]
mod tests;

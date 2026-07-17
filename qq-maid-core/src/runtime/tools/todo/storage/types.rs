//! Todo storage public data types and storage-local conversions.

use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::storage::database::{DatabaseError, SqliteDatabase};

const EXPLICIT_NO_RECURRENCE_INTERVAL_DAYS: u32 = u32::MAX;

/// 待办事项的状态。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    Completed,
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
    // 分钟/小时级周期任务（例如“每 5 分钟”“每 2 小时”）；
    // 完成后才推进下一周期，行为与 Daily/EveryNDays 一致，不引入自主循环。
    EveryNMinutes,
    EveryNHours,
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
    // 分钟/小时级重复单位，配合 EveryNMinutes / EveryNHours 使用。
    Minute,
    Hour,
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
}

impl TodoListDateField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::CompletedAt => "completed_at",
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

/// 用户主动查询 Todo 时使用的默认展示数量。
pub const TODO_QUERY_DEFAULT_LIMIT: usize = 10;
/// 服务端允许的单次查询上限，为后续分页保留显式边界。
pub const TODO_QUERY_MAX_LIMIT: usize = 20;

/// Todo 列表查询状态范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TodoQueryStatus {
    #[default]
    Pending,
    Completed,
    All,
}

/// Todo 列表时间筛选。日期范围按请求时区下的本地自然日解释。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoQueryTimeFilter {
    DateRange {
        start: NaiveDate,
        end: NaiveDate,
        field: TodoListDateField,
    },
    Overdue {
        now: DateTime<FixedOffset>,
    },
    NoDueDate,
}

/// 命令、自然语言和 Tool 共用的 Todo 查询模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoQuery {
    pub status: TodoQueryStatus,
    pub time: Option<TodoQueryTimeFilter>,
    pub keyword: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

impl Default for TodoQuery {
    fn default() -> Self {
        Self {
            status: TodoQueryStatus::Pending,
            time: None,
            keyword: None,
            limit: TODO_QUERY_DEFAULT_LIMIT,
            offset: 0,
        }
    }
}

/// Todo 查询结果同时携带真实匹配总数，避免展示层静默截断。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoQueryPage {
    pub items: Vec<TodoItem>,
    pub total_count: usize,
    pub limit: usize,
    pub offset: usize,
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
    pub(super) database: SqliteDatabase,
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
    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self {
            code: "bad_request",
            message: message.into(),
        }
    }

    /// 构造资源未找到错误。
    pub(super) fn not_found(message: impl Into<String>) -> Self {
        Self {
            code: "not_found",
            message: message.into(),
        }
    }

    /// 构造 I/O 错误。
    pub(super) fn io(message: impl Into<String>) -> Self {
        Self {
            code: "io_error",
            message: message.into(),
        }
    }

    pub(super) fn data(message: impl Into<String>) -> Self {
        Self {
            code: "data_error",
            message: message.into(),
        }
    }

    pub(super) fn from_database(err: DatabaseError) -> Self {
        Self {
            code: err.code(),
            message: err.message().to_owned(),
        }
    }

    pub(super) fn from_sql(err: rusqlite::Error) -> Self {
        match err {
            rusqlite::Error::FromSqlConversionFailure(_, _, inner) => {
                Self::data(format!("sqlite data mapping failed: {inner}"))
            }
            other => Self::io(format!("sqlite failed: {other}")),
        }
    }
}

impl TodoStatus {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
        }
    }

    #[cfg(test)]
    pub(super) fn completed_flag(&self) -> i64 {
        i64::from(matches!(self, Self::Completed))
    }

    pub(super) fn from_db(value: &str) -> Result<Self, String> {
        match value {
            "pending" => Ok(Self::Pending),
            "completed" => Ok(Self::Completed),
            other => Err(format!("invalid todo status `{other}`")),
        }
    }
}

impl TodoTimePrecision {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Date => "date",
            Self::DateTime => "date_time",
            Self::Inferred => "inferred",
        }
    }

    pub(super) fn from_db(value: &str) -> Result<Self, String> {
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
    pub(super) fn as_str(&self) -> &'static str {
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
            Self::EveryNMinutes => "every_n_minutes",
            Self::EveryNHours => "every_n_hours",
        }
    }

    pub(super) fn from_db(value: &str) -> Result<Self, String> {
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
            "every_n_minutes" => Ok(Self::EveryNMinutes),
            "every_n_hours" => Ok(Self::EveryNHours),
            other => Err(format!("invalid todo recurrence kind `{other}`")),
        }
    }
}

impl TodoRecurrenceUnit {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
            Self::Minute => "minute",
            Self::Hour => "hour",
        }
    }

    pub(super) fn from_db(value: &str) -> Result<Self, String> {
        match value {
            "day" => Ok(Self::Day),
            "week" => Ok(Self::Week),
            "month" => Ok(Self::Month),
            "year" => Ok(Self::Year),
            "minute" => Ok(Self::Minute),
            "hour" => Ok(Self::Hour),
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

    pub(crate) fn has_explicit_no_recurrence_marker(&self) -> bool {
        matches!(self.recurrence_kind, TodoRecurrenceKind::None)
            && self.recurrence_interval_days == EXPLICIT_NO_RECURRENCE_INTERVAL_DAYS
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
/// `TodoStore::owner` 与各子模块都复用同一套空白/空串归一语义。
pub(super) fn clean_optional(value: &str) -> Option<String> {
    let value = value.trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn is_default_recurrence_unit(value: &TodoRecurrenceUnit) -> bool {
    matches!(value, TodoRecurrenceUnit::Day)
}

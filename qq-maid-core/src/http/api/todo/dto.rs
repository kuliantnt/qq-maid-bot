//! Todo 管理 API 的领域 DTO。

use chrono::NaiveDate;
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};

use crate::runtime::tools::todo::{
    TodoEditPatch, TodoItem, TodoItemDraft, TodoListDateFilter, TodoQuery, TodoQueryStatus,
    TodoQueryTimeFilter, TodoRecurrenceKind, TodoRecurrenceUnit, TodoStatus, TodoTimePrecision,
    resolve_todo_list_date_filter,
};

use super::super::common::{ApiError, PaginationRequest, ValidatedPagination};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateTodoRequest {
    pub title: String,
    pub detail: Option<String>,
    pub due_date: Option<String>,
    pub due_at: Option<String>,
    pub recurrence_kind: Option<TodoRecurrenceKind>,
    pub recurrence_interval_days: Option<u32>,
    pub recurrence_interval: Option<u32>,
    pub recurrence_unit: Option<TodoRecurrenceUnit>,
}

impl CreateTodoRequest {
    pub(super) fn into_draft(self) -> Result<TodoItemDraft, ApiError> {
        validate_optional_date(self.due_date.as_deref())?;
        validate_optional_datetime(self.due_at.as_deref())?;
        Ok(TodoItemDraft {
            title: self.title,
            detail: self.detail,
            raw_text: None,
            due_date: self.due_date,
            due_at: self.due_at,
            reminder_at: None,
            time_precision: TodoTimePrecision::None,
            recurrence_kind: self.recurrence_kind.unwrap_or_default(),
            recurrence_interval_days: self.recurrence_interval_days.unwrap_or_default(),
            recurrence_interval: self.recurrence_interval.unwrap_or_default(),
            recurrence_unit: self.recurrence_unit.unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TodoListStatus {
    Pending,
    Completed,
    All,
}

impl TodoListStatus {
    fn query_status(self) -> TodoQueryStatus {
        match self {
            Self::Pending => TodoQueryStatus::Pending,
            Self::Completed => TodoQueryStatus::Completed,
            Self::All => TodoQueryStatus::All,
        }
    }

    fn storage_status(self) -> Option<TodoStatus> {
        match self {
            Self::Pending => Some(TodoStatus::Pending),
            Self::Completed => Some(TodoStatus::Completed),
            Self::All => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TodoTimeFilterRequest {
    Overdue,
    NoDueDate,
}

#[derive(Debug, Deserialize)]
pub(super) struct ListTodoRequest {
    /// 使用 flatten 保持 `{page, page_size}` 顶层协议，同时复用公共分页 DTO。
    #[serde(flatten)]
    pub pagination: PaginationRequest,
    pub status: Option<TodoListStatus>,
    pub due_date: Option<String>,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
    pub time_filter: Option<TodoTimeFilterRequest>,
    pub keyword: Option<String>,
    pub recurring: Option<bool>,
}

impl ListTodoRequest {
    pub(super) fn pagination(&self) -> Result<ValidatedPagination, ApiError> {
        self.pagination.clone().validate()
    }

    pub(super) fn into_query(self, pagination: ValidatedPagination) -> Result<TodoQuery, ApiError> {
        let status = self.status.unwrap_or(TodoListStatus::All);
        let due_date = self.due_date.as_deref().map(parse_date).transpose()?;
        let date_range = match (self.date_start.as_deref(), self.date_end.as_deref()) {
            (None, None) => None,
            (Some(start), Some(end)) => Some((parse_date(start)?, parse_date(end)?)),
            _ => {
                return Err(ApiError::validation(
                    "date_start and date_end must be provided together",
                ));
            }
        };
        if due_date.is_some() && date_range.is_some() {
            return Err(ApiError::validation(
                "due_date cannot be combined with date_start/date_end",
            ));
        }
        if self.time_filter.is_some() && (due_date.is_some() || date_range.is_some()) {
            return Err(ApiError::validation(
                "time_filter cannot be combined with date filters",
            ));
        }
        let date_filter =
            resolve_todo_list_date_filter(status.storage_status(), due_date, date_range)
                .map_err(|error| ApiError::validation(error.message()))?;
        let query_status = match (self.status, self.time_filter) {
            (None, Some(TodoTimeFilterRequest::Overdue)) => TodoQueryStatus::Pending,
            _ => status.query_status(),
        };
        let time = match self.time_filter {
            Some(TodoTimeFilterRequest::Overdue) => Some(TodoQueryTimeFilter::Overdue {
                now: qq_maid_common::time_context::parse_local_datetime_for_comparison(
                    qq_maid_common::time_context::request_time_context().current_time(),
                )
                .expect("request time context must contain a valid local datetime"),
            }),
            Some(TodoTimeFilterRequest::NoDueDate) => Some(TodoQueryTimeFilter::NoDueDate),
            None => date_filter.map(time_filter_from_date),
        };
        let keyword = self
            .keyword
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let limit = usize::try_from(pagination.page_size())
            .map_err(|_| ApiError::validation("page_size is too large"))?;
        let offset = usize::try_from(pagination.offset())
            .map_err(|_| ApiError::validation("pagination offset is too large"))?;
        Ok(TodoQuery {
            status: query_status,
            time,
            keyword,
            recurring: self.recurring,
            limit,
            offset,
        })
    }
}

fn time_filter_from_date(filter: TodoListDateFilter) -> TodoQueryTimeFilter {
    TodoQueryTimeFilter::DateRange {
        start: filter.start,
        end: filter.end,
        field: filter.field,
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum TodoId {
    String(String),
    Number(i64),
}

impl TodoId {
    pub(super) fn into_string(self) -> Result<String, ApiError> {
        let value = match self {
            Self::String(value) => value.trim().to_owned(),
            Self::Number(value) if value > 0 => value.to_string(),
            Self::Number(_) => return Err(ApiError::validation("id must be a positive integer")),
        };
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ApiError::validation("id must be a positive integer"));
        }
        Ok(value)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GetTodoRequest {
    pub id: TodoId,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DeleteTodoRequest {
    pub id: TodoId,
}

#[derive(Debug, Default)]
pub(super) enum PatchField<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for PatchField<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateTodoRequest {
    pub id: TodoId,
    #[serde(default)]
    pub title: PatchField<String>,
    #[serde(default)]
    pub detail: PatchField<String>,
    #[serde(default)]
    pub due_date: PatchField<String>,
    #[serde(default)]
    pub due_at: PatchField<String>,
    pub status: Option<TodoStatus>,
    pub recurrence_kind: Option<TodoRecurrenceKind>,
    pub recurrence_interval_days: Option<u32>,
    pub recurrence_interval: Option<u32>,
    pub recurrence_unit: Option<TodoRecurrenceUnit>,
}

impl UpdateTodoRequest {
    pub(super) fn into_parts(
        self,
    ) -> Result<(String, crate::runtime::tools::todo::TodoManagementUpdate), ApiError> {
        let id = self.id.into_string()?;
        let title = match self.title {
            PatchField::Missing => None,
            PatchField::Null => return Err(ApiError::validation("title cannot be null")),
            PatchField::Value(value) => Some(value),
        };
        let detail = nullable_patch(self.detail);
        let due_date = nullable_patch(self.due_date);
        let due_at = nullable_patch(self.due_at);
        if let Some(value) = due_date.as_deref().filter(|value| !value.is_empty()) {
            validate_optional_date(Some(value))?;
        }
        if let Some(value) = due_at.as_deref().filter(|value| !value.is_empty()) {
            validate_optional_datetime(Some(value))?;
        }
        let update = crate::runtime::tools::todo::TodoManagementUpdate {
            fields: TodoEditPatch {
                title,
                detail,
                due_date,
                due_at,
                recurrence_kind: self.recurrence_kind,
                recurrence_interval_days: self.recurrence_interval_days,
                recurrence_interval: self.recurrence_interval,
                recurrence_unit: self.recurrence_unit,
                ..Default::default()
            },
            status: self.status,
        };
        if !update.has_field_changes() && update.status.is_none() {
            return Err(ApiError::validation(
                "at least one updatable field must be provided",
            ));
        }
        Ok((id, update))
    }
}

fn nullable_patch(field: PatchField<String>) -> Option<String> {
    match field {
        PatchField::Missing => None,
        PatchField::Null => Some(String::new()),
        PatchField::Value(value) => Some(value),
    }
}

#[derive(Debug, Serialize)]
pub(super) struct TodoDto {
    id: String,
    title: String,
    detail: Option<String>,
    due_date: Option<String>,
    due_at: Option<String>,
    reminder_at: Option<String>,
    time_precision: TodoTimePrecision,
    recurrence_kind: TodoRecurrenceKind,
    recurrence_interval_days: u32,
    recurrence_interval: u32,
    recurrence_unit: TodoRecurrenceUnit,
    status: TodoStatus,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

impl From<TodoItem> for TodoDto {
    fn from(item: TodoItem) -> Self {
        Self {
            id: item.id,
            title: item.title,
            detail: item.detail,
            due_date: item.due_date,
            due_at: item.due_at,
            reminder_at: item.reminder_at,
            time_precision: item.time_precision,
            recurrence_kind: item.recurrence_kind,
            recurrence_interval_days: item.recurrence_interval_days,
            recurrence_interval: item.recurrence_interval,
            recurrence_unit: item.recurrence_unit,
            status: item.status,
            created_at: item.created_at,
            updated_at: item.updated_at,
            completed_at: item.completed_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct DeleteTodoResponse {
    pub id: String,
    pub deleted: bool,
}

fn parse_date(value: &str) -> Result<NaiveDate, ApiError> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map_err(|_| ApiError::validation("date must use YYYY-MM-DD format"))
}

fn validate_optional_date(value: Option<&str>) -> Result<(), ApiError> {
    if let Some(value) = value {
        parse_date(value)?;
    }
    Ok(())
}

fn validate_optional_datetime(value: Option<&str>) -> Result<(), ApiError> {
    if let Some(value) = value
        && qq_maid_common::time_context::parse_local_datetime_for_comparison(value).is_none()
    {
        return Err(ApiError::validation(
            "datetime must be RFC3339 or YYYY-MM-DD HH:MM:SS",
        ));
    }
    Ok(())
}

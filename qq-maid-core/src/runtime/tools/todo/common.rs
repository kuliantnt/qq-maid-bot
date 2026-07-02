//! Todo Tool 共享常量、选择/引用类型与参数解析 helper。
//!
//! 这里只承载“模型 JSON 参数 -> 内部结构”的纯解析与校验，不依赖
//! `TodoStore` / `SessionRecord`，便于 prepare 与 execute 复用同一套边界。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use qq_maid_llm::tool::ToolOutput;

use crate::{
    error::LlmError,
    runtime::todo::{TodoEditPatch, TodoTimePrecision},
};

// Tool 名常量；metadata 必须返回与 Tool Loop 路由完全一致的 name。
pub(super) const LIST_TODOS_TOOL_NAME: &str = "list_todos";
pub(super) const GET_TODO_TOOL_NAME: &str = "get_todo";
pub(super) const CREATE_TODO_TOOL_NAME: &str = "create_todo";
pub(super) const COMPLETE_TODOS_TOOL_NAME: &str = "complete_todos";
pub(super) const EDIT_TODO_TOOL_NAME: &str = "edit_todo";
pub(super) const CANCEL_TODO_TOOL_NAME: &str = "cancel_todo";
pub(super) const RESTORE_TODOS_TOOL_NAME: &str = "restore_todos";
pub(super) const DELETE_TODOS_TOOL_NAME: &str = "delete_todos";

// 输入上限；超长直接拒绝，避免把异常长参数带进 pending / 存储。
pub(super) const TODO_TOOL_MAX_NUMBERS: usize = 20;
pub(super) const TODO_TOOL_MAX_BATCH_CREATE_ITEMS: usize = 20;
pub(super) const TODO_TOOL_MAX_TEXT_CHARS: usize = 500;

// 引用关键字；模型只能用 "last" 触发最近对象引用，不能传内部 ID。
pub(super) const TODO_REFERENCE_LAST: &str = "last";

// 面向模型的错误码；Tool 必须返回结构化失败而不是抛 Err，避免
// 普通 retry 把本应呈现给用户的语义错误升级为模型重试。
pub(super) const TODO_VISIBLE_NUMBERS_UNAVAILABLE_CODE: &str = "todo_visible_numbers_unavailable";
pub(super) const TODO_REFERENCE_UNAVAILABLE_CODE: &str = "todo_reference_unavailable";
pub(super) const TODO_REFERENCE_INVALID_STATE_CODE: &str = "todo_reference_invalid_state";
pub(super) const TODO_SELECTION_NOT_FOUND_CODE: &str = "todo_selection_not_found";
pub(super) const TODO_DELETE_MIXED_STATUS_CODE: &str = "todo_delete_mixed_status";

// prepare 阶段写进 arguments 的预解析键；以下划线开头，避免与模型参数冲突。
pub(super) const PREBOUND_SELECTION_KEY: &str = "_resolved_selection";
pub(super) const PREBOUND_SINGLE_ID_KEY: &str = "_resolved_todo_id";
pub(super) const PREBOUND_SINGLE_LABEL_KEY: &str = "_resolved_label";
pub(super) const PREBOUND_EDIT_DRAFT_KEY: &str = "_resolved_edit_draft";
pub(super) const PREBOUND_ERROR_OUTPUT_KEY: &str = "_error_output";

// dedup 历史在 session.extra 里的键与上限；用于 replay 同一 call_id 时复用结果，
// 避免重复 pending 抢占单槽位。
pub(super) const TODO_DEDUP_HISTORY_KEY: &str = "tool_todo_dedup_history";
pub(super) const TODO_DEDUP_HISTORY_LIMIT: usize = 32;

/// 用户引用最近操作对象的语义；模型永远拿不到内部 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum TodoReference {
    Last,
}

impl TodoReference {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Last => TODO_REFERENCE_LAST,
        }
    }
}

/// 工具调用层对操作目标的请求形式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TodoSelectionRequest {
    Numbers(Vec<usize>),
    Reference(TodoReference),
}

/// 面向模型输出的可见编号或最近对象引用标签。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum TodoSelectionLabel {
    Number(usize),
    Reference(TodoReference),
}

/// prepare 阶段序列化的单条编号匹配结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PreparedSelectionMatch {
    pub label: TodoSelectionLabel,
    pub id: String,
}

/// prepare 阶段写回 arguments 的预解析选择结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct PreparedResolvedSelection {
    pub labels: Vec<TodoSelectionLabel>,
    pub matched: Vec<PreparedSelectionMatch>,
    pub missing: Vec<TodoSelectionLabel>,
    pub error_output: Option<Value>,
}

/// replay 时记录的 (call_id, 参数, 输出) 条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct TodoToolDedupEntry {
    pub call_id: String,
    pub arguments: Value,
    pub output: Value,
}

/// `list_todos` 查询的状态参数。
#[derive(Debug, Clone, Copy)]
pub(super) enum TodoToolListStatus {
    Pending,
    Completed,
    Cancelled,
    All,
}

impl TodoToolListStatus {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::All => "all",
        }
    }

    pub(super) fn query_type(self) -> &'static str {
        match self {
            Self::Pending => "list",
            Self::Completed => "completed-list",
            Self::Cancelled => "cancelled-list",
            Self::All => "all",
        }
    }

    pub(super) fn condition(self) -> &'static str {
        match self {
            Self::Pending => "",
            Self::Completed => "已完成列表",
            Self::Cancelled => "已取消列表",
            Self::All => "全部待办",
        }
    }
}

pub(super) fn todo_tool_error(err: crate::runtime::todo::TodoError) -> LlmError {
    LlmError::new(err.code().to_owned(), err.message().to_owned(), "todo_tool")
}

pub(super) fn session_tool_error(err: crate::runtime::session::SessionError) -> LlmError {
    LlmError::new(err.code().to_owned(), err.message().to_owned(), "todo_tool")
}

pub(super) fn bad_tool_arguments(message: impl Into<String>) -> LlmError {
    LlmError::new("bad_tool_arguments", message, "tool")
}

pub(super) fn todo_tool_error_output(error_code: &str, message: &str) -> ToolOutput {
    ToolOutput::json(json!({
        "ok": false,
        "error_code": error_code,
        "message": message,
    }))
}

/// 解析 `list_todos` 的 status 参数。
pub(super) fn todo_status_argument(
    arguments: &Value,
    key: &str,
) -> Result<TodoToolListStatus, LlmError> {
    match arguments.get(key).and_then(Value::as_str) {
        Some("pending") => Ok(TodoToolListStatus::Pending),
        Some("completed") => Ok(TodoToolListStatus::Completed),
        Some("cancelled") => Ok(TodoToolListStatus::Cancelled),
        Some("all") => Ok(TodoToolListStatus::All),
        _ => Err(bad_tool_arguments(
            "status must be pending/completed/cancelled/all",
        )),
    }
}

/// 多编号 + reference 的 schema，complete/restore/delete 复用。
pub(super) fn number_list_or_reference_schema(description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "numbers": todo_numbers_schema(description),
            "selection_text": todo_selection_text_schema(),
            "reference": todo_reference_schema("当用户说“刚才那个 / 它 / 恢复的那个 / 刚完成的”时传 \"last\"；与 numbers/selection_text 三选一。")
        },
        "required": ["numbers", "selection_text", "reference"],
        "additionalProperties": false
    })
}

/// 单项编号 + reference 的 schema，get/edit 等只允许解析出一条 Todo。
pub(super) fn single_number_or_reference_schema(number_description: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "number": {
                "type": ["integer", "null"],
                "minimum": 1,
                "description": number_description
            },
            "numbers": todo_numbers_schema("同 number，只能包含一个 visible_number；保留用于复用通用选择器。"),
            "selection_text": todo_selection_text_schema(),
            "reference": todo_reference_schema("当用户说“刚才那个 / 它 / 恢复的那个 / 刚完成的”时传 \"last\"；与 number/numbers/selection_text 三选一。")
        },
        "required": ["number", "numbers", "selection_text", "reference"],
        "additionalProperties": false
    })
}

pub(super) fn todo_numbers_schema(description: &str) -> Value {
    json!({
        "type": ["array", "null"],
        "description": description,
        "minItems": 1,
        "maxItems": TODO_TOOL_MAX_NUMBERS,
        "items": {
            "type": "integer",
            "minimum": 1
        }
    })
}

pub(super) fn todo_selection_text_schema() -> Value {
    json!({
        "type": ["string", "null"],
        "description": "用户显式给出的编号文本，例如 \"1-5\"、\"1,3,5\"、\"1 到 5\"。仅在 numbers 无法表达原文范围时使用；与 numbers/reference 三选一。"
    })
}

pub(super) fn todo_reference_schema(description: &str) -> Value {
    json!({
        "type": ["string", "null"],
        "enum": [TODO_REFERENCE_LAST, null],
        "description": description
    })
}

/// 从 numbers/reference 互斥参数解析选择请求；`allow_many=false` 时强制单条。
pub(super) fn todo_selection_request(
    arguments: &Value,
    allow_many: bool,
) -> Result<TodoSelectionRequest, LlmError> {
    let numbers = optional_number_list(arguments, "numbers")?;
    let single_number = optional_single_number_as_list(arguments, "number")?;
    let selection_text_numbers = optional_selection_text_numbers(arguments, "selection_text")?;
    let reference = optional_reference(arguments, "reference")?;
    let selected_count = usize::from(numbers.is_some())
        + usize::from(single_number.is_some())
        + usize::from(selection_text_numbers.is_some())
        + usize::from(reference.is_some());
    if selected_count != 1 {
        return Err(bad_tool_arguments(
            "exactly one of numbers/number/selection_text/reference is required",
        ));
    }
    let numbers = numbers.or(single_number).or(selection_text_numbers);
    match (numbers, reference) {
        (Some(numbers), None) => {
            if !allow_many && numbers.len() != 1 {
                return Err(bad_tool_arguments("numbers must contain exactly one item"));
            }
            Ok(TodoSelectionRequest::Numbers(numbers))
        }
        (None, Some(reference)) => Ok(TodoSelectionRequest::Reference(reference)),
        (Some(_), Some(_)) => Err(bad_tool_arguments(
            "numbers and reference are mutually exclusive",
        )),
        (None, None) => Err(bad_tool_arguments(
            "either numbers or reference is required",
        )),
    }
}

fn optional_single_number_as_list(
    arguments: &Value,
    key: &str,
) -> Result<Option<Vec<usize>>, LlmError> {
    optional_positive_usize(arguments, key).map(|value| value.map(|number| vec![number]))
}

fn optional_selection_text_numbers(
    arguments: &Value,
    key: &str,
) -> Result<Option<Vec<usize>>, LlmError> {
    let Some(text) = optional_text(arguments, key)? else {
        return Ok(None);
    };
    Ok(Some(parse_selection_text(&text)?))
}

fn parse_selection_text(text: &str) -> Result<Vec<usize>, LlmError> {
    let compact = text
        .trim()
        .replace(['，', '、'], ",")
        .replace(['～', '—', '－'], "-")
        .replace("到", "-")
        .replace("至", "-");
    if compact.contains('-') && !compact.contains(',') {
        let parts = compact.split('-').collect::<Vec<_>>();
        if parts.len() == 2 {
            let start = parse_visible_number_token(parts[0])?;
            let end = parse_visible_number_token(parts[1])?;
            if start == 0 || end == 0 || start > end {
                return Err(bad_tool_arguments("selection range is invalid"));
            }
            let count = end - start + 1;
            if count > TODO_TOOL_MAX_NUMBERS {
                return Err(bad_tool_arguments("numbers length is out of range"));
            }
            return Ok((start..=end).collect());
        }
    }
    let mut numbers = Vec::new();
    for part in compact.split(',') {
        let number = parse_visible_number_token(part)?;
        if number == 0 {
            return Err(bad_tool_arguments("selection numbers must be positive"));
        }
        if !numbers.contains(&number) {
            numbers.push(number);
        }
    }
    if numbers.is_empty() || numbers.len() > TODO_TOOL_MAX_NUMBERS {
        return Err(bad_tool_arguments("numbers length is out of range"));
    }
    Ok(numbers)
}

fn parse_visible_number_token(value: &str) -> Result<usize, LlmError> {
    let token = value
        .trim()
        .trim_start_matches('第')
        .trim_end_matches(['条', '个', '项'])
        .trim();
    token
        .parse::<usize>()
        .map_err(|_| bad_tool_arguments("selection_text must contain explicit visible numbers"))
}

/// 从 number/reference 互斥参数解析单条选择请求。
pub(super) fn single_todo_selection_request(
    arguments: &Value,
) -> Result<TodoSelectionRequest, LlmError> {
    let number = optional_positive_usize(arguments, "number")?;
    let reference = optional_reference(arguments, "reference")?;
    match (number, reference) {
        (Some(number), None) => Ok(TodoSelectionRequest::Numbers(vec![number])),
        (None, Some(reference)) => Ok(TodoSelectionRequest::Reference(reference)),
        (Some(_), Some(_)) => Err(bad_tool_arguments(
            "number and reference are mutually exclusive",
        )),
        (None, None) => Err(bad_tool_arguments("either number or reference is required")),
    }
}

fn optional_number_list(arguments: &Value, key: &str) -> Result<Option<Vec<usize>>, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) if values.is_empty() => Ok(None),
        Some(Value::Array(values)) => Ok(Some(parse_number_list(values)?)),
        _ => Err(bad_tool_arguments(format!(
            "{key} must be an array or null"
        ))),
    }
}

fn parse_number_list(values: &[Value]) -> Result<Vec<usize>, LlmError> {
    if values.is_empty() || values.len() > TODO_TOOL_MAX_NUMBERS {
        return Err(bad_tool_arguments("numbers length is out of range"));
    }
    let mut numbers = Vec::new();
    for value in values {
        let number = value
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| bad_tool_arguments("numbers must contain positive integers"))?;
        if !numbers.contains(&number) {
            numbers.push(number);
        }
    }
    Ok(numbers)
}

fn optional_positive_usize(arguments: &Value, key: &str) -> Result<Option<usize>, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => arguments
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| bad_tool_arguments(format!("{key} must be a positive integer"))),
    }
}

fn optional_reference(arguments: &Value, key: &str) -> Result<Option<TodoReference>, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.trim().is_empty() => Ok(None),
        Some(Value::String(value)) => match value.trim() {
            TODO_REFERENCE_LAST => Ok(Some(TodoReference::Last)),
            _ => Err(bad_tool_arguments(format!(
                "{key} must be \"last\" or null"
            ))),
        },
        _ => Err(bad_tool_arguments(format!("{key} must be string or null"))),
    }
}

/// 必填非空文本；超过长度上限直接拒绝。
pub(super) fn required_non_empty_text(arguments: &Value, key: &str) -> Result<String, LlmError> {
    let value = arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| bad_tool_arguments(format!("{key} must be a non-empty string")))?;
    if value.chars().count() > TODO_TOOL_MAX_TEXT_CHARS {
        return Err(bad_tool_arguments(format!("{key} is too long")));
    }
    Ok(value.to_owned())
}

/// 可选文本；空串/null 归 None，超长拒绝。
pub(super) fn optional_text(arguments: &Value, key: &str) -> Result<Option<String>, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else if value.chars().count() > TODO_TOOL_MAX_TEXT_CHARS {
                Err(bad_tool_arguments(format!("{key} is too long")))
            } else {
                Ok(Some(value.to_owned()))
            }
        }
        _ => Err(bad_tool_arguments(format!("{key} must be string or null"))),
    }
}

/// `time_precision` 字段未传时默认 None；edit 补丁路径用 `optional_edit_time_precision`。
pub(super) fn optional_time_precision(
    arguments: &Value,
    key: &str,
) -> Result<TodoTimePrecision, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(TodoTimePrecision::None),
        Some(Value::String(value)) => match value.as_str() {
            "none" => Ok(TodoTimePrecision::None),
            "date" => Ok(TodoTimePrecision::Date),
            "date_time" => Ok(TodoTimePrecision::DateTime),
            "inferred" => Ok(TodoTimePrecision::Inferred),
            _ => Err(bad_tool_arguments("invalid time_precision")),
        },
        _ => Err(bad_tool_arguments(format!("{key} must be string or null"))),
    }
}

pub(super) fn optional_edit_time_precision(
    arguments: &Value,
    key: &str,
) -> Result<Option<TodoTimePrecision>, LlmError> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => match value.as_str() {
            "none" => Ok(Some(TodoTimePrecision::None)),
            "date" => Ok(Some(TodoTimePrecision::Date)),
            "date_time" => Ok(Some(TodoTimePrecision::DateTime)),
            "inferred" => Ok(Some(TodoTimePrecision::Inferred)),
            _ => Err(bad_tool_arguments("invalid time_precision")),
        },
        _ => Err(bad_tool_arguments(format!("{key} must be string or null"))),
    }
}

/// 把 edit_todo 的结构化参数适配成共享 `TodoEditPatch`。
pub(super) fn todo_edit_patch(arguments: &Value) -> Result<TodoEditPatch, LlmError> {
    Ok(TodoEditPatch {
        title: optional_text(arguments, "title")?,
        detail: optional_text(arguments, "detail")?,
        due_date: optional_text(arguments, "due_date")?,
        due_at: optional_text(arguments, "due_at")?,
        time_precision: optional_edit_time_precision(arguments, "time_precision")?,
    })
}

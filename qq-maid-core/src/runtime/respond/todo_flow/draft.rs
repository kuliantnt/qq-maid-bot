//! Todo 草稿和编辑补丁解析。
//!
//! LLM 在这里最多产出草稿或字段级补丁；真正写入仍必须经过 pending 确认和
//! `TodoStore` 规范化，避免自然语言解析直接改变持久化数据。

use serde_json::Value;

use crate::runtime::todo::{
    TodoItem, TodoItemDraft, TodoTimePrecision, enrich_draft_time_from_text,
};

use crate::{
    runtime::respond::common::{clean_string, extract_json_object},
    util::time_context::request_time_context,
};

/// 待办编辑操作的增量补丁，只包含需要修改的字段。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TodoEditPatch {
    /// 新的标题
    title: Option<String>,
    /// 新的详情
    detail: Option<String>,
    /// 新的截止日期（YYYY-MM-DD）
    due_date: Option<String>,
    /// 新的截止时间（YYYY-MM-DD HH:MM:SS）
    due_at: Option<String>,
    /// 时间精度
    time_precision: Option<TodoTimePrecision>,
}

impl TodoEditPatch {
    /// 是否存在至少一项修改。
    pub(super) fn has_changes(&self) -> bool {
        self.title.is_some()
            || self.detail.is_some()
            || self.due_date.is_some()
            || self.due_at.is_some()
            || self.time_precision.is_some()
    }
}

/// 从 LLM 返回的 JSON 中解析待办草稿，验证字段合法性。
pub(super) fn parse_todo_draft_json(
    raw: &str,
    user_text: &str,
    existing: Option<&TodoItem>,
) -> Result<TodoItemDraft, String> {
    let value = extract_json_object(raw).ok_or_else(|| {
        "唔，待办内容没有解析成功。请换一种更明确的写法，例如：/todo add 明天下午检查日志。"
            .to_owned()
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| "唔，待办解析结果格式不对，没有写入。请再试一次。".to_owned())?;

    let title = json_string_field(object, "title")
        .or_else(|| existing.map(|item| item.title.clone()))
        .and_then(clean_string)
        .ok_or_else(|| "唔，没解析出待办标题，没有写入。".to_owned())?;
    let detail = match object.get("detail") {
        Some(Value::Null) => None,
        Some(value) => value.as_str().map(str::to_owned).and_then(clean_string),
        None => existing.and_then(|item| item.detail.clone()),
    };
    let due_date = match object.get("due_date") {
        Some(Value::Null) => None,
        Some(value) => {
            let Some(value) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return Err("唔，待办日期字段格式不对，没有写入。".to_owned());
            };
            if !valid_todo_date(value) {
                return Err("唔，待办日期需要是 YYYY-MM-DD 格式，没有写入。".to_owned());
            }
            Some(value.to_owned())
        }
        None => existing.and_then(|item| item.due_date.clone()),
    };
    let due_at = match object.get("due_at") {
        Some(Value::Null) => None,
        Some(value) => {
            let Some(value) = value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return Err("唔，待办时间字段格式不对，没有写入。".to_owned());
            };
            if !valid_todo_datetime(value) {
                return Err("唔，待办时间需要是 YYYY-MM-DD HH:MM:SS 格式，没有写入。".to_owned());
            }
            Some(value.to_owned())
        }
        None => existing.and_then(|item| item.due_at.clone()),
    };
    let mut time_precision = match object.get("time_precision") {
        Some(Value::String(value)) => parse_todo_time_precision(value)?,
        Some(Value::Null) | None => existing
            .map(|item| item.time_precision.clone())
            .unwrap_or(TodoTimePrecision::None),
        _ => return Err("唔，待办时间精度字段格式不对，没有写入。".to_owned()),
    };
    if due_at.is_none() && due_date.is_none() {
        time_precision = TodoTimePrecision::None;
    } else if due_at.is_some() && matches!(time_precision, TodoTimePrecision::None) {
        time_precision = TodoTimePrecision::DateTime;
    } else if due_date.is_some() && matches!(time_precision, TodoTimePrecision::None) {
        time_precision = TodoTimePrecision::Date;
    }

    Ok(TodoItemDraft {
        title,
        detail,
        raw_text: Some(user_text.to_owned()),
        due_date,
        due_at,
        time_precision,
    })
}

pub(super) fn apply_todo_edit_patch(
    mut draft: TodoItemDraft,
    patch: TodoEditPatch,
    user_text: &str,
) -> TodoItemDraft {
    if let Some(title) = patch.title.and_then(clean_string) {
        draft.title = title;
    }
    if let Some(detail) = patch.detail {
        draft.detail = clean_string(detail);
    }
    if let Some(due_at) = patch.due_at.and_then(clean_string) {
        draft.due_at = Some(due_at);
        draft.due_date = patch.due_date.and_then(clean_string);
        draft.time_precision = patch.time_precision.unwrap_or(TodoTimePrecision::DateTime);
    } else if let Some(due_date) = patch.due_date.and_then(clean_string) {
        draft.due_date = Some(due_date);
        draft.due_at = None;
        draft.time_precision = patch.time_precision.unwrap_or(TodoTimePrecision::Date);
    } else if let Some(precision) = patch.time_precision {
        draft.time_precision = precision;
    }
    draft.raw_text = Some(user_text.to_owned());
    draft
}

/// 从 LLM 返回的 JSON 中解析待办编辑增量补丁。
pub(super) fn parse_todo_edit_patch_json(raw: &str) -> Result<TodoEditPatch, String> {
    let Some(value) = extract_json_object(raw) else {
        return Ok(TodoEditPatch::default());
    };
    let object = value
        .as_object()
        .ok_or_else(|| "唔，待办修改解析结果格式不对，没有更新待确认内容。".to_owned())?;

    let title = json_string_field(object, "title").and_then(clean_string);
    let detail = json_string_field(object, "detail").and_then(clean_string);
    let due_date = match object.get("due_date") {
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                None
            } else if valid_todo_date(value) {
                Some(value.to_owned())
            } else {
                return Err("唔，待办日期需要是 YYYY-MM-DD 格式，没有更新待确认内容。".to_owned());
            }
        }
        Some(Value::Null) | None => None,
        _ => return Err("唔，待办日期字段格式不对，没有更新待确认内容。".to_owned()),
    };
    let due_at = match object.get("due_at") {
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                None
            } else if valid_todo_datetime(value) {
                Some(value.to_owned())
            } else {
                return Err(
                    "唔，待办时间需要是 YYYY-MM-DD HH:MM:SS 格式，没有更新待确认内容。".to_owned(),
                );
            }
        }
        Some(Value::Null) | None => None,
        _ => return Err("唔，待办时间字段格式不对，没有更新待确认内容。".to_owned()),
    };
    let mut time_precision = match object.get("time_precision") {
        Some(Value::String(value)) => Some(parse_todo_time_precision(value)?),
        Some(Value::Null) | None => None,
        _ => return Err("唔，待办时间精度字段格式不对，没有更新待确认内容。".to_owned()),
    };
    if due_at.is_some() && time_precision.is_none() {
        time_precision = Some(TodoTimePrecision::DateTime);
    } else if due_date.is_some() && time_precision.is_none() {
        time_precision = Some(TodoTimePrecision::Date);
    }

    Ok(TodoEditPatch {
        title,
        detail,
        due_date,
        due_at,
        time_precision,
    })
}

pub(super) fn enrich_todo_edit_patch_time_from_text(patch: &mut TodoEditPatch, user_text: &str) {
    if patch.due_date.is_some() || patch.due_at.is_some() {
        return;
    }
    let mut draft = TodoItemDraft {
        title: "待办".to_owned(),
        detail: None,
        raw_text: None,
        due_date: None,
        due_at: None,
        time_precision: TodoTimePrecision::None,
    };
    let time_ctx = request_time_context();
    enrich_draft_time_from_text(&mut draft, user_text, &time_ctx);
    if draft.due_date.is_some() || draft.due_at.is_some() {
        patch.due_date = draft.due_date;
        patch.due_at = draft.due_at;
        patch.time_precision = Some(draft.time_precision);
    }
}

fn json_string_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object.get(key)?.as_str().map(str::to_owned)
}

fn parse_todo_time_precision(value: &str) -> Result<TodoTimePrecision, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "none" | "null" | "unspecified" => Ok(TodoTimePrecision::None),
        "date" => Ok(TodoTimePrecision::Date),
        "datetime" | "date_time" => Ok(TodoTimePrecision::DateTime),
        "inferred" | "guess" | "guessed" => Ok(TodoTimePrecision::Inferred),
        _ => Err("唔，待办时间精度只能是 none/date/datetime/inferred。".to_owned()),
    }
}

fn valid_todo_date(value: &str) -> bool {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

fn valid_todo_datetime(value: &str) -> bool {
    value.len() == 19
        && value.as_bytes().get(10) == Some(&b' ')
        && chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").is_ok()
}

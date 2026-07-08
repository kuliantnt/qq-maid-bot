//! 待办草稿规范化：校验必填、脱敏敏感文本、截断超长文本，并推断截止时间精度。
//!
//! 截断长度（标题 120、详情/原文 500）与现有快照展示一致，改动前需同步
//! 检查 session 快照与用户可见列表的展示宽度。敏感文本脱敏复用 session 层
//! 的 `redact_sensitive_text`，避免在 todo 存储层重复实现脱敏规则。

use qq_maid_common::{
    text::truncate_chars_trimmed as truncate_chars,
    time_context::{has_valid_ymd_date_prefix, is_valid_ymd_date},
};

use super::{
    TodoError, TodoItemDraft, TodoTimePrecision, clean_optional,
    recurrence::normalize_todo_recurrence_input,
};
use crate::storage::session::redact_sensitive_text;

/// 规范化待办草稿：校验必填字段、脱敏敏感文本、截断超长文本。
pub(super) fn normalize_draft(mut draft: TodoItemDraft) -> Result<TodoItemDraft, TodoError> {
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
    draft.reminder_at = draft.reminder_at.as_deref().and_then(clean_optional);
    if draft.due_at.is_none() {
        draft.due_at = draft.reminder_at.clone();
    }
    normalize_todo_recurrence_input(&mut draft)?;
    if draft.due_at.is_some() && matches!(draft.time_precision, TodoTimePrecision::None) {
        draft.time_precision = TodoTimePrecision::DateTime;
    } else if draft.due_date.is_some() && matches!(draft.time_precision, TodoTimePrecision::None) {
        draft.time_precision = TodoTimePrecision::Date;
    } else if draft.due_at.is_none() && draft.due_date.is_none() {
        draft.time_precision = TodoTimePrecision::None;
    }
    Ok(draft)
}

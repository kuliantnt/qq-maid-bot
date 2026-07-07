//! 待办时间显示与自然语言推断 helper。
//!
//! `display_*` 输出供用户可见列表与工具 JSON 使用；`infer_due_date_from_text` /
//! `enrich_draft_time_from_text` 在创建待办时把自然语言日期写入草稿，避免在
//! LLM / tool 层重复实现时间推断语义。时间格式化与日期校验复用
//! `qq-maid-common` / `util::time_context` 的统一入口，不做时区单独处理。

use super::{TodoItem, TodoItemDraft, TodoTimePrecision, clean_optional};
use crate::util::time_context::{
    self, DateInferencePrecision, RequestTimeContext, format_todo_time_for_display,
    infer_daypart_datetime_from_text,
};

/// 从用户文本中推断截止时间并填充到草稿中（仅当草稿尚未设置截止时间时生效）。
pub fn enrich_draft_time_from_text(
    draft: &mut TodoItemDraft,
    user_text: &str,
    ctx: &RequestTimeContext,
) {
    if draft.due_at.is_some() {
        return;
    }
    if let Some(daypart) = infer_daypart_datetime_from_text(user_text, ctx) {
        let due_date = draft
            .due_date
            .clone()
            .or_else(|| infer_due_date_from_text(user_text, ctx).map(|(date, _)| date))
            .unwrap_or_else(|| daypart.date.clone());
        draft.due_date = Some(due_date.clone());
        draft.due_at = Some(daypart.datetime_on_date(&due_date));
        draft.time_precision = TodoTimePrecision::DateTime;
        return;
    }
    if draft.due_date.is_none()
        && let Some((date, precision)) = infer_due_date_from_text(user_text, ctx)
    {
        draft.due_date = Some(date);
        draft.time_precision = precision;
    }
}

/// 把自然语言文本推断为 (日期字符串, 时间精度)，精度只区分 Date / Inferred。
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

/// 显示待办事项的截止时间（优先 due_at，其次 due_date），无截止时间显示“未指定”。
pub fn display_todo_time(item: &TodoItem) -> String {
    display_time_parts(item.due_date.as_deref(), item.due_at.as_deref())
}

/// 显示草稿的截止时间，语义同 `display_todo_time`。
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

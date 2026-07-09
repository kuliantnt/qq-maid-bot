//! 待办时间显示与自然语言推断的 Core 适配层。
//!
//! 通用时间解析、相对分钟/小时提醒推断、日期展示等实现放在
//! `qq_maid_common::time_context::todo_time`，这里只负责把 common 的纯字段结果
//! 映射到 Core 的 `TodoItem` / `TodoItemDraft` / `TodoTimePrecision` 业务类型。

use super::{TodoItem, TodoItemDraft, TodoTimePrecision};
use qq_maid_common::time_context::{
    RequestTimeContext, TodoTimeFields, TodoTimeInferencePrecision, display_todo_time_parts,
    enrich_todo_time_fields_from_text, infer_todo_due_date_from_text,
};

/// 从用户文本中推断截止时间并填充到草稿中（仅当草稿尚未设置截止时间时生效）。
pub fn enrich_draft_time_from_text(
    draft: &mut TodoItemDraft,
    user_text: &str,
    ctx: &RequestTimeContext,
) {
    let enriched = enrich_todo_time_fields_from_text(
        TodoTimeFields {
            due_date: draft.due_date.clone(),
            due_at: draft.due_at.clone(),
            reminder_at: draft.reminder_at.clone(),
        },
        precision_to_common(draft.time_precision),
        user_text,
        ctx,
    );

    draft.due_date = enriched.fields.due_date;
    draft.due_at = enriched.fields.due_at;
    draft.reminder_at = enriched.fields.reminder_at;
    draft.time_precision = precision_from_common(enriched.precision);
}

/// 把自然语言文本推断为 (日期字符串, 时间精度)，精度只区分 Date / Inferred。
pub fn infer_due_date_from_text(
    text: &str,
    ctx: &RequestTimeContext,
) -> Option<(String, TodoTimePrecision)> {
    infer_todo_due_date_from_text(text, ctx)
        .map(|(date, precision)| (date, precision_from_common(precision)))
}

/// 显示待办事项的截止时间（优先 due_at，其次 due_date），无截止时间显示“未指定”。
pub fn display_todo_time(item: &TodoItem) -> String {
    display_todo_time_parts(item.due_date.as_deref(), item.due_at.as_deref())
}

/// 显示草稿的截止时间，语义同 `display_todo_time`。
pub fn display_draft_time(draft: &TodoItemDraft) -> String {
    display_todo_time_parts(draft.due_date.as_deref(), draft.due_at.as_deref())
}

fn precision_to_common(value: TodoTimePrecision) -> TodoTimeInferencePrecision {
    match value {
        TodoTimePrecision::None => TodoTimeInferencePrecision::None,
        TodoTimePrecision::Date => TodoTimeInferencePrecision::Date,
        TodoTimePrecision::DateTime => TodoTimeInferencePrecision::DateTime,
        TodoTimePrecision::Inferred => TodoTimeInferencePrecision::Inferred,
    }
}

fn precision_from_common(value: TodoTimeInferencePrecision) -> TodoTimePrecision {
    match value {
        TodoTimeInferencePrecision::None => TodoTimePrecision::None,
        TodoTimeInferencePrecision::Date => TodoTimePrecision::Date,
        TodoTimeInferencePrecision::DateTime => TodoTimePrecision::DateTime,
        TodoTimeInferencePrecision::Inferred => TodoTimePrecision::Inferred,
    }
}

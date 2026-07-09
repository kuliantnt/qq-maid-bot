//! 待办时间推断与展示的通用 helper。
//!
//! 这里不依赖 qq-maid-core 的 TodoItem / TodoItemDraft 业务结构，只处理纯文本、
//! Option<String> 和通用时间上下文；Core 侧负责把结果映射回自己的 Todo 类型。

use super::{
    DateInferencePrecision, RequestTimeContext, format_todo_time_for_display,
    infer_daypart_datetime_from_text, infer_due_date_from_text,
    infer_short_relative_datetime_from_text,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoTimeInferencePrecision {
    None,
    Date,
    DateTime,
    Inferred,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoTimeFields {
    pub due_date: Option<String>,
    pub due_at: Option<String>,
    pub reminder_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichedTodoTimeFields {
    pub fields: TodoTimeFields,
    pub precision: TodoTimeInferencePrecision,
}

/// 从用户文本中推断待办时间字段。
///
/// 语义边界：
/// - `N 分钟/小时后提醒` 写入 reminder_at，不回填 due_at；
/// - 明确的 due_at 由调用方传入并优先保留；
/// - 上午/下午/晚上等时段词用于推断 due_at；
/// - 日期级表达只写 due_date。
pub fn enrich_todo_time_fields_from_text(
    mut fields: TodoTimeFields,
    precision: TodoTimeInferencePrecision,
    user_text: &str,
    ctx: &RequestTimeContext,
) -> EnrichedTodoTimeFields {
    let mut precision = precision;
    if fields.reminder_at.is_none()
        && let Some(reminder_at) = infer_short_relative_datetime_from_text(user_text, ctx)
    {
        fields.reminder_at = Some(reminder_at);
        precision = TodoTimeInferencePrecision::DateTime;
    }

    if fields.due_at.is_some() {
        return EnrichedTodoTimeFields { fields, precision };
    }

    if let Some(daypart) = infer_daypart_datetime_from_text(user_text, ctx) {
        let due_date = fields
            .due_date
            .clone()
            .or_else(|| infer_todo_due_date_from_text(user_text, ctx).map(|(date, _)| date))
            .unwrap_or_else(|| daypart.date.clone());
        fields.due_date = Some(due_date.clone());
        fields.due_at = Some(daypart.datetime_on_date(&due_date));
        precision = TodoTimeInferencePrecision::DateTime;
        return EnrichedTodoTimeFields { fields, precision };
    }

    if fields.due_date.is_none()
        && let Some((date, inferred_precision)) = infer_todo_due_date_from_text(user_text, ctx)
    {
        fields.due_date = Some(date);
        precision = inferred_precision;
    }

    EnrichedTodoTimeFields { fields, precision }
}

/// 把自然语言文本推断为 (日期字符串, 时间精度)，精度只区分 Date / Inferred。
pub fn infer_todo_due_date_from_text(
    text: &str,
    ctx: &RequestTimeContext,
) -> Option<(String, TodoTimeInferencePrecision)> {
    let inferred = infer_due_date_from_text(text, ctx)?;
    let precision = match inferred.precision {
        DateInferencePrecision::Date => TodoTimeInferencePrecision::Date,
        DateInferencePrecision::Inferred => TodoTimeInferencePrecision::Inferred,
    };
    Some((inferred.date, precision))
}

/// 显示待办事项的截止时间（优先 due_at，其次 due_date），无截止时间显示“未指定”。
pub fn display_todo_time_parts(due_date: Option<&str>, due_at: Option<&str>) -> String {
    due_at
        .and_then(clean_optional_time_value)
        .or_else(|| due_date.and_then(clean_optional_time_value))
        .map(format_todo_time_for_display)
        .unwrap_or_else(|| "未指定".to_owned())
}

fn clean_optional_time_value(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

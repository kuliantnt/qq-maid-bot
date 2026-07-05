//! Todo 重复规则与时间推进 helper。
//!
//! 这里集中维护三类语义：
//! - 用户原文里的“每天 / 隔天 / 每隔 N 天”等规则解析；
//! - 重复规则字段的标准化与展示文案；
//! - 完成本次重复任务后，如何把当前时间推进到下一次。
//!
//! 之所以放在 storage::todo 内部，是因为重复规则既影响草稿归一化，
//! 也影响持久化后的真实数据推进；避免 Tool / respond / push 三层各自复制一套。

use std::sync::OnceLock;

use regex::Regex;

use super::{TodoError, TodoItem, TodoItemDraft, TodoRecurrenceKind};
use crate::util::time_context::{
    parse_small_positive_number, shift_local_date_string, shift_timestamp_by_days,
};

static EVERY_N_DAYS_RE: OnceLock<Regex> = OnceLock::new();

pub(super) fn normalize_recurrence_fields(draft: &mut TodoItemDraft) -> Result<(), TodoError> {
    let explicit_none = draft.take_explicit_no_recurrence_marker();
    let explicit = explicit_recurrence(draft)?;
    let inferred = if explicit.is_none() && !explicit_none {
        let source = draft.raw_text.as_deref().unwrap_or(&draft.title);
        parse_recurrence_from_text(source)?
    } else {
        None
    };
    let recurrence = explicit.or(inferred);

    match recurrence {
        Some((kind, interval_days)) => {
            draft.recurrence_kind = kind;
            draft.recurrence_interval_days = interval_days;
        }
        None => {
            draft.recurrence_kind = TodoRecurrenceKind::None;
            draft.recurrence_interval_days = 0;
        }
    }

    if recurrence_interval(&draft.recurrence_kind, draft.recurrence_interval_days).is_some()
        && draft.due_date.is_none()
        && draft.due_at.is_none()
        && draft.reminder_at.is_none()
    {
        return Err(TodoError::bad_request(
            "重复任务需要至少一个日期或提醒时间，请补充提醒时间或到期时间。",
        ));
    }
    Ok(())
}

pub fn recurrence_label(kind: &TodoRecurrenceKind, interval_days: u32) -> Option<String> {
    match recurrence_interval(kind, interval_days) {
        Some(1) => Some("每天".to_owned()),
        Some(2) => Some("隔天".to_owned()),
        Some(days) => Some(format!("每隔 {days} 天")),
        None => None,
    }
}

pub fn is_recurring(item: &TodoItem) -> bool {
    recurrence_interval(&item.recurrence_kind, item.recurrence_interval_days).is_some()
}

pub fn preview_next_reminder_at(item: &TodoItem) -> Result<Option<String>, String> {
    let Some(interval_days) =
        recurrence_interval(&item.recurrence_kind, item.recurrence_interval_days)
    else {
        return Ok(None);
    };
    item.reminder_at
        .as_deref()
        .map(|value| advance_datetime_value(value, interval_days))
        .transpose()
}

pub fn advance_after_completion(item: &TodoItem) -> Result<TodoItemDraft, TodoError> {
    let Some(interval_days) =
        recurrence_interval(&item.recurrence_kind, item.recurrence_interval_days)
    else {
        return Err(TodoError::bad_request("todo is not recurring"));
    };
    let due_date = item
        .due_date
        .as_deref()
        .map(|value| advance_date_value(value, interval_days))
        .transpose()
        .map_err(TodoError::bad_request)?;
    let due_at = item
        .due_at
        .as_deref()
        .map(|value| advance_datetime_value(value, interval_days))
        .transpose()
        .map_err(TodoError::bad_request)?;
    let reminder_at = item
        .reminder_at
        .as_deref()
        .map(|value| advance_datetime_value(value, interval_days))
        .transpose()
        .map_err(TodoError::bad_request)?;
    if due_date.is_none() && due_at.is_none() && reminder_at.is_none() {
        return Err(TodoError::bad_request(
            "重复任务缺少可推进的时间字段，请重新设置提醒时间或到期时间。",
        ));
    }
    Ok(TodoItemDraft {
        title: item.title.clone(),
        detail: item.detail.clone(),
        raw_text: item.raw_text.clone(),
        due_date,
        due_at,
        reminder_at,
        time_precision: item.time_precision.clone(),
        recurrence_kind: item.recurrence_kind.clone(),
        recurrence_interval_days: item.recurrence_interval_days,
    })
}

pub fn recurrence_interval(kind: &TodoRecurrenceKind, interval_days: u32) -> Option<u32> {
    match kind {
        TodoRecurrenceKind::None => None,
        TodoRecurrenceKind::Daily => Some(1),
        TodoRecurrenceKind::EveryNDays => (interval_days > 0).then_some(interval_days),
    }
}

fn explicit_recurrence(
    draft: &TodoItemDraft,
) -> Result<Option<(TodoRecurrenceKind, u32)>, TodoError> {
    match draft.recurrence_kind {
        TodoRecurrenceKind::None => {
            if draft.recurrence_interval_days > 0 {
                return Err(TodoError::bad_request(
                    "recurrence_interval_days 只有在设置重复规则时才允许大于 0。",
                ));
            }
            Ok(None)
        }
        TodoRecurrenceKind::Daily => Ok(Some((TodoRecurrenceKind::Daily, 1))),
        TodoRecurrenceKind::EveryNDays => {
            let interval_days = draft.recurrence_interval_days;
            if interval_days == 0 {
                return Err(TodoError::bad_request("重复天数必须是正整数。"));
            }
            if interval_days == 1 {
                return Ok(Some((TodoRecurrenceKind::Daily, 1)));
            }
            Ok(Some((TodoRecurrenceKind::EveryNDays, interval_days)))
        }
    }
}

fn parse_recurrence_from_text(text: &str) -> Result<Option<(TodoRecurrenceKind, u32)>, TodoError> {
    let compact = text.split_whitespace().collect::<String>();
    if compact.is_empty() {
        return Ok(None);
    }
    if compact.contains("每隔几天") || compact.contains("隔几天") || compact.contains("每几天")
    {
        return Err(TodoError::bad_request(
            "“每隔几天”缺少具体数字，请明确说成“每隔 3 天”之类的规则。",
        ));
    }
    if compact.contains("每天") || compact.contains("每日") {
        return Ok(Some((TodoRecurrenceKind::Daily, 1)));
    }
    if compact.contains("隔天") || compact.contains("每隔一天") {
        return Ok(Some((TodoRecurrenceKind::EveryNDays, 2)));
    }

    let regex = EVERY_N_DAYS_RE.get_or_init(|| {
        Regex::new(r"(?:每隔|隔|每)(?P<n>[0-9一二两三四五六七八九十百]+)天")
            .expect("valid recurrence regex")
    });
    let Some(captures) = regex.captures(&compact) else {
        return Ok(None);
    };
    let number = captures
        .name("n")
        .and_then(|value| parse_small_positive_number(value.as_str()))
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| TodoError::bad_request("重复天数必须是正整数。"))?;
    if number == 1 {
        return Ok(Some((TodoRecurrenceKind::Daily, 1)));
    }
    Ok(Some((TodoRecurrenceKind::EveryNDays, number)))
}

fn advance_date_value(value: &str, interval_days: u32) -> Result<String, String> {
    shift_local_date_string(value, i64::from(interval_days))
        .ok_or_else(|| "重复任务的日期格式无效，必须是 YYYY-MM-DD。".to_owned())
}

fn advance_datetime_value(value: &str, interval_days: u32) -> Result<String, String> {
    shift_timestamp_by_days(value, i64::from(interval_days)).ok_or_else(|| {
        "重复任务的提醒时间格式无效，必须是 YYYY-MM-DD HH:MM[:SS] 或 RFC3339。".to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::todo::{TodoStatus, TodoTimePrecision};

    fn recurring_item() -> TodoItem {
        TodoItem {
            id: "1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_key: "private:u1".to_owned(),
            title: "喝水".to_owned(),
            detail: None,
            raw_text: Some("每天 9 点提醒我喝水".to_owned()),
            due_date: Some("2099-01-01".to_owned()),
            due_at: Some("2099-01-01 09:00:00".to_owned()),
            reminder_at: Some("2099-01-01 09:00:00".to_owned()),
            time_precision: TodoTimePrecision::DateTime,
            recurrence_kind: TodoRecurrenceKind::Daily,
            recurrence_interval_days: 1,
            status: TodoStatus::Pending,
            created_at: "2026-07-05T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-05T09:00:00+08:00".to_owned(),
            completed_at: None,
            cancelled_at: None,
        }
    }

    #[test]
    fn parses_supported_recurrence_phrases() {
        assert_eq!(
            parse_recurrence_from_text("每天 9 点提醒我喝水")
                .unwrap()
                .unwrap(),
            (TodoRecurrenceKind::Daily, 1)
        );
        assert_eq!(
            parse_recurrence_from_text("隔天提醒我浇花")
                .unwrap()
                .unwrap(),
            (TodoRecurrenceKind::EveryNDays, 2)
        );
        assert_eq!(
            parse_recurrence_from_text("每隔 3 天提醒我整理日志")
                .unwrap()
                .unwrap(),
            (TodoRecurrenceKind::EveryNDays, 3)
        );
        assert_eq!(
            parse_recurrence_from_text("每三天整理一次")
                .unwrap()
                .unwrap(),
            (TodoRecurrenceKind::EveryNDays, 3)
        );
    }

    #[test]
    fn ambiguous_recurrence_requires_specific_number() {
        let err = parse_recurrence_from_text("每隔几天提醒我复盘").unwrap_err();
        assert_eq!(err.code(), "bad_request");
    }

    #[test]
    fn preview_and_advance_keep_interval() {
        let item = recurring_item();

        assert_eq!(
            preview_next_reminder_at(&item).unwrap(),
            Some("2099-01-02 09:00:00".to_owned())
        );

        let advanced = advance_after_completion(&item).unwrap();
        assert_eq!(advanced.due_at.as_deref(), Some("2099-01-02 09:00:00"));
        assert_eq!(advanced.reminder_at.as_deref(), Some("2099-01-02 09:00:00"));
        assert_eq!(advanced.recurrence_kind, TodoRecurrenceKind::Daily);
        assert_eq!(advanced.recurrence_interval_days, 1);
    }
}

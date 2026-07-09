//! Todo 重复规则字段归一、展示文案与时间推进 helper。
//!
//! storage 层只处理结构化 recurrence 字段，不再读取 raw_text/title/detail
//! 判断自然语言意图。用户原文里的“每天 / 每隔 N 天”等解析属于 Todo Tool
//! 业务域，写库前应先转换成结构化字段。

use chrono::{DateTime, FixedOffset, Utc};
use qq_maid_common::time_context::CalendarRecurrenceUnit;

use super::{
    TodoEditRecurrencePatch, TodoError, TodoItem, TodoItemDraft, TodoRecurrenceKind,
    TodoRecurrenceUnit,
};
use qq_maid_common::time_context::{
    cycles_to_advance_date_after_calendar, cycles_to_advance_datetime_after_calendar,
    parse_local_date_string, parse_local_datetime_for_comparison, shanghai_offset,
    shift_local_date_string_by_calendar, shift_timestamp_by_calendar,
};

const MAX_RECURRENCE_ADVANCE_CYCLES: i64 = 100_000;
// 分钟/小时单位用于短周期提醒，超过该范围应改用 day/week/month/year 表达，
// 避免把长周期写成巨大的 minute/hour 间隔导致误解。
const MAX_RECURRENCE_MINUTES: u32 = 1_440;
const MAX_RECURRENCE_HOURS: u32 = 720;
const MAX_RECURRENCE_DAYS: u32 = 1_827;
const MAX_RECURRENCE_WEEKS: u32 = 261;
const MAX_RECURRENCE_MONTHS: u32 = 60;
const MAX_RECURRENCE_YEARS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TodoRecurrenceRule {
    pub interval: u32,
    pub unit: TodoRecurrenceUnit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedTodoRecurrence {
    pub kind: TodoRecurrenceKind,
    pub interval_days: u32,
    pub interval: u32,
    pub unit: TodoRecurrenceUnit,
}

/// 把 create/edit 两侧输入统一归一成稳定 recurrence 字段。
pub(super) fn normalize_todo_recurrence_input(
    draft: &mut TodoItemDraft,
) -> Result<NormalizedTodoRecurrence, TodoError> {
    let explicit_none = draft.take_explicit_no_recurrence_marker();
    let explicit = explicit_recurrence(draft)?;
    let recurrence = if explicit_none { None } else { explicit };

    let normalized = match recurrence {
        Some((kind, rule)) => NormalizedTodoRecurrence {
            kind,
            interval_days: legacy_interval_days(&rule),
            interval: rule.interval,
            unit: rule.unit,
        },
        None => NormalizedTodoRecurrence {
            kind: TodoRecurrenceKind::None,
            interval_days: 0,
            interval: 0,
            unit: TodoRecurrenceUnit::Day,
        },
    };
    apply_normalized_recurrence_to_draft(draft, &normalized);

    if recurrence_rule(draft).is_some()
        && draft.due_date.is_none()
        && draft.due_at.is_none()
        && draft.reminder_at.is_none()
    {
        return Err(TodoError::bad_request(
            "重复任务需要至少一个日期、到期时间或提醒时间；如果只是周期提醒，请说清楚提醒内容和重复间隔。",
        ));
    }
    Ok(normalized)
}

pub(super) fn apply_normalized_recurrence_to_draft(
    draft: &mut TodoItemDraft,
    recurrence: &NormalizedTodoRecurrence,
) {
    draft.recurrence_kind = recurrence.kind.clone();
    draft.recurrence_interval_days = recurrence.interval_days;
    draft.recurrence_interval = recurrence.interval;
    draft.recurrence_unit = recurrence.unit;
}

/// 把编辑补丁里的 recurrence 字段应用到草稿。
///
/// 这里只做字段组合与默认值补齐，真正的业务校验仍由
/// `normalize_todo_recurrence_input` 统一执行。
pub fn apply_recurrence_patch_to_draft(draft: &mut TodoItemDraft, patch: TodoEditRecurrencePatch) {
    if let Some(recurrence_kind) = patch.kind {
        if matches!(recurrence_kind, TodoRecurrenceKind::None) {
            draft.mark_explicit_no_recurrence();
        } else {
            let default_rule = default_rule_for_kind(&recurrence_kind);
            draft.recurrence_kind = recurrence_kind;
            if let Some(rule) = default_rule {
                draft.recurrence_interval = rule.interval;
                draft.recurrence_unit = rule.unit;
                draft.recurrence_interval_days = legacy_interval_days(&rule);
            } else {
                draft.recurrence_interval = 0;
                draft.recurrence_interval_days = 0;
                if let Some(default_unit) = default_unit_for_kind(&draft.recurrence_kind) {
                    draft.recurrence_unit = default_unit;
                }
            }
        }
    }
    if let Some(recurrence_interval_days) = patch.interval_days {
        draft.recurrence_interval_days = recurrence_interval_days;
        if patch.interval.is_none() && patch.unit.is_none() {
            draft.recurrence_interval = recurrence_interval_days;
            draft.recurrence_unit = TodoRecurrenceUnit::Day;
        }
    }
    if let Some(recurrence_interval) = patch.interval {
        draft.recurrence_interval = recurrence_interval;
    }
    if let Some(recurrence_unit) = patch.unit {
        draft.recurrence_unit = recurrence_unit;
    } else if let Some(default_unit) = default_unit_for_kind(&draft.recurrence_kind) {
        draft.recurrence_unit = default_unit;
    }
}

fn default_rule_for_kind(kind: &TodoRecurrenceKind) -> Option<TodoRecurrenceRule> {
    match kind {
        TodoRecurrenceKind::Daily => Some(TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Day,
        }),
        TodoRecurrenceKind::Weekly => Some(TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Week,
        }),
        TodoRecurrenceKind::Monthly => Some(TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Month,
        }),
        TodoRecurrenceKind::Yearly => Some(TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Year,
        }),
        TodoRecurrenceKind::EveryNDays
        | TodoRecurrenceKind::EveryNWeeks
        | TodoRecurrenceKind::EveryNMonths
        | TodoRecurrenceKind::EveryNYears
        | TodoRecurrenceKind::EveryNMinutes
        | TodoRecurrenceKind::EveryNHours
        | TodoRecurrenceKind::None => None,
    }
}

fn default_unit_for_kind(kind: &TodoRecurrenceKind) -> Option<TodoRecurrenceUnit> {
    match kind {
        TodoRecurrenceKind::Daily | TodoRecurrenceKind::EveryNDays => Some(TodoRecurrenceUnit::Day),
        TodoRecurrenceKind::Weekly | TodoRecurrenceKind::EveryNWeeks => {
            Some(TodoRecurrenceUnit::Week)
        }
        TodoRecurrenceKind::Monthly | TodoRecurrenceKind::EveryNMonths => {
            Some(TodoRecurrenceUnit::Month)
        }
        TodoRecurrenceKind::Yearly | TodoRecurrenceKind::EveryNYears => {
            Some(TodoRecurrenceUnit::Year)
        }
        TodoRecurrenceKind::EveryNMinutes => Some(TodoRecurrenceUnit::Minute),
        TodoRecurrenceKind::EveryNHours => Some(TodoRecurrenceUnit::Hour),
        TodoRecurrenceKind::None => None,
    }
}

pub fn recurrence_label(
    kind: &TodoRecurrenceKind,
    interval_days: u32,
    interval: u32,
    unit: &TodoRecurrenceUnit,
) -> Option<String> {
    match recurrence_rule_from_parts(kind, interval_days, interval, unit)
        .ok()
        .flatten()?
    {
        TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Minute,
        } => Some("每分钟".to_owned()),
        TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Minute,
        } => Some(format!("每隔 {interval} 分钟")),
        TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Hour,
        } => Some("每小时".to_owned()),
        TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Hour,
        } => Some(format!("每隔 {interval} 小时")),
        TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Day,
        } => Some("每天".to_owned()),
        TodoRecurrenceRule {
            interval: 2,
            unit: TodoRecurrenceUnit::Day,
        } => Some("隔天".to_owned()),
        TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Day,
        } => Some(format!("每隔 {interval} 天")),
        TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Week,
        } => Some("每周".to_owned()),
        TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Week,
        } => Some(format!("每隔 {interval} 周")),
        TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Month,
        } => Some("每月".to_owned()),
        TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Month,
        } => Some(format!("每隔 {interval} 个月")),
        TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Year,
        } => Some("每年".to_owned()),
        TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Year,
        } => Some(format!("每隔 {interval} 年")),
    }
}

pub fn validate_recurrence_rule(interval: u32, unit: &TodoRecurrenceUnit) -> Result<(), TodoError> {
    if interval == 0 {
        return Err(TodoError::bad_request("重复间隔必须是正整数。"));
    }
    let max = max_interval_for_unit(unit);
    if interval > max {
        return Err(TodoError::bad_request(max_interval_error_message(unit)));
    }
    Ok(())
}

pub fn recurrence_rule_for_item(item: &TodoItem) -> Result<Option<TodoRecurrenceRule>, TodoError> {
    recurrence_rule_from_parts(
        &item.recurrence_kind,
        item.recurrence_interval_days,
        item.recurrence_interval,
        &item.recurrence_unit,
    )
}

pub fn recurrence_rule_from_parts(
    kind: &TodoRecurrenceKind,
    legacy_interval_days: u32,
    interval: u32,
    unit: &TodoRecurrenceUnit,
) -> Result<Option<TodoRecurrenceRule>, TodoError> {
    let rule = match kind {
        TodoRecurrenceKind::None => {
            if legacy_interval_days > 0 || interval > 0 {
                return Err(TodoError::bad_request(
                    "重复间隔只有在设置重复规则时才允许大于 0。",
                ));
            }
            return Ok(None);
        }
        TodoRecurrenceKind::Daily => TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Day,
        },
        TodoRecurrenceKind::EveryNDays => TodoRecurrenceRule {
            interval: if interval > 0 {
                interval
            } else if legacy_interval_days == 1 {
                2
            } else {
                legacy_interval_days
            },
            unit: TodoRecurrenceUnit::Day,
        },
        TodoRecurrenceKind::Weekly => TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Week,
        },
        TodoRecurrenceKind::EveryNWeeks => TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Week,
        },
        TodoRecurrenceKind::Monthly => TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Month,
        },
        TodoRecurrenceKind::EveryNMonths => TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Month,
        },
        TodoRecurrenceKind::Yearly => TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Year,
        },
        TodoRecurrenceKind::EveryNYears => TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Year,
        },
        TodoRecurrenceKind::EveryNMinutes => TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Minute,
        },
        TodoRecurrenceKind::EveryNHours => TodoRecurrenceRule {
            interval,
            unit: TodoRecurrenceUnit::Hour,
        },
    };
    let rule = normalize_rule_interval(rule)?;
    if interval > 0 && *unit != rule.unit {
        return Err(TodoError::bad_request(
            "重复间隔单位与重复规则不一致，请重新设置重复周期。",
        ));
    }
    validate_recurrence_rule(rule.interval, &rule.unit)?;
    Ok(Some(rule))
}

fn recurrence_rule(draft: &TodoItemDraft) -> Option<TodoRecurrenceRule> {
    recurrence_rule_from_parts(
        &draft.recurrence_kind,
        draft.recurrence_interval_days,
        draft.recurrence_interval,
        &draft.recurrence_unit,
    )
    .ok()
    .flatten()
}

pub fn recurrence_kind_for_rule(rule: &TodoRecurrenceRule) -> TodoRecurrenceKind {
    match (rule.unit, rule.interval) {
        (TodoRecurrenceUnit::Minute, _) => TodoRecurrenceKind::EveryNMinutes,
        (TodoRecurrenceUnit::Hour, _) => TodoRecurrenceKind::EveryNHours,
        (TodoRecurrenceUnit::Day, 1) => TodoRecurrenceKind::Daily,
        (TodoRecurrenceUnit::Day, _) => TodoRecurrenceKind::EveryNDays,
        (TodoRecurrenceUnit::Week, 1) => TodoRecurrenceKind::Weekly,
        (TodoRecurrenceUnit::Week, _) => TodoRecurrenceKind::EveryNWeeks,
        (TodoRecurrenceUnit::Month, 1) => TodoRecurrenceKind::Monthly,
        (TodoRecurrenceUnit::Month, _) => TodoRecurrenceKind::EveryNMonths,
        (TodoRecurrenceUnit::Year, 1) => TodoRecurrenceKind::Yearly,
        (TodoRecurrenceUnit::Year, _) => TodoRecurrenceKind::EveryNYears,
    }
}

fn normalize_rule_interval(mut rule: TodoRecurrenceRule) -> Result<TodoRecurrenceRule, TodoError> {
    if rule.interval == 0 {
        return Err(TodoError::bad_request("重复间隔必须是正整数。"));
    }
    if matches!(rule.unit, TodoRecurrenceUnit::Day) && rule.interval == 1 {
        rule.interval = 1;
    }
    Ok(rule)
}

fn legacy_interval_days(rule: &TodoRecurrenceRule) -> u32 {
    match rule.unit {
        TodoRecurrenceUnit::Day => rule.interval,
        _ => 0,
    }
}

fn max_interval_for_unit(unit: &TodoRecurrenceUnit) -> u32 {
    match unit {
        TodoRecurrenceUnit::Minute => MAX_RECURRENCE_MINUTES,
        TodoRecurrenceUnit::Hour => MAX_RECURRENCE_HOURS,
        TodoRecurrenceUnit::Day => MAX_RECURRENCE_DAYS,
        TodoRecurrenceUnit::Week => MAX_RECURRENCE_WEEKS,
        TodoRecurrenceUnit::Month => MAX_RECURRENCE_MONTHS,
        TodoRecurrenceUnit::Year => MAX_RECURRENCE_YEARS,
    }
}

fn max_interval_error_message(unit: &TodoRecurrenceUnit) -> &'static str {
    match unit {
        TodoRecurrenceUnit::Minute => {
            "分钟级重复间隔过大，最多支持每 1440 分钟一次；更长周期请改用小时或天。"
        }
        TodoRecurrenceUnit::Hour => {
            "小时级重复间隔过大，最多支持每 720 小时一次；更长周期请改用天、周或月。"
        }
        TodoRecurrenceUnit::Day
        | TodoRecurrenceUnit::Week
        | TodoRecurrenceUnit::Month
        | TodoRecurrenceUnit::Year => "重复间隔过大，最多支持 5 年内的重复周期。",
    }
}

fn calendar_unit(unit: &TodoRecurrenceUnit) -> CalendarRecurrenceUnit {
    match unit {
        TodoRecurrenceUnit::Minute => CalendarRecurrenceUnit::Minute,
        TodoRecurrenceUnit::Hour => CalendarRecurrenceUnit::Hour,
        TodoRecurrenceUnit::Day => CalendarRecurrenceUnit::Day,
        TodoRecurrenceUnit::Week => CalendarRecurrenceUnit::Week,
        TodoRecurrenceUnit::Month => CalendarRecurrenceUnit::Month,
        TodoRecurrenceUnit::Year => CalendarRecurrenceUnit::Year,
    }
}

pub fn recurrence_rule_error_message(err: TodoError) -> String {
    err.message().to_owned()
}

pub fn recurrence_rule_from_interval_unit(
    interval: u32,
    unit: TodoRecurrenceUnit,
) -> Result<(TodoRecurrenceKind, TodoRecurrenceRule), TodoError> {
    let rule = normalize_rule_interval(TodoRecurrenceRule { interval, unit })?;
    validate_recurrence_rule(rule.interval, &rule.unit)?;
    Ok((recurrence_kind_for_rule(&rule), rule))
}

fn explicit_recurrence(
    draft: &TodoItemDraft,
) -> Result<Option<(TodoRecurrenceKind, TodoRecurrenceRule)>, TodoError> {
    if matches!(draft.recurrence_kind, TodoRecurrenceKind::None) {
        if draft.recurrence_interval_days > 0 {
            return Err(TodoError::bad_request(
                "重复间隔只有在设置重复规则时才允许大于 0。",
            ));
        }
        if draft.recurrence_interval > 0 {
            return recurrence_rule_from_interval_unit(
                draft.recurrence_interval,
                draft.recurrence_unit,
            )
            .map(Some);
        }
        return Ok(None);
    }
    let Some(rule) = recurrence_rule_from_parts(
        &draft.recurrence_kind,
        draft.recurrence_interval_days,
        draft.recurrence_interval,
        &draft.recurrence_unit,
    )?
    else {
        return Ok(None);
    };
    Ok(Some((recurrence_kind_for_rule(&rule), rule)))
}

pub fn is_recurring(item: &TodoItem) -> bool {
    recurrence_rule_for_item(item).ok().flatten().is_some()
}

pub fn preview_next_reminder_at(item: &TodoItem) -> Result<Option<String>, String> {
    let Some(rule) = recurrence_rule_for_item(item).map_err(recurrence_rule_error_message)? else {
        return Ok(None);
    };
    item.reminder_at
        .as_deref()
        .map(|value| advance_datetime_value(value, rule, 1))
        .transpose()
}

pub fn advance_after_completion(item: &TodoItem) -> Result<TodoItemDraft, TodoError> {
    advance_after_completion_at(item, Utc::now().with_timezone(&shanghai_offset()))
}

pub fn advance_after_completion_at(
    item: &TodoItem,
    now: DateTime<FixedOffset>,
) -> Result<TodoItemDraft, TodoError> {
    let Some(rule) = recurrence_rule_for_item(item)? else {
        return Err(TodoError::bad_request("todo is not recurring"));
    };
    let cycles = recurrence_advance_cycles(item, rule, now)?;
    let due_date = item
        .due_date
        .as_deref()
        .map(|value| advance_date_value(value, rule, cycles))
        .transpose()
        .map_err(TodoError::bad_request)?;
    let due_at = item
        .due_at
        .as_deref()
        .map(|value| advance_datetime_value(value, rule, cycles))
        .transpose()
        .map_err(TodoError::bad_request)?;
    let reminder_at = item
        .reminder_at
        .as_deref()
        .map(|value| advance_datetime_value(value, rule, cycles))
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
        time_precision: item.time_precision,
        recurrence_kind: item.recurrence_kind.clone(),
        recurrence_interval_days: item.recurrence_interval_days,
        recurrence_interval: item.recurrence_interval,
        recurrence_unit: item.recurrence_unit,
    })
}

fn recurrence_advance_cycles(
    item: &TodoItem,
    rule: TodoRecurrenceRule,
    now: DateTime<FixedOffset>,
) -> Result<i64, TodoError> {
    let unit = calendar_unit(&rule.unit);
    let cycles = if let Some(reminder_at) = item.reminder_at.as_deref() {
        let anchor = parse_local_datetime_anchor(reminder_at)?;
        cycles_to_advance_datetime_after_calendar(
            anchor,
            now,
            rule.interval,
            unit,
            MAX_RECURRENCE_ADVANCE_CYCLES,
        )
    } else if let Some(due_at) = item.due_at.as_deref() {
        let anchor = parse_local_datetime_anchor(due_at)?;
        cycles_to_advance_datetime_after_calendar(
            anchor,
            now,
            rule.interval,
            unit,
            MAX_RECURRENCE_ADVANCE_CYCLES,
        )
    } else if let Some(due_date) = item.due_date.as_deref() {
        if matches!(
            rule.unit,
            TodoRecurrenceUnit::Minute | TodoRecurrenceUnit::Hour
        ) {
            return Err(TodoError::bad_request(
                "分钟/小时重复任务需要提醒时间或截止时间，不能只设置日期。",
            ));
        }
        let anchor = parse_local_date_anchor(due_date)?;
        cycles_to_advance_date_after_calendar(
            anchor,
            now.date_naive(),
            rule.interval,
            unit,
            MAX_RECURRENCE_ADVANCE_CYCLES,
        )
    } else {
        return Err(TodoError::bad_request(
            "重复任务缺少可推进的时间字段，请重新设置提醒时间或到期时间。",
        ));
    };
    cycles.ok_or_else(|| {
        TodoError::bad_request("重复任务时间推进超出可处理范围，请重新设置提醒时间或到期时间。")
    })
}

fn parse_local_datetime_anchor(value: &str) -> Result<DateTime<FixedOffset>, TodoError> {
    parse_local_datetime_for_comparison(value).ok_or_else(|| {
        TodoError::bad_request(
            "重复任务的提醒时间格式无效，必须是 YYYY-MM-DD HH:MM[:SS] 或 RFC3339。",
        )
    })
}

fn parse_local_date_anchor(value: &str) -> Result<chrono::NaiveDate, TodoError> {
    parse_local_date_string(value)
        .ok_or_else(|| TodoError::bad_request("重复任务的日期格式无效，必须是 YYYY-MM-DD。"))
}

fn advance_date_value(
    value: &str,
    rule: TodoRecurrenceRule,
    cycles: i64,
) -> Result<String, String> {
    if matches!(
        rule.unit,
        TodoRecurrenceUnit::Minute | TodoRecurrenceUnit::Hour
    ) {
        return Err("分钟/小时重复任务不能推进仅日期字段，请改用提醒时间或截止时间。".to_owned());
    }
    shift_local_date_string_by_calendar(value, rule.interval, calendar_unit(&rule.unit), cycles)
        .ok_or_else(|| "重复任务的日期格式无效，必须是 YYYY-MM-DD。".to_owned())
}

fn advance_datetime_value(
    value: &str,
    rule: TodoRecurrenceRule,
    cycles: i64,
) -> Result<String, String> {
    shift_timestamp_by_calendar(value, rule.interval, calendar_unit(&rule.unit), cycles).ok_or_else(
        || "重复任务的提醒时间格式无效，必须是 YYYY-MM-DD HH:MM[:SS] 或 RFC3339。".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tools::todo::{TodoStatus, TodoTimePrecision};
    use chrono::TimeZone;

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
            recurrence_interval: 1,
            recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            status: TodoStatus::Pending,
            created_at: "2026-07-05T09:00:00+08:00".to_owned(),
            updated_at: "2026-07-05T09:00:00+08:00".to_owned(),
            completed_at: None,
        }
    }

    #[test]
    fn explicit_every_n_days_one_means_every_other_day() {
        let mut draft = TodoItemDraft {
            title: "浇花".to_owned(),
            detail: None,
            raw_text: None,
            due_date: Some("2099-01-01".to_owned()),
            due_at: None,
            reminder_at: None,
            time_precision: TodoTimePrecision::Date,
            recurrence_kind: TodoRecurrenceKind::EveryNDays,
            recurrence_interval_days: 1,
            recurrence_interval: 0,
            recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
        };

        normalize_todo_recurrence_input(&mut draft).unwrap();

        assert_eq!(draft.recurrence_kind, TodoRecurrenceKind::EveryNDays);
        assert_eq!(draft.recurrence_interval_days, 2);
        assert_eq!(
            recurrence_label(
                &draft.recurrence_kind,
                draft.recurrence_interval_days,
                draft.recurrence_interval,
                &draft.recurrence_unit,
            )
            .as_deref(),
            Some("隔天")
        );
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

    #[test]
    fn valid_day_week_month_and_year_intervals_normalize() {
        for (interval, unit, expected_kind, expected_legacy_days) in [
            (1, TodoRecurrenceUnit::Day, TodoRecurrenceKind::Daily, 1),
            (
                7,
                TodoRecurrenceUnit::Day,
                TodoRecurrenceKind::EveryNDays,
                7,
            ),
            (1, TodoRecurrenceUnit::Week, TodoRecurrenceKind::Weekly, 0),
            (
                3,
                TodoRecurrenceUnit::Month,
                TodoRecurrenceKind::EveryNMonths,
                0,
            ),
            (
                5,
                TodoRecurrenceUnit::Year,
                TodoRecurrenceKind::EveryNYears,
                0,
            ),
        ] {
            let mut draft = TodoItemDraft {
                title: "复盘".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2099-01-01".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: interval,
                recurrence_unit: unit,
            };

            normalize_todo_recurrence_input(&mut draft).unwrap();

            assert_eq!(draft.recurrence_kind, expected_kind);
            assert_eq!(draft.recurrence_interval, interval);
            assert_eq!(draft.recurrence_unit, unit);
            assert_eq!(draft.recurrence_interval_days, expected_legacy_days);
        }
    }

    #[test]
    fn recurrence_interval_limits_reject_over_five_years() {
        for (interval, unit) in [
            (1_828, TodoRecurrenceUnit::Day),
            (262, TodoRecurrenceUnit::Week),
            (61, TodoRecurrenceUnit::Month),
            (6, TodoRecurrenceUnit::Year),
        ] {
            let mut draft = TodoItemDraft {
                title: "复盘".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2099-01-01".to_owned()),
                due_at: None,
                reminder_at: None,
                time_precision: TodoTimePrecision::Date,
                recurrence_kind: TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: interval,
                recurrence_unit: unit,
            };

            let err = normalize_todo_recurrence_input(&mut draft).unwrap_err();

            assert_eq!(err.code(), "bad_request");
            assert!(err.message().contains("最多支持 5 年内"));
        }
    }

    #[test]
    fn minute_and_hour_interval_limits_are_explicit() {
        for (interval, unit, expected) in [
            (1_441, TodoRecurrenceUnit::Minute, "分钟级重复间隔过大"),
            (721, TodoRecurrenceUnit::Hour, "小时级重复间隔过大"),
        ] {
            let mut draft = TodoItemDraft {
                title: "报时".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                reminder_at: Some("2099-01-01 09:00:00".to_owned()),
                time_precision: TodoTimePrecision::DateTime,
                recurrence_kind: TodoRecurrenceKind::None,
                recurrence_interval_days: 0,
                recurrence_interval: interval,
                recurrence_unit: unit,
            };

            let err = normalize_todo_recurrence_input(&mut draft).unwrap_err();

            assert_eq!(err.code(), "bad_request");
            assert!(err.message().contains(expected));
        }
    }

    #[test]
    fn huge_legacy_day_interval_returns_error_without_panic() {
        let item = TodoItem {
            recurrence_kind: TodoRecurrenceKind::EveryNDays,
            recurrence_interval_days: u32::MAX,
            recurrence_interval: 0,
            recurrence_unit: TodoRecurrenceUnit::Day,
            ..recurring_item()
        };

        let preview = std::panic::catch_unwind(|| preview_next_reminder_at(&item));
        assert!(preview.is_ok());
        assert!(preview.unwrap().unwrap_err().contains("最多支持 5 年内"));

        let advanced = std::panic::catch_unwind(|| {
            advance_after_completion_at(
                &item,
                shanghai_offset()
                    .with_ymd_and_hms(2026, 7, 5, 10, 0, 0)
                    .unwrap(),
            )
        });
        assert!(advanced.is_ok());
        assert_eq!(advanced.unwrap().unwrap_err().code(), "bad_request");
    }

    #[test]
    fn month_end_and_leap_day_use_calendar_clamping() {
        let monthly = TodoItem {
            due_date: Some("2026-01-31".to_owned()),
            due_at: Some("2026-01-31 09:00:00".to_owned()),
            reminder_at: Some("2026-01-31 08:30:00".to_owned()),
            recurrence_kind: TodoRecurrenceKind::Monthly,
            recurrence_interval_days: 0,
            recurrence_interval: 1,
            recurrence_unit: TodoRecurrenceUnit::Month,
            ..recurring_item()
        };
        let yearly = TodoItem {
            due_date: Some("2024-02-29".to_owned()),
            due_at: Some("2024-02-29 09:00:00".to_owned()),
            reminder_at: Some("2024-02-29 08:30:00".to_owned()),
            recurrence_kind: TodoRecurrenceKind::Yearly,
            recurrence_interval_days: 0,
            recurrence_interval: 1,
            recurrence_unit: TodoRecurrenceUnit::Year,
            ..recurring_item()
        };
        let now = shanghai_offset()
            .with_ymd_and_hms(2026, 1, 1, 10, 0, 0)
            .unwrap();

        let monthly_advanced = advance_after_completion_at(&monthly, now).unwrap();
        assert_eq!(monthly_advanced.due_date.as_deref(), Some("2026-02-28"));
        assert_eq!(
            monthly_advanced.reminder_at.as_deref(),
            Some("2026-02-28 08:30:00")
        );

        let leap_now = shanghai_offset()
            .with_ymd_and_hms(2024, 1, 1, 10, 0, 0)
            .unwrap();
        let yearly_advanced = advance_after_completion_at(&yearly, leap_now).unwrap();
        assert_eq!(yearly_advanced.due_date.as_deref(), Some("2025-02-28"));
        assert_eq!(
            yearly_advanced.reminder_at.as_deref(),
            Some("2025-02-28 08:30:00")
        );
    }

    #[test]
    fn overdue_daily_recurring_reminder_advances_to_future() {
        let item = TodoItem {
            due_date: Some("2026-07-01".to_owned()),
            due_at: Some("2026-07-01 09:00:00".to_owned()),
            reminder_at: Some("2026-07-01 09:00:00".to_owned()),
            ..recurring_item()
        };
        let now = shanghai_offset()
            .with_ymd_and_hms(2026, 7, 5, 10, 0, 0)
            .unwrap();

        let advanced = advance_after_completion_at(&item, now).unwrap();

        assert_eq!(advanced.due_date.as_deref(), Some("2026-07-06"));
        assert_eq!(advanced.due_at.as_deref(), Some("2026-07-06 09:00:00"));
        assert_eq!(advanced.reminder_at.as_deref(), Some("2026-07-06 09:00:00"));
    }

    #[test]
    fn overdue_every_other_day_reminder_advances_to_future() {
        let item = TodoItem {
            due_date: Some("2026-07-01".to_owned()),
            due_at: Some("2026-07-01 09:00:00".to_owned()),
            reminder_at: Some("2026-07-01 09:00:00".to_owned()),
            recurrence_kind: TodoRecurrenceKind::EveryNDays,
            recurrence_interval_days: 2,
            recurrence_interval: 2,
            recurrence_unit: crate::runtime::tools::todo::TodoRecurrenceUnit::Day,
            ..recurring_item()
        };
        let now = shanghai_offset()
            .with_ymd_and_hms(2026, 7, 5, 10, 0, 0)
            .unwrap();

        let advanced = advance_after_completion_at(&item, now).unwrap();

        assert_eq!(advanced.due_date.as_deref(), Some("2026-07-07"));
        assert_eq!(advanced.due_at.as_deref(), Some("2026-07-07 09:00:00"));
        assert_eq!(advanced.reminder_at.as_deref(), Some("2026-07-07 09:00:00"));
    }

    #[test]
    fn future_recurring_reminder_still_advances_one_period() {
        let item = recurring_item();
        let now = shanghai_offset()
            .with_ymd_and_hms(2026, 7, 5, 10, 0, 0)
            .unwrap();

        let advanced = advance_after_completion_at(&item, now).unwrap();

        assert_eq!(advanced.due_at.as_deref(), Some("2099-01-02 09:00:00"));
        assert_eq!(advanced.reminder_at.as_deref(), Some("2099-01-02 09:00:00"));
    }

    #[test]
    fn minute_recurring_reminder_advances_without_due_at() {
        let item = TodoItem {
            due_date: None,
            due_at: None,
            reminder_at: Some("2026-07-05 10:00:00".to_owned()),
            recurrence_kind: TodoRecurrenceKind::EveryNMinutes,
            recurrence_interval_days: 0,
            recurrence_interval: 5,
            recurrence_unit: TodoRecurrenceUnit::Minute,
            ..recurring_item()
        };
        let now = shanghai_offset()
            .with_ymd_and_hms(2026, 7, 5, 10, 2, 0)
            .unwrap();

        assert_eq!(
            preview_next_reminder_at(&item).unwrap(),
            Some("2026-07-05 10:05:00".to_owned())
        );
        let advanced = advance_after_completion_at(&item, now).unwrap();

        assert_eq!(advanced.due_date, None);
        assert_eq!(advanced.due_at, None);
        assert_eq!(advanced.reminder_at.as_deref(), Some("2026-07-05 10:05:00"));
        assert_eq!(advanced.recurrence_kind, TodoRecurrenceKind::EveryNMinutes);
        assert_eq!(advanced.recurrence_unit, TodoRecurrenceUnit::Minute);
    }

    #[test]
    fn minute_recurrence_rejects_date_only_anchor() {
        let item = TodoItem {
            due_date: Some("2026-07-05".to_owned()),
            due_at: None,
            reminder_at: None,
            recurrence_kind: TodoRecurrenceKind::EveryNMinutes,
            recurrence_interval_days: 0,
            recurrence_interval: 5,
            recurrence_unit: TodoRecurrenceUnit::Minute,
            ..recurring_item()
        };
        let now = shanghai_offset()
            .with_ymd_and_hms(2026, 7, 5, 10, 2, 0)
            .unwrap();

        let err = advance_after_completion_at(&item, now).unwrap_err();

        assert_eq!(err.code(), "bad_request");
        assert!(err.message().contains("不能只设置日期"));
    }

    #[test]
    fn reminder_anchor_keeps_due_at_offset_when_both_exist() {
        let item = TodoItem {
            due_date: Some("2026-07-01".to_owned()),
            due_at: Some("2026-07-01 10:00:00".to_owned()),
            reminder_at: Some("2026-07-01 09:00:00".to_owned()),
            ..recurring_item()
        };
        let now = shanghai_offset()
            .with_ymd_and_hms(2026, 7, 5, 10, 0, 0)
            .unwrap();

        let advanced = advance_after_completion_at(&item, now).unwrap();

        assert_eq!(advanced.due_at.as_deref(), Some("2026-07-06 10:00:00"));
        assert_eq!(advanced.reminder_at.as_deref(), Some("2026-07-06 09:00:00"));
    }

    #[test]
    fn due_date_only_advances_by_local_date() {
        let item = TodoItem {
            due_date: Some("2026-07-01".to_owned()),
            due_at: None,
            reminder_at: None,
            ..recurring_item()
        };
        let now = shanghai_offset()
            .with_ymd_and_hms(2026, 7, 5, 10, 0, 0)
            .unwrap();

        let advanced = advance_after_completion_at(&item, now).unwrap();

        assert_eq!(advanced.due_date.as_deref(), Some("2026-07-06"));
    }

    #[test]
    fn recurring_without_time_fields_returns_bad_request() {
        let item = TodoItem {
            due_date: None,
            due_at: None,
            reminder_at: None,
            ..recurring_item()
        };
        let now = shanghai_offset()
            .with_ymd_and_hms(2026, 7, 5, 10, 0, 0)
            .unwrap();

        let err = advance_after_completion_at(&item, now).unwrap_err();

        assert_eq!(err.code(), "bad_request");
        assert!(err.message().contains("缺少可推进的时间字段"));
    }
}

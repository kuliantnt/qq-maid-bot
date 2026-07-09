//! Todo Tool 域的重复规则意图解析。
//!
//! storage 层只接受结构化 recurrence 字段；这里负责把用户原文里的
//! “每天 / 每隔 N 分钟 / 隔天”等自然语言表达转换成结构化字段，并在
//! “周期性纯提醒”没有显式时间锚点时生成第一次 `reminder_at`。

use std::sync::OnceLock;

use chrono::Utc;
use qq_maid_common::time_context::{
    CalendarRecurrenceUnit, parse_small_positive_number, shanghai_offset,
    shift_timestamp_by_calendar,
};
use regex::Regex;

use crate::runtime::tools::todo::{
    TodoError, TodoItemDraft, TodoRecurrenceKind, TodoRecurrenceRule, TodoRecurrenceUnit,
    TodoTimePrecision, recurrence_kind_for_rule, recurrence_rule_from_interval_unit,
};

static EVERY_N_RE: OnceLock<Regex> = OnceLock::new();

/// 在 Tool 写库前补齐用户自然语言中的 recurrence 业务语义。
pub(super) fn apply_recurrence_intent_from_text(
    draft: &mut TodoItemDraft,
) -> Result<(), TodoError> {
    if has_explicit_recurrence_fields(draft) || draft.has_explicit_no_recurrence_marker() {
        ensure_recurring_reminder_anchor(draft)?;
        return Ok(());
    }
    let source = draft.raw_text.as_deref().unwrap_or(&draft.title);
    let Some((kind, rule)) = parse_recurrence_from_text(source)? else {
        return Ok(());
    };
    draft.recurrence_kind = kind;
    draft.recurrence_interval = rule.interval;
    draft.recurrence_unit = rule.unit;
    draft.recurrence_interval_days = if matches!(rule.unit, TodoRecurrenceUnit::Day) {
        rule.interval
    } else {
        0
    };
    ensure_recurring_reminder_anchor(draft)
}

fn has_explicit_recurrence_fields(draft: &TodoItemDraft) -> bool {
    !matches!(draft.recurrence_kind, TodoRecurrenceKind::None)
        || draft.recurrence_interval_days > 0
        || draft.recurrence_interval > 0
}

fn ensure_recurring_reminder_anchor(draft: &mut TodoItemDraft) -> Result<(), TodoError> {
    if matches!(draft.recurrence_kind, TodoRecurrenceKind::None)
        || draft.due_date.is_some()
        || draft.due_at.is_some()
        || draft.reminder_at.is_some()
    {
        return Ok(());
    }
    if !is_recurring_reminder_intent(draft) {
        return Ok(());
    }
    let (_, rule) =
        recurrence_rule_from_interval_unit(draft.recurrence_interval, draft.recurrence_unit)?;
    // 周期性纯提醒没有 due_at 锚点时，第一次提醒从创建时间后一个周期开始。
    // 后续推进仍复用 reminder_at + recurrence 的 outbox / sent hook 链路。
    draft.reminder_at = Some(next_reminder_from_now(rule)?);
    draft.time_precision = TodoTimePrecision::DateTime;
    Ok(())
}

fn is_recurring_reminder_intent(draft: &TodoItemDraft) -> bool {
    let mut source = String::new();
    if let Some(raw_text) = draft.raw_text.as_deref() {
        source.push_str(raw_text);
    }
    source.push_str(&draft.title);
    if let Some(detail) = draft.detail.as_deref() {
        source.push_str(detail);
    }
    let compact = source.split_whitespace().collect::<String>();
    ["提醒", "通知我", "提示我", "叫我", "喊我", "闹钟"]
        .iter()
        .any(|keyword| compact.contains(keyword))
}

fn next_reminder_from_now(rule: TodoRecurrenceRule) -> Result<String, TodoError> {
    let now = Utc::now()
        .with_timezone(&shanghai_offset())
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    shift_timestamp_by_calendar(&now, rule.interval, calendar_unit(&rule.unit), 1)
        .ok_or_else(|| TodoError::bad_request("重复提醒首次提醒时间生成失败。"))
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

fn parse_recurrence_from_text(
    text: &str,
) -> Result<Option<(TodoRecurrenceKind, TodoRecurrenceRule)>, TodoError> {
    let compact = text.split_whitespace().collect::<String>();
    if compact.is_empty() {
        return Ok(None);
    }
    if compact.contains("每隔几分钟")
        || compact.contains("隔几分钟")
        || compact.contains("每几分钟")
        || compact.contains("每隔几小时")
        || compact.contains("隔几小时")
        || compact.contains("每几小时")
        || compact.contains("每隔几天")
        || compact.contains("隔几天")
        || compact.contains("每几天")
    {
        return Err(TodoError::bad_request(
            "重复规则缺少具体数字，请明确说成“每隔 5 分钟”或“每隔 3 天”之类的规则。",
        ));
    }
    if compact.contains("隔天") || compact.contains("隔一天") || compact.contains("每隔一天")
    {
        let rule = TodoRecurrenceRule {
            interval: 2,
            unit: TodoRecurrenceUnit::Day,
        };
        return Ok(Some((TodoRecurrenceKind::EveryNDays, rule)));
    }
    if compact.contains("每分钟") || compact.contains("每分") {
        let rule = TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Minute,
        };
        return Ok(Some((TodoRecurrenceKind::EveryNMinutes, rule)));
    }
    if compact.contains("每小时") || compact.contains("每个小时") {
        let rule = TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Hour,
        };
        return Ok(Some((TodoRecurrenceKind::EveryNHours, rule)));
    }
    if compact.contains("每天") || compact.contains("每日") || compact.contains("每一天") {
        let rule = TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Day,
        };
        return Ok(Some((TodoRecurrenceKind::Daily, rule)));
    }
    if compact.contains("每周")
        || compact.contains("每星期")
        || compact.contains("每个星期")
        || compact.contains("每礼拜")
        || compact.contains("每个礼拜")
    {
        let rule = TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Week,
        };
        return Ok(Some((TodoRecurrenceKind::Weekly, rule)));
    }
    if compact.contains("每月") || compact.contains("每个月") {
        let rule = TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Month,
        };
        return Ok(Some((TodoRecurrenceKind::Monthly, rule)));
    }
    if compact.contains("每年") || compact.contains("每一年") {
        let rule = TodoRecurrenceRule {
            interval: 1,
            unit: TodoRecurrenceUnit::Year,
        };
        return Ok(Some((TodoRecurrenceKind::Yearly, rule)));
    }

    let regex = EVERY_N_RE.get_or_init(|| {
        Regex::new(
            r"(?P<prefix>每隔|隔|每)(?P<n>[0-9一二两三四五六七八九十百]+)(?P<unit>分钟|分|小时|天|周|星期|礼拜|个月|月|年)",
        )
        .expect("valid recurrence regex")
    });
    let Some(captures) = regex.captures(&compact) else {
        return Ok(None);
    };
    let number = captures
        .name("n")
        .and_then(|value| parse_small_positive_number(value.as_str()))
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| TodoError::bad_request("重复间隔必须是正整数。"))?;
    let prefix = captures
        .name("prefix")
        .map(|value| value.as_str())
        .unwrap_or("");
    let unit_text = captures
        .name("unit")
        .map(|value| value.as_str())
        .unwrap_or("天");
    let unit = match unit_text {
        "分钟" | "分" => TodoRecurrenceUnit::Minute,
        "小时" => TodoRecurrenceUnit::Hour,
        "天" => TodoRecurrenceUnit::Day,
        "周" | "星期" | "礼拜" => TodoRecurrenceUnit::Week,
        "个月" | "月" => TodoRecurrenceUnit::Month,
        "年" => TodoRecurrenceUnit::Year,
        _ => TodoRecurrenceUnit::Day,
    };
    if matches!(unit, TodoRecurrenceUnit::Day) && number == 1 && matches!(prefix, "每隔" | "隔")
    {
        let rule = TodoRecurrenceRule {
            interval: 2,
            unit: TodoRecurrenceUnit::Day,
        };
        return Ok(Some((TodoRecurrenceKind::EveryNDays, rule)));
    }
    if number == 1 {
        let rule = TodoRecurrenceRule { interval: 1, unit };
        return Ok(Some((recurrence_kind_for_rule(&rule), rule)));
    }
    let (kind, rule) = recurrence_rule_from_interval_unit(number, unit)?;
    Ok(Some((kind, rule)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tools::todo::TodoTimePrecision;

    fn assert_rule(
        parsed: (TodoRecurrenceKind, TodoRecurrenceRule),
        kind: TodoRecurrenceKind,
        interval: u32,
        unit: TodoRecurrenceUnit,
    ) {
        assert_eq!(parsed.0, kind);
        assert_eq!(parsed.1.interval, interval);
        assert_eq!(parsed.1.unit, unit);
    }

    #[test]
    fn parses_supported_recurrence_phrases() {
        assert_rule(
            parse_recurrence_from_text("每天 9 点提醒我喝水")
                .unwrap()
                .unwrap(),
            TodoRecurrenceKind::Daily,
            1,
            TodoRecurrenceUnit::Day,
        );
        assert_rule(
            parse_recurrence_from_text("每日 9 点提醒我喝水")
                .unwrap()
                .unwrap(),
            TodoRecurrenceKind::Daily,
            1,
            TodoRecurrenceUnit::Day,
        );
        assert_rule(
            parse_recurrence_from_text("每一天提醒我喝水")
                .unwrap()
                .unwrap(),
            TodoRecurrenceKind::Daily,
            1,
            TodoRecurrenceUnit::Day,
        );
        assert_rule(
            parse_recurrence_from_text("隔天提醒我浇花")
                .unwrap()
                .unwrap(),
            TodoRecurrenceKind::EveryNDays,
            2,
            TodoRecurrenceUnit::Day,
        );
        for phrase in [
            "隔一天提醒我浇花",
            "每隔一天提醒我浇花",
            "每隔 1 天提醒我浇花",
            "隔 1 天提醒我浇花",
        ] {
            assert_rule(
                parse_recurrence_from_text(phrase).unwrap().unwrap(),
                TodoRecurrenceKind::EveryNDays,
                2,
                TodoRecurrenceUnit::Day,
            );
        }
        assert_rule(
            parse_recurrence_from_text("每隔 3 天提醒我整理日志")
                .unwrap()
                .unwrap(),
            TodoRecurrenceKind::EveryNDays,
            3,
            TodoRecurrenceUnit::Day,
        );
        assert_rule(
            parse_recurrence_from_text("每三天整理一次")
                .unwrap()
                .unwrap(),
            TodoRecurrenceKind::EveryNDays,
            3,
            TodoRecurrenceUnit::Day,
        );
        assert_rule(
            parse_recurrence_from_text("每分钟报一次时间")
                .unwrap()
                .unwrap(),
            TodoRecurrenceKind::EveryNMinutes,
            1,
            TodoRecurrenceUnit::Minute,
        );
        assert_rule(
            parse_recurrence_from_text("每隔 5 分钟提醒我检查状态")
                .unwrap()
                .unwrap(),
            TodoRecurrenceKind::EveryNMinutes,
            5,
            TodoRecurrenceUnit::Minute,
        );
        assert_rule(
            parse_recurrence_from_text("每 2 小时提醒我休息")
                .unwrap()
                .unwrap(),
            TodoRecurrenceKind::EveryNHours,
            2,
            TodoRecurrenceUnit::Hour,
        );
    }

    #[test]
    fn ambiguous_recurrence_requires_specific_number() {
        let err = parse_recurrence_from_text("每隔几天提醒我复盘").unwrap_err();
        assert_eq!(err.code(), "bad_request");
    }

    #[test]
    fn recurring_reminder_gets_first_reminder_anchor() {
        let mut draft = TodoItemDraft {
            title: "报时".to_owned(),
            detail: None,
            raw_text: Some("每隔 5 分钟提醒我报时".to_owned()),
            due_date: None,
            due_at: None,
            reminder_at: None,
            time_precision: TodoTimePrecision::None,
            recurrence_kind: TodoRecurrenceKind::None,
            recurrence_interval_days: 0,
            recurrence_interval: 0,
            recurrence_unit: TodoRecurrenceUnit::Day,
        };

        apply_recurrence_intent_from_text(&mut draft).unwrap();

        assert_eq!(draft.recurrence_kind, TodoRecurrenceKind::EveryNMinutes);
        assert_eq!(draft.recurrence_interval, 5);
        assert_eq!(draft.recurrence_unit, TodoRecurrenceUnit::Minute);
        assert!(draft.reminder_at.is_some());
        assert_eq!(draft.time_precision, TodoTimePrecision::DateTime);
    }
}

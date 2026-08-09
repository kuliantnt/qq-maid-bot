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

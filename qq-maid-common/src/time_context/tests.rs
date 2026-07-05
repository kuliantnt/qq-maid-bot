use super::*;
use chrono::TimeZone;

fn fixed_context() -> RequestTimeContext {
    let offset = shanghai_offset();
    RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 6, 9, 18, 40, 0).unwrap())
}

#[test]
fn formats_request_time_context_fields() {
    let ctx = fixed_context();

    assert_eq!(ctx.current_date(), "2026-06-09");
    assert_eq!(ctx.current_time(), "2026-06-09 18:40:00");
    assert_eq!(ctx.timezone(), REQUEST_TIMEZONE);
}

#[test]
fn resolves_relative_dates_and_ranges() {
    let ctx = fixed_context();
    let resolved = ctx.resolve_relative_time_text("昨天、上周、下个月和去年");

    assert_eq!(
        resolved,
        vec![
            ResolvedTimeExpression {
                term: "昨天",
                value: "2026-06-08".to_owned()
            },
            ResolvedTimeExpression {
                term: "上周",
                value: "2026-06-01 至 2026-06-07".to_owned()
            },
            ResolvedTimeExpression {
                term: "下个月",
                value: "2026-07-01 至 2026-07-31".to_owned()
            },
            ResolvedTimeExpression {
                term: "去年",
                value: "2025-01-01 至 2025-12-31".to_owned()
            },
        ]
    );
}

#[test]
fn resolves_month_ranges_across_year_boundary() {
    let offset = shanghai_offset();
    let ctx =
        RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 1, 5, 8, 0, 0).unwrap());

    assert_eq!(
        ctx.resolve_relative_time_text("上个月")[0].value,
        "2025-12-01 至 2025-12-31"
    );
}

#[test]
fn infers_common_due_dates_from_text() {
    let offset = shanghai_offset();
    let ctx =
        RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 6, 10, 9, 0, 0).unwrap());

    assert_eq!(
        infer_due_date_from_text("三天后检查日志", &ctx).unwrap(),
        InferredDateExpression {
            date: "2026-06-13".to_owned(),
            precision: DateInferencePrecision::Date
        }
    );
    assert_eq!(
        infer_due_date_from_text("下周一处理", &ctx).unwrap(),
        InferredDateExpression {
            date: "2026-06-15".to_owned(),
            precision: DateInferencePrecision::Date
        }
    );
    assert_eq!(
        infer_due_date_from_text("周五提交", &ctx).unwrap(),
        InferredDateExpression {
            date: "2026-06-12".to_owned(),
            precision: DateInferencePrecision::Inferred
        }
    );
    assert_eq!(
        infer_due_date_from_text("6月15号提醒", &ctx).unwrap(),
        InferredDateExpression {
            date: "2026-06-15".to_owned(),
            precision: DateInferencePrecision::Date
        }
    );
    assert_eq!(
        infer_due_date_from_text("月底复盘", &ctx).unwrap(),
        InferredDateExpression {
            date: "2026-06-30".to_owned(),
            precision: DateInferencePrecision::Inferred
        }
    );
}

#[test]
fn formats_and_parses_local_timestamp_dates() {
    assert_eq!(
        local_date_from_timestamp("2026-06-08T20:30:00+00:00"),
        Some(ymd(2026, 6, 9))
    );
    assert_eq!(
        format_local_date_for_display("2026-06-08T20:30:00+00:00"),
        "2026-06-09"
    );
    assert_eq!(
        format_local_date_with_weekday_for_display("2026-06-12"),
        "06-12（五）"
    );
    assert_eq!(
        format_local_date_with_weekday_for_display("2026-06-13"),
        "06-13（六）"
    );
    assert_eq!(format_todo_time_for_display("2026-06-15"), "06-15（一）");
    assert_eq!(
        format_todo_time_for_display("2026-06-15 12:30:00"),
        "06-15（一）12:30"
    );
    assert_eq!(
        format_todo_time_for_display("2026-06-15 12:30"),
        "06-15（一）12:30"
    );
    assert_eq!(
        format_todo_time_for_display("2026-06-15T12:30:00+08:00"),
        "06-15（一）12:30"
    );
    assert_eq!(
        format_todo_time_for_display("2026-06-15（推测）"),
        "06-15（一）"
    );
    assert_eq!(
        format_todo_time_for_display("2026-06-15 12:30:00【推测】"),
        "06-15（一）12:30"
    );
    assert_eq!(
        format_todo_time_for_display("坏数据（推测）"),
        "坏数据（推测）"
    );
    assert_eq!(
        format_todo_time_chip_for_display_with_year("2025-07-10 07:00:00", 2025),
        "07-10 7:00（四）"
    );
    assert_eq!(
        format_todo_time_chip_for_display_with_year("2025-07-10", 2025),
        "07-10（四）"
    );
    assert_eq!(
        format_todo_time_chip_for_display_with_year("2026-01-02 05:00", 2025),
        "26-01-02 5:00（五）"
    );
    assert_eq!(
        format_todo_time_chip_for_display_with_year("2025-08-02T05:00:00+08:00", 2025),
        "08-02 5:00（六）"
    );
    assert_eq!(
        format_todo_time_chip_for_display_with_year("坏数据（推测）", 2025),
        "坏数据（推测）"
    );
    assert_eq!(format_local_date_for_display("2026-06-09"), "2026-06-09");
    assert_eq!(
        format_local_time_for_display("2026-06-08T20:30:00+00:00"),
        "2026-06-09 04:30:00"
    );
    assert_eq!(
        format_local_time_for_display("2026-06-12T20:15"),
        "2026-06-12 20:15:00"
    );
    assert_eq!(
        format_local_time_for_display("2026-06-09T12:00:00+08:00"),
        "2026-06-09 12:00:00"
    );
    assert!(is_valid_ymd_date("2026-06-09"));
    assert!(has_valid_ymd_date_prefix("2026-06-09 12:00:00"));
    assert!(!has_valid_ymd_date_prefix("2026-99-99 12:00:00"));
}

#[test]
fn parses_local_datetime_for_comparison() {
    assert_eq!(
        parse_local_datetime_for_comparison("2026-07-01 09:00")
            .unwrap()
            .to_rfc3339(),
        "2026-07-01T09:00:00+08:00"
    );
    assert_eq!(
        parse_local_datetime_for_comparison("2026-07-01T01:00:00+00:00")
            .unwrap()
            .to_rfc3339(),
        "2026-07-01T09:00:00+08:00"
    );
    assert_eq!(parse_local_date_string("2026-07-01"), Some(ymd(2026, 7, 1)));
    assert!(parse_local_datetime_for_comparison("bad").is_none());
    assert!(parse_local_date_string("2026-99-99").is_none());
}

#[test]
fn computes_cycles_to_advance_after_now() {
    let offset = shanghai_offset();
    let now = offset.with_ymd_and_hms(2026, 7, 5, 10, 0, 0).unwrap();
    let overdue_daily = offset.with_ymd_and_hms(2026, 7, 1, 9, 0, 0).unwrap();
    let future_daily = offset.with_ymd_and_hms(2099, 1, 1, 9, 0, 0).unwrap();

    assert_eq!(
        cycles_to_advance_datetime_after(overdue_daily, now, 1, 100),
        Some(5)
    );
    assert_eq!(
        cycles_to_advance_datetime_after(overdue_daily, now, 2, 100),
        Some(3)
    );
    assert_eq!(
        cycles_to_advance_datetime_after(future_daily, now, 1, 100),
        Some(1)
    );
    assert_eq!(
        cycles_to_advance_date_after(ymd(2026, 7, 1), ymd(2026, 7, 5), 1, 100),
        Some(5)
    );
    assert_eq!(
        cycles_to_advance_datetime_after(overdue_daily, now, 0, 100),
        None
    );
    assert_eq!(
        cycles_to_advance_datetime_after(overdue_daily, now, 1, 1),
        None
    );
}

#[test]
fn formats_diagnostic_times_for_ping() {
    assert_eq!(unix_seconds_marker(1), "unix:1");
    assert_eq!(
        format_unix_seconds_for_display(1),
        "1970-01-01 08:00:01 +08:00 (unix:1)"
    );
    assert_eq!(
        format_diagnostic_time_for_display("unix:1781726091"),
        "2026-06-18 03:54:51 +08:00 (unix:1781726091)"
    );
    assert_eq!(
        format_diagnostic_time_for_display("2026-06-08T20:30:00+00:00"),
        "2026-06-09 04:30:00 +08:00"
    );
    assert_eq!(
        format_diagnostic_time_for_display("2026-06-09T12:00:00+08:00"),
        "2026-06-09 12:00:00 +08:00"
    );
    assert_eq!(
        format_diagnostic_time_for_display("2026-06-12T20:15"),
        "2026-06-12 20:15:00 +08:00"
    );
    assert_eq!(
        format_diagnostic_time_for_display("not-a-date"),
        "not-a-date"
    );
}

#[test]
fn formats_ping_diagnostic_times_for_summary_view() {
    assert_eq!(
        format_diagnostic_time_without_unix_for_display("unix:1"),
        "1970-01-01 08:00:01 +08:00"
    );
    assert_eq!(
        format_diagnostic_clock_time_for_display("unix:1781726091"),
        "03:54:51"
    );
    assert_eq!(
        diagnostic_time_unix_seconds("2026-06-09T12:00:00+08:00"),
        Some(1_780_977_600)
    );
    assert_eq!(
        format_diagnostic_time_ago_for_display_at("unix:1000", 1000),
        Some("刚刚".to_owned())
    );
    assert_eq!(
        format_diagnostic_time_ago_for_display_at("unix:970", 1000),
        Some("30秒前".to_owned())
    );
    assert_eq!(
        format_diagnostic_time_ago_for_display_at("unix:700", 1000),
        Some("5分钟前".to_owned())
    );
    assert_eq!(
        format_diagnostic_time_ago_for_display_at("unix:1100", 1000),
        Some("1分钟40秒后".to_owned())
    );
    assert_eq!(
        format_diagnostic_elapsed_between_for_display("unix:1000", "unix:1005"),
        Some("5秒".to_owned())
    );
    assert_eq!(
        format_duration_for_display(StdDuration::from_secs(4 * 3600 + 26 * 60)),
        "4小时26分钟"
    );
}

#[test]
fn rss_time_display_converts_common_offsets_to_shanghai_time() {
    struct Case {
        name: &'static str,
        input: &'static str,
        expected: &'static str,
    }

    let cases = [
        Case {
            name: "utc_rfc3339_to_utc_plus_8",
            input: "2026-06-16T20:30:00Z",
            expected: "2026-06-17 04:30",
        },
        Case {
            name: "positive_offset_to_utc_plus_8",
            input: "2026-06-17T10:15:00+02:00",
            expected: "2026-06-17 16:15",
        },
        Case {
            name: "negative_offset_to_utc_plus_8",
            input: "2026-06-17T10:15:00-04:00",
            expected: "2026-06-17 22:15",
        },
        Case {
            name: "already_utc_plus_8_is_not_shifted_again",
            input: "2026-06-17T10:15:00+08:00",
            expected: "2026-06-17 10:15",
        },
        Case {
            name: "rfc2822_gmt_to_utc_plus_8",
            input: "Wed, 17 Jun 2026 08:00:00 GMT",
            expected: "2026-06-17 16:00",
        },
        Case {
            name: "rfc3339_zero_offset_to_utc_plus_8",
            input: "2026-06-17T08:00:00+00:00",
            expected: "2026-06-17 16:00",
        },
        Case {
            name: "invalid_keeps_original_text",
            input: "not-a-date",
            expected: "not-a-date",
        },
    ];

    for case in cases {
        assert_eq!(
            format_rss_time_for_display(case.input),
            case.expected,
            "case '{}' failed",
            case.name
        );
    }
}

#[test]
fn parses_reusable_date_boundary_expressions() {
    let ctx = fixed_context();

    let yesterday_before = parse_date_boundary_expression("昨天之前", &ctx).unwrap();
    assert_eq!(yesterday_before.kind, DateBoundaryKind::Before);
    assert_eq!(yesterday_before.target_date, ymd(2026, 6, 8));
    assert_eq!(yesterday_before.before_date, ymd(2026, 6, 8));

    let yesterday_inclusive = parse_date_boundary_expression("昨天以前", &ctx).unwrap();
    assert_eq!(yesterday_inclusive.kind, DateBoundaryKind::OnOrBefore);
    assert_eq!(yesterday_inclusive.target_date, ymd(2026, 6, 8));
    assert_eq!(yesterday_inclusive.before_date, ymd(2026, 6, 9));

    let up_to_yesterday = parse_date_boundary_expression("截至昨天", &ctx).unwrap();
    assert_eq!(up_to_yesterday.kind, DateBoundaryKind::OnOrBefore);
    assert_eq!(up_to_yesterday.target_date, ymd(2026, 6, 8));
    assert_eq!(up_to_yesterday.before_date, ymd(2026, 6, 9));

    let cutoff = parse_date_boundary_expression("截至 2026-06-01", &ctx).unwrap();
    assert_eq!(cutoff.kind, DateBoundaryKind::OnOrBefore);
    assert_eq!(cutoff.target_date, ymd(2026, 6, 1));
    assert_eq!(cutoff.before_date, ymd(2026, 6, 2));

    let today_before = parse_date_boundary_expression("今天之前", &ctx).unwrap();
    assert_eq!(today_before.kind, DateBoundaryKind::Before);
    assert_eq!(today_before.before_date, ymd(2026, 6, 9));
}

#[test]
fn parses_reusable_single_date_expressions() {
    let ctx = fixed_context();

    let today = parse_single_date_expression("查看今天待办", &ctx).unwrap();
    assert_eq!(today.raw, "今天");
    assert_eq!(today.date, ymd(2026, 6, 9));

    let tomorrow = parse_single_date_expression("明天要做什么", &ctx).unwrap();
    assert_eq!(tomorrow.raw, "明天");
    assert_eq!(tomorrow.date, ymd(2026, 6, 10));

    let iso = parse_single_date_expression("查看 2026-07-05 的待办", &ctx).unwrap();
    assert_eq!(iso.raw, "2026-07-05");
    assert_eq!(iso.date, ymd(2026, 7, 5));

    let month_day = parse_single_date_expression("查看 7 月 5 日的待办", &ctx).unwrap();
    assert_eq!(month_day.raw, "7月5日");
    assert_eq!(month_day.date, ymd(2026, 7, 5));
}

#[test]
fn matches_timestamps_by_local_natural_date() {
    assert!(timestamp_matches_local_date(
        "2026-06-08T16:00:00+00:00",
        ymd(2026, 6, 9)
    ));
    assert!(timestamp_matches_local_date("2026-06-09", ymd(2026, 6, 9)));
    assert!(!timestamp_matches_local_date(
        "2026-06-09T16:00:00+00:00",
        ymd(2026, 6, 9)
    ));
}

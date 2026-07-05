use std::sync::LazyLock;

use chrono::{Datelike, Duration, NaiveDate};
use regex::Regex;

use super::{RequestTimeContext, parse_small_positive_number};

/// 匹配 "最近N天" 中文日期范围表达。
static RECENT_DAYS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"最近(?P<num>\d+|[一二两三四五六七八九十]+)天").unwrap());

/// 从用户文本中归一化出的本地日期闭区间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateRangeExpression {
    pub raw: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
}

/// 从一段自然语言中提取高频中文日期范围。
///
/// 该函数只接受本地代码明确支持的相对表达，所有结果都基于请求级 now + timezone
/// 计算；模型输出的绝对日期不作为最终事实入口。
pub fn parse_date_range_expression(
    text: &str,
    ctx: &RequestTimeContext,
) -> Option<DateRangeExpression> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    let compact = raw
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    let today = ctx.local_date();
    let this_week_start = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));

    let (term, start, end) = if compact.contains("明后天") {
        (
            "明后天",
            today + Duration::days(1),
            today + Duration::days(2),
        )
    } else if compact.contains("这两天") {
        ("这两天", today - Duration::days(1), today)
    } else if compact.contains("这几天") {
        ("这几天", today - Duration::days(2), today)
    } else if let Some(captures) = RECENT_DAYS_RE.captures(&compact) {
        let days = captures
            .name("num")
            .and_then(|value| parse_small_positive_number(value.as_str()))?;
        if days <= 0 {
            return None;
        }
        ("最近N天", today - Duration::days(days - 1), today)
    } else if compact.contains("前天") {
        ("前天", today - Duration::days(2), today - Duration::days(2))
    } else if compact.contains("昨天") {
        ("昨天", today - Duration::days(1), today - Duration::days(1))
    } else if compact.contains("今天") {
        ("今天", today, today)
    } else if compact.contains("本周") || compact.contains("这周") {
        ("本周", this_week_start, this_week_start + Duration::days(6))
    } else if compact.contains("上周") {
        (
            "上周",
            this_week_start - Duration::days(7),
            this_week_start - Duration::days(1),
        )
    } else if compact.contains("下周") {
        (
            "下周",
            this_week_start + Duration::days(7),
            this_week_start + Duration::days(13),
        )
    } else if compact.contains("本月") || compact.contains("这个月") {
        let (start, end) = month_range(today.year(), today.month());
        ("本月", start, end)
    } else if compact.contains("上月") || compact.contains("上个月") {
        let (year, month) = shift_month(today, -1);
        let (start, end) = month_range(year, month);
        ("上月", start, end)
    } else {
        return None;
    };
    Some(DateRangeExpression::new(term, start, end))
}

impl DateRangeExpression {
    fn new(raw: impl Into<String>, start: NaiveDate, end: NaiveDate) -> Self {
        Self {
            raw: raw.into(),
            start,
            end,
        }
    }

    pub fn start_string(&self) -> String {
        format_date(self.start)
    }

    pub fn end_string(&self) -> String {
        format_date(self.end)
    }
}

fn shift_month(date: NaiveDate, offset: i32) -> (i32, u32) {
    let month_zero = date.month() as i32 - 1 + offset;
    let year = date.year() + month_zero.div_euclid(12);
    let month = month_zero.rem_euclid(12) + 1;
    (year, month as u32)
}

fn month_range(year: i32, month: u32) -> (NaiveDate, NaiveDate) {
    let start = ymd(year, month, 1);
    let next_start = if month == 12 {
        ymd(year + 1, 1, 1)
    } else {
        ymd(year, month + 1, 1)
    };
    (start, next_start - Duration::days(1))
}

fn ymd(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid generated date")
}

fn format_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

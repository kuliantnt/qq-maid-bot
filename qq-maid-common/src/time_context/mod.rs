//! 时间上下文解析模块。
//!
//! 提供北京时间（Asia/Shanghai）的当前时间获取、中文自然语言日期推断、
//! 相对时间解析、日期格式化等功能，用于理解用户请求中的时间语义。

use std::sync::LazyLock;

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone,
    Utc,
};
use regex::Regex;

mod date_range;
mod display;
mod recurrence;

pub use date_range::{DateRangeExpression, parse_date_range_expression};
pub use display::{
    diagnostic_time_unix_seconds, format_diagnostic_clock_time_for_display,
    format_diagnostic_elapsed_between_for_display, format_diagnostic_time_ago_for_display,
    format_diagnostic_time_ago_for_display_at, format_diagnostic_time_for_display,
    format_diagnostic_time_without_unix_for_display, format_duration_for_display,
    format_local_date_for_display, format_local_date_with_weekday_for_display,
    format_local_time_for_display, format_rss_time_for_display, format_todo_time_chip_for_display,
    format_todo_time_chip_for_display_with_year, format_todo_time_for_display,
    format_unix_seconds_for_display, local_date_from_timestamp, now_diagnostic_time_for_display,
    now_unix_seconds, now_unix_seconds_marker, timestamp_matches_local_date, unix_seconds_marker,
};
pub use recurrence::{
    CalendarRecurrenceUnit, cycles_to_advance_date_after_calendar,
    cycles_to_advance_datetime_after_calendar, shift_local_date_string_by_calendar,
    shift_timestamp_by_calendar,
};

/// 请求上下文使用的时区（北京时间）。
pub const REQUEST_TIMEZONE: &str = "Asia/Shanghai";
/// 东八区固定偏移秒数。
const SHANGHAI_OFFSET_SECONDS: i32 = 8 * 60 * 60;

/// 匹配 "X天后" 中文日期表达的正则（支持数字和汉字数字）。
static DAYS_LATER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?P<num>\d+|[一二两三四五六七八九十]+)\s*天后").unwrap());
/// 匹配 "下周X" 中文星期表达的正则。
static NEXT_WEEKDAY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"下周(?P<day>[一二三四五六日天1-7])").unwrap());
/// 匹配 "周X/星期X/礼拜X" 中文星期表达的正则。
static WEEKDAY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:周|星期|礼拜)(?P<day>[一二三四五六日天1-7])").unwrap());
/// 匹配 "YYYY年M月D日" 完整中文日期格式的正则。
static FULL_DATE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<year>\d{4})年(?P<month>\d{1,2})月(?P<day>\d{1,2})(?:日|号)?").unwrap()
});
/// 匹配 "M月D日" 月日表达的正则（跨年时自动推到明年）。
static MONTH_DAY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?P<month>\d{1,2})月(?P<day>\d{1,2})(?:日|号)?").unwrap());
/// 匹配 "YYYY-MM-DD" ISO 日期格式。
static ISO_DATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?P<date>\d{4}-\d{1,2}-\d{1,2})").unwrap());

/// 请求时间上下文，封装当前日期、时间和时区信息。
///
/// 用于解析用户请求中的相对时间词（今天、明天、上周等）
/// 并提供给业务层作为时间感知上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTimeContext {
    current_date: String,
    current_time: String,
    timezone: &'static str,
    local_date: NaiveDate,
}

/// 已解析的相对时间表达，包含原文词条和解析后的具体日期。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTimeExpression {
    pub term: &'static str,
    pub value: String,
}

/// 日期边界类型：严格之前（Before）或包含当天（OnOrBefore）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateBoundaryKind {
    Before,
    OnOrBefore,
}

/// 日期边界表达式，用于解析 "昨天之前"、"截至今天" 等条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateBoundaryExpression {
    pub raw: String,
    pub kind: DateBoundaryKind,
    pub target_date: NaiveDate,
    pub before_date: NaiveDate,
}

/// 日期推断的精度：明确指定（Date）或模糊推断（Inferred）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateInferencePrecision {
    Date,
    Inferred,
}

/// 从文本推断出的日期表达式，包含具体日期和精度标记。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredDateExpression {
    pub date: String,
    pub precision: DateInferencePrecision,
}

/// 从用户文本中解析出的单日本地日期。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SingleDateExpression {
    pub raw: String,
    pub date: NaiveDate,
}

/// 获取当前请求时间上下文（基于北京时间）。
pub fn request_time_context() -> RequestTimeContext {
    RequestTimeContext::now()
}

/// 获取当前北京时间 ISO8601 格式字符串（含时区偏移）。
pub fn now_iso_cn() -> String {
    Utc::now()
        .with_timezone(&shanghai_offset())
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// 从用户文本中推断截止日期，支持今天、明天、后天、X天/周后、具体日期等多种中文表达。
pub fn infer_due_date_from_text(
    text: &str,
    ctx: &RequestTimeContext,
) -> Option<InferredDateExpression> {
    let date = ctx.local_date();
    if text.contains("今天") {
        return Some(InferredDateExpression::date(date));
    }
    if text.contains("明天") {
        return Some(InferredDateExpression::date(date + Duration::days(1)));
    }
    if text.contains("后天") {
        return Some(InferredDateExpression::date(date + Duration::days(2)));
    }
    if let Some(captures) = DAYS_LATER_RE.captures(text)
        && let Some(days) = captures
            .name("num")
            .and_then(|value| parse_small_number(value.as_str()))
    {
        return Some(InferredDateExpression::date(date + Duration::days(days)));
    }
    if let Some(captures) = NEXT_WEEKDAY_RE.captures(text) {
        let target = parse_weekday(captures.name("day")?.as_str())?;
        let this_week_start =
            date - Duration::days(i64::from(date.weekday().num_days_from_monday()));
        let due = this_week_start + Duration::days(7 + target);
        return Some(InferredDateExpression::date(due));
    }
    if let Some(captures) = WEEKDAY_RE.captures(text)
        && !text.contains("下周")
    {
        let target = parse_weekday(captures.name("day")?.as_str())?;
        let current = i64::from(date.weekday().num_days_from_monday());
        let mut offset = target - current;
        if offset <= 0 {
            offset += 7;
        }
        return Some(InferredDateExpression::inferred(
            date + Duration::days(offset),
        ));
    }
    if let Some(captures) = FULL_DATE_RE.captures(text) {
        let year = captures.name("year")?.as_str().parse::<i32>().ok()?;
        let month = captures.name("month")?.as_str().parse::<u32>().ok()?;
        let day = captures.name("day")?.as_str().parse::<u32>().ok()?;
        return NaiveDate::from_ymd_opt(year, month, day).map(InferredDateExpression::date);
    }
    if let Some(captures) = MONTH_DAY_RE.captures(text) {
        let month = captures.name("month")?.as_str().parse::<u32>().ok()?;
        let day = captures.name("day")?.as_str().parse::<u32>().ok()?;
        let mut year = date.year();
        let mut due = NaiveDate::from_ymd_opt(year, month, day)?;
        if due < date {
            year += 1;
            due = NaiveDate::from_ymd_opt(year, month, day)?;
        }
        return Some(InferredDateExpression::date(due));
    }
    if text.contains("下个月初") {
        let (year, month) = shift_month(date, 1);
        return Some(InferredDateExpression::inferred(NaiveDate::from_ymd_opt(
            year, month, 1,
        )?));
    }
    if text.contains("月底") {
        return Some(InferredDateExpression::inferred(
            month_range(date.year(), date.month()).1,
        ));
    }
    None
}

/// 验证字符串是否为有效的 YYYY-MM-DD 日期格式。
pub fn is_valid_ymd_date(value: &str) -> bool {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

/// 验证字符串是否以有效的 YYYY-MM-DD 日期开头（用于日期时间字符串）。
pub fn has_valid_ymd_date_prefix(value: &str) -> bool {
    value.len() >= 10 && value.get(..10).is_some_and(is_valid_ymd_date)
}

/// 解析正整数，支持阿拉伯数字和小范围中文数字。
///
/// 当前用于“X 天后”“每隔 X 天”等中文时间/周期表达的共享解析。
pub fn parse_small_positive_number(value: &str) -> Option<i64> {
    parse_small_number(value)
}

/// 将 YYYY-MM-DD 日期字符串按本地自然日平移若干天。
pub fn shift_local_date_string(value: &str, days: i64) -> Option<String> {
    parse_ymd_date(value.trim()).map(|date| format_date(date + Duration::days(days)))
}

/// 将 RFC3339 或本地日期时间字符串按天平移，并尽量保留原始格式类别。
pub fn shift_timestamp_by_days(value: &str, days: i64) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return Some((datetime + Duration::days(days)).to_rfc3339());
    }
    for (parse_format, render_format) in [
        ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M:%S"),
        ("%Y-%m-%d %H:%M", "%Y-%m-%d %H:%M"),
        ("%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M:%S"),
        ("%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M"),
    ] {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(value, parse_format) {
            return Some(
                (datetime + Duration::days(days))
                    .format(render_format)
                    .to_string(),
            );
        }
    }
    None
}

/// 将 RFC3339 或本地日期时间字符串解析为北京时间。
///
/// 本地日期时间格式不携带时区，按 `Asia/Shanghai` 解释；用于业务层在保持
/// 原始字符串格式的同时，统一比较“是否已经落在当前时间之后”。
pub fn parse_local_datetime_for_comparison(value: &str) -> Option<DateTime<FixedOffset>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return Some(datetime.with_timezone(&shanghai_offset()));
    }
    parse_naive_local_datetime(value)
        .and_then(|datetime| shanghai_offset().from_local_datetime(&datetime).single())
}

/// 解析严格的 YYYY-MM-DD 本地自然日。
pub fn parse_local_date_string(value: &str) -> Option<NaiveDate> {
    parse_ymd_date(value.trim())
}

/// 计算日期时间锚点按 `interval_days` 推进到 `now` 之后需要的周期数。
///
/// 如果锚点本来就在未来，仍返回 1，表示“完成本次”后推进一个周期；
/// 如果锚点已逾期，则一次性跳过所有过去周期。`max_cycles` 用于保护异常旧数据。
pub fn cycles_to_advance_datetime_after(
    anchor: DateTime<FixedOffset>,
    now: DateTime<FixedOffset>,
    interval_days: u32,
    max_cycles: i64,
) -> Option<i64> {
    let step_seconds = i64::from(interval_days).checked_mul(24 * 60 * 60)?;
    if step_seconds <= 0 || max_cycles <= 0 {
        return None;
    }
    let cycles = if anchor > now {
        1
    } else {
        let diff_seconds = now.signed_duration_since(anchor).num_seconds();
        diff_seconds / step_seconds + 1
    };
    (1..=max_cycles).contains(&cycles).then_some(cycles)
}

/// 计算本地自然日锚点按 `interval_days` 推进到 `now` 之后需要的周期数。
pub fn cycles_to_advance_date_after(
    anchor: NaiveDate,
    now: NaiveDate,
    interval_days: u32,
    max_cycles: i64,
) -> Option<i64> {
    if interval_days == 0 || max_cycles <= 0 {
        return None;
    }
    let cycles = if anchor > now {
        1
    } else {
        let diff_days = now.signed_duration_since(anchor).num_days();
        diff_days / i64::from(interval_days) + 1
    };
    (1..=max_cycles).contains(&cycles).then_some(cycles)
}

/// 从一段自然语言或结构化文本中提取明确的单日日期。
///
/// 该 helper 面向“查询某一天的事项”这类业务复用：支持今天、明天、后天、
/// YYYY-MM-DD、YYYY年M月D日、M月D日，以及现有截止日期推断中的常见表达。
pub fn parse_single_date_expression(
    text: &str,
    ctx: &RequestTimeContext,
) -> Option<SingleDateExpression> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    let compact = raw
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();

    if compact.contains("今天") {
        return Some(SingleDateExpression::new("今天", ctx.local_date()));
    }
    if compact.contains("明天") {
        return Some(SingleDateExpression::new(
            "明天",
            ctx.local_date() + Duration::days(1),
        ));
    }
    if compact.contains("后天") {
        return Some(SingleDateExpression::new(
            "后天",
            ctx.local_date() + Duration::days(2),
        ));
    }
    if let Some(captures) = ISO_DATE_RE.captures(&compact) {
        let raw_date = captures.name("date")?.as_str();
        let date = parse_ymd_date(raw_date)?;
        return Some(SingleDateExpression::new(raw_date, date));
    }
    if let Some(captures) = FULL_DATE_RE.captures(&compact) {
        let raw_date = captures.get(0)?.as_str();
        let year = captures.name("year")?.as_str().parse::<i32>().ok()?;
        let month = captures.name("month")?.as_str().parse::<u32>().ok()?;
        let day = captures.name("day")?.as_str().parse::<u32>().ok()?;
        let date = NaiveDate::from_ymd_opt(year, month, day)?;
        return Some(SingleDateExpression::new(raw_date, date));
    }
    if let Some(captures) = MONTH_DAY_RE.captures(&compact) {
        let raw_date = captures.get(0)?.as_str();
        let month = captures.name("month")?.as_str().parse::<u32>().ok()?;
        let day = captures.name("day")?.as_str().parse::<u32>().ok()?;
        let mut year = ctx.local_date().year();
        let mut date = NaiveDate::from_ymd_opt(year, month, day)?;
        if date < ctx.local_date() {
            year += 1;
            date = NaiveDate::from_ymd_opt(year, month, day)?;
        }
        return Some(SingleDateExpression::new(raw_date, date));
    }

    let inferred = infer_due_date_from_text(&compact, ctx)?;
    let date = NaiveDate::parse_from_str(&inferred.date, "%Y-%m-%d").ok()?;
    Some(SingleDateExpression::new(inferred.date, date))
}

impl RequestTimeContext {
    pub fn now() -> Self {
        let offset = shanghai_offset();
        Self::from_datetime(Utc::now().with_timezone(&offset))
    }

    pub fn from_datetime(local_now: DateTime<FixedOffset>) -> Self {
        let local_date = local_now.date_naive();
        Self {
            current_date: format_date(local_date),
            current_time: local_now.format("%Y-%m-%d %H:%M:%S").to_string(),
            timezone: REQUEST_TIMEZONE,
            local_date,
        }
    }

    pub fn current_date(&self) -> &str {
        &self.current_date
    }

    pub fn current_time(&self) -> &str {
        &self.current_time
    }

    pub fn timezone(&self) -> &str {
        self.timezone
    }

    pub fn local_date(&self) -> NaiveDate {
        self.local_date
    }

    pub fn resolve_relative_time_text(&self, text: &str) -> Vec<ResolvedTimeExpression> {
        let mut resolved = Vec::new();
        let date = self.local_date;

        if text.contains("前天") {
            resolved.push(ResolvedTimeExpression::date(
                "前天",
                date - Duration::days(2),
            ));
        }
        if text.contains("昨天") {
            resolved.push(ResolvedTimeExpression::date(
                "昨天",
                date - Duration::days(1),
            ));
        }
        if text.contains("今天") {
            resolved.push(ResolvedTimeExpression::date("今天", date));
        }
        if text.contains("明天") {
            resolved.push(ResolvedTimeExpression::date(
                "明天",
                date + Duration::days(1),
            ));
        }
        if text.contains("上周") {
            let this_week_start =
                date - Duration::days(i64::from(date.weekday().num_days_from_monday()));
            resolved.push(ResolvedTimeExpression::range(
                "上周",
                this_week_start - Duration::days(7),
                this_week_start - Duration::days(1),
            ));
        }
        if text.contains("下周") {
            let this_week_start =
                date - Duration::days(i64::from(date.weekday().num_days_from_monday()));
            resolved.push(ResolvedTimeExpression::range(
                "下周",
                this_week_start + Duration::days(7),
                this_week_start + Duration::days(13),
            ));
        }
        if text.contains("上个月") {
            let (year, month) = shift_month(date, -1);
            let (start, end) = month_range(year, month);
            resolved.push(ResolvedTimeExpression::range("上个月", start, end));
        }
        if text.contains("下个月") {
            let (year, month) = shift_month(date, 1);
            let (start, end) = month_range(year, month);
            resolved.push(ResolvedTimeExpression::range("下个月", start, end));
        }
        if text.contains("今年") {
            resolved.push(ResolvedTimeExpression::range(
                "今年",
                ymd(date.year(), 1, 1),
                ymd(date.year(), 12, 31),
            ));
        }
        if text.contains("去年") {
            let year = date.year() - 1;
            resolved.push(ResolvedTimeExpression::range(
                "去年",
                ymd(year, 1, 1),
                ymd(year, 12, 31),
            ));
        }

        resolved
    }

    pub fn query_time_block(&self, query: &str) -> String {
        let resolved = self.resolve_relative_time_text(query);
        let resolution = if resolved.is_empty() {
            "未检测到需要解析的相对时间词。".to_owned()
        } else {
            resolved
                .iter()
                .map(|item| format!("{} = {}", item.term, item.value))
                .collect::<Vec<_>>()
                .join("\n")
        };

        format!(
            "当前本地日期：{}\n当前本地时间：{}\n当前时区：{}\n\n用户原始问题：\n{}\n\n程序解析：\n{}",
            self.current_date,
            self.current_time,
            self.timezone,
            query.trim(),
            resolution
        )
    }
}

impl ResolvedTimeExpression {
    fn date(term: &'static str, date: NaiveDate) -> Self {
        Self {
            term,
            value: format_date(date),
        }
    }

    fn range(term: &'static str, start: NaiveDate, end: NaiveDate) -> Self {
        Self {
            term,
            value: format!("{} 至 {}", format_date(start), format_date(end)),
        }
    }
}

impl InferredDateExpression {
    fn date(date: NaiveDate) -> Self {
        Self {
            date: format_date(date),
            precision: DateInferencePrecision::Date,
        }
    }

    fn inferred(date: NaiveDate) -> Self {
        Self {
            date: format_date(date),
            precision: DateInferencePrecision::Inferred,
        }
    }
}

impl SingleDateExpression {
    fn new(raw: impl Into<String>, date: NaiveDate) -> Self {
        Self {
            raw: raw.into(),
            date,
        }
    }
}

pub fn parse_date_boundary_expression(
    text: &str,
    ctx: &RequestTimeContext,
) -> Option<DateBoundaryExpression> {
    let raw = text.trim().to_owned();
    if raw.is_empty() {
        return None;
    }
    let compact = raw
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    let (date_text, kind) = if let Some(rest) = compact.strip_prefix("截至") {
        (rest, DateBoundaryKind::OnOrBefore)
    } else if let Some(rest) = compact.strip_suffix("之前") {
        (rest, DateBoundaryKind::Before)
    } else if let Some(rest) = compact.strip_suffix("以前") {
        (rest, DateBoundaryKind::OnOrBefore)
    } else {
        return None;
    };
    if date_text.is_empty() {
        return None;
    }

    let target_date = parse_boundary_date(date_text, ctx.local_date())?;
    let before_date = match kind {
        DateBoundaryKind::Before => target_date,
        DateBoundaryKind::OnOrBefore => target_date + Duration::days(1),
    };
    Some(DateBoundaryExpression {
        raw,
        kind,
        target_date,
        before_date,
    })
}

fn parse_boundary_date(text: &str, local_today: NaiveDate) -> Option<NaiveDate> {
    match text {
        "今天" => Some(local_today),
        "昨天" => Some(local_today - Duration::days(1)),
        _ => parse_ymd_date(text),
    }
}

fn parse_ymd_date(text: &str) -> Option<NaiveDate> {
    let mut parts = text.split('-');
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    NaiveDate::from_ymd_opt(year, month, day)
}

fn parse_naive_local_datetime(value: &str) -> Option<NaiveDateTime> {
    [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
}

fn parse_small_number(value: &str) -> Option<i64> {
    if let Ok(number) = value.parse::<i64>() {
        return (number > 0).then_some(number);
    }
    let mut total = 0_i64;
    let mut current = 0_i64;
    for ch in value.chars() {
        match ch {
            '一' => current = 1,
            '二' | '两' => current = 2,
            '三' => current = 3,
            '四' => current = 4,
            '五' => current = 5,
            '六' => current = 6,
            '七' => current = 7,
            '八' => current = 8,
            '九' => current = 9,
            '十' => {
                total += if current == 0 { 10 } else { current * 10 };
                current = 0;
            }
            _ => return None,
        }
    }
    let number = total + current;
    (number > 0).then_some(number)
}

fn parse_weekday(value: &str) -> Option<i64> {
    match value {
        "一" | "1" => Some(0),
        "二" | "2" => Some(1),
        "三" | "3" => Some(2),
        "四" | "4" => Some(3),
        "五" | "5" => Some(4),
        "六" | "6" => Some(5),
        "日" | "天" | "7" => Some(6),
        _ => None,
    }
}

pub fn shanghai_offset() -> FixedOffset {
    FixedOffset::east_opt(SHANGHAI_OFFSET_SECONDS).expect("valid Asia/Shanghai fixed offset")
}

fn format_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
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

#[cfg(test)]
mod tests;

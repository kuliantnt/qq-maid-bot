//! 时间上下文解析模块。
//!
//! 提供北京时间（Asia/Shanghai）的当前时间获取、中文自然语言日期推断、
//! 相对时间解析、日期格式化等功能，用于理解用户请求中的时间语义。

use std::{
    sync::LazyLock,
    time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
};

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone,
    Timelike, Utc,
};
use regex::Regex;

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

/// 从时间戳字符串中提取本地日期（北京时间），支持 RFC3339 和 YYYY-MM-DD 格式。
pub fn local_date_from_timestamp(value: &str) -> Option<NaiveDate> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return Some(datetime.with_timezone(&shanghai_offset()).date_naive());
    }
    value
        .get(..10)
        .and_then(|prefix| NaiveDate::parse_from_str(prefix, "%Y-%m-%d").ok())
}

/// 判断时间戳或 YYYY-MM-DD 日期是否落在指定本地自然日。
pub fn timestamp_matches_local_date(value: &str, date: NaiveDate) -> bool {
    local_date_from_timestamp(value).is_some_and(|local_date| local_date == date)
}

/// 格式化本地日期用于显示，将时间戳转为 YYYY-MM-DD 格式的日期。
pub fn format_local_date_for_display(value: &str) -> String {
    local_date_from_timestamp(value)
        .map(format_date)
        .unwrap_or_else(|| value.trim().to_owned())
}

/// 格式化日期为 "MM-DD（星期X）" 的简短显示格式。
pub fn format_local_date_with_weekday_for_display(value: &str) -> String {
    local_date_from_timestamp(value)
        .map(format_short_date_with_weekday)
        .unwrap_or_else(|| value.trim().to_owned())
}

/// 格式化本地时间用于显示，转为 "YYYY-MM-DD HH:MM:SS" 格式。
pub fn format_local_time_for_display(value: &str) -> String {
    let value = value.trim();
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return datetime
            .with_timezone(&shanghai_offset())
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
    }
    if let Some(datetime) = parse_naive_local_datetime(value) {
        return datetime.format("%Y-%m-%d %H:%M:%S").to_string();
    }
    value
        .replace('T', " ")
        .trim_end_matches("+08:00")
        .to_owned()
}

/// 格式化诊断时间，用于 `/ping` 等人工排障输出。
///
/// 诊断文本需要同时兼顾人眼可读和日志交叉定位，因此 Unix 秒会保留在括号中；
/// QQ 平台传入的 RFC3339 时间会统一换算成北京时间。
pub fn format_diagnostic_time_for_display(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return value.to_owned();
    }
    if let Some(seconds) = value
        .strip_prefix("unix:")
        .and_then(|seconds| seconds.parse::<i64>().ok())
    {
        return format_unix_seconds_for_display(seconds);
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return format_datetime_with_offset(datetime.with_timezone(&shanghai_offset()));
    }
    if let Some(datetime) = parse_naive_local_datetime(value) {
        return format!("{} +08:00", datetime.format("%Y-%m-%d %H:%M:%S"));
    }
    value
        .replace('T', " ")
        .trim_end_matches("+08:00")
        .to_owned()
}

/// 格式化诊断时间给默认用户视图使用，不展示 Unix 秒。
///
/// `/ping` 默认主视图应优先可读，Unix 秒只保留给 `/ping all` 调试详情。
pub fn format_diagnostic_time_without_unix_for_display(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return value.to_owned();
    }
    if let Some(seconds) = parse_unix_seconds_marker(value) {
        return format_unix_seconds_without_marker(seconds)
            .unwrap_or_else(|| format!("unix:{seconds}"));
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return format_datetime_with_offset(datetime.with_timezone(&shanghai_offset()));
    }
    if let Some(datetime) = parse_naive_local_datetime(value) {
        return format!("{} +08:00", datetime.format("%Y-%m-%d %H:%M:%S"));
    }
    value
        .replace('T', " ")
        .trim_end_matches("+08:00")
        .to_owned()
}

/// 格式化诊断事件的时分秒，用于 `/ping` 最近事件列表。
pub fn format_diagnostic_clock_time_for_display(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return value.to_owned();
    }
    if let Some(seconds) = diagnostic_time_unix_seconds(value)
        && let Some(datetime) = shanghai_datetime_from_unix_seconds(seconds)
    {
        return datetime.format("%H:%M:%S").to_string();
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return datetime
            .with_timezone(&shanghai_offset())
            .format("%H:%M:%S")
            .to_string();
    }
    if let Some(datetime) = parse_naive_local_datetime(value) {
        return datetime.format("%H:%M:%S").to_string();
    }
    format_diagnostic_time_without_unix_for_display(value)
}

/// 将诊断时间解析为 Unix 秒，供状态新旧关系和相对时间判断使用。
pub fn diagnostic_time_unix_seconds(value: &str) -> Option<i64> {
    let value = value.trim();
    if let Some(seconds) = parse_unix_seconds_marker(value) {
        return Some(seconds);
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        return Some(datetime.timestamp());
    }
    parse_naive_local_datetime(value).and_then(|datetime| {
        shanghai_offset()
            .from_local_datetime(&datetime)
            .single()
            .map(|datetime| datetime.timestamp())
    })
}

/// 格式化紧凑时长，例如 `4小时26分钟`，用于 `/ping` 运行时长和恢复耗时。
pub fn format_duration_for_display(duration: StdDuration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        return format!("{seconds}秒");
    }

    let minutes = seconds / 60;
    let seconds_remainder = seconds % 60;
    if minutes < 60 {
        return if seconds_remainder == 0 {
            format!("{minutes}分钟")
        } else {
            format!("{minutes}分钟{seconds_remainder}秒")
        };
    }

    let hours = minutes / 60;
    let minutes_remainder = minutes % 60;
    if hours < 24 {
        return if minutes_remainder == 0 {
            format!("{hours}小时")
        } else {
            format!("{hours}小时{minutes_remainder}分钟")
        };
    }

    let days = hours / 24;
    let hours_remainder = hours % 24;
    if hours_remainder == 0 {
        format!("{days}天")
    } else {
        format!("{days}天{hours_remainder}小时")
    }
}

/// 基于当前时间输出诊断时间的相对描述，例如 `刚刚`、`30秒前`。
pub fn format_diagnostic_time_ago_for_display(value: &str) -> Option<String> {
    format_diagnostic_time_ago_for_display_at(value, Utc::now().timestamp())
}

/// 基于指定 Unix 秒输出诊断时间的相对描述，主要供测试和状态展示复用。
pub fn format_diagnostic_time_ago_for_display_at(value: &str, now_seconds: i64) -> Option<String> {
    let seconds = diagnostic_time_unix_seconds(value)?;
    if now_seconds >= seconds {
        let diff = now_seconds - seconds;
        if diff < 5 {
            return Some("刚刚".to_owned());
        }
        return Some(format!(
            "{}前",
            format_duration_for_display(StdDuration::from_secs(diff as u64))
        ));
    }

    let future = seconds - now_seconds;
    Some(format!(
        "{}后",
        format_duration_for_display(StdDuration::from_secs(future as u64))
    ))
}

/// 计算两个诊断时间之间的耗时，无法解析或结束早于开始时返回 `None`。
pub fn format_diagnostic_elapsed_between_for_display(start: &str, end: &str) -> Option<String> {
    let start = diagnostic_time_unix_seconds(start)?;
    let end = diagnostic_time_unix_seconds(end)?;
    let elapsed = end.checked_sub(start)?;
    if elapsed < 0 {
        return None;
    }
    Some(format_duration_for_display(StdDuration::from_secs(
        elapsed as u64,
    )))
}

/// 格式化 Unix 秒为北京时间诊断文本。
pub fn format_unix_seconds_for_display(seconds: i64) -> String {
    format_unix_seconds_without_marker(seconds)
        .map(|datetime| format!("{datetime} (unix:{seconds})"))
        .unwrap_or_else(|| format!("unix:{seconds}"))
}

fn format_unix_seconds_without_marker(seconds: i64) -> Option<String> {
    shanghai_datetime_from_unix_seconds(seconds).map(format_datetime_with_offset)
}

fn shanghai_datetime_from_unix_seconds(seconds: i64) -> Option<DateTime<FixedOffset>> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .map(|datetime| datetime.with_timezone(&shanghai_offset()))
}

fn parse_unix_seconds_marker(value: &str) -> Option<i64> {
    value
        .strip_prefix("unix:")
        .and_then(|seconds| seconds.parse::<i64>().ok())
}

/// 获取当前北京时间诊断文本，保留 Unix 秒便于和日志时间线对应。
pub fn now_diagnostic_time_for_display() -> String {
    let now = Utc::now();
    format!(
        "{} (unix:{})",
        format_datetime_with_offset(now.with_timezone(&shanghai_offset())),
        now.timestamp()
    )
}

/// 获取当前 Unix 秒，供需要和诊断时间标记比较的运行态展示逻辑使用。
pub fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(StdDuration::ZERO)
        .as_secs()
        .min(i64::MAX as u64) as i64
}

/// 获取当前 Unix 秒标记，供运行时状态内部保存。
///
/// 展示给用户前仍应调用 `format_diagnostic_time_for_display`，避免 `/ping`
/// 直接输出裸 Unix 秒。
pub fn now_unix_seconds_marker() -> String {
    unix_seconds_marker(now_unix_seconds() as u64)
}

/// 将 Unix 秒转成运行时状态使用的稳定标记格式。
pub fn unix_seconds_marker(seconds: u64) -> String {
    format!("unix:{seconds}")
}

/// 格式化 RSS 发布时间用于用户展示。
///
/// RSS/Atom 源常见 RFC3339 与 RFC2822 两类时间格式；这里只在展示消息时
/// 转换为北京时间，不参与 RSS 条目指纹、去重或新旧判断。
pub fn format_rss_time_for_display(value: &str) -> String {
    let value = value.trim();
    parse_rss_datetime(value)
        .map(|datetime| {
            datetime
                .with_timezone(&shanghai_offset())
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| value.to_owned())
}

/// 格式化待办时间用于显示，支持 RFC3339、日期时间、纯日期及 "（推测）" 后缀。
pub fn format_todo_time_for_display(value: &str) -> String {
    let original = value.trim();
    if original.is_empty() {
        return original.to_owned();
    }
    let normalized = strip_todo_inferred_suffix(original);
    if let Ok(datetime) = DateTime::parse_from_rfc3339(normalized) {
        return format_todo_datetime(datetime.with_timezone(&shanghai_offset()).naive_local());
    }
    if let Some(datetime) = parse_naive_local_datetime(normalized) {
        return format_todo_datetime(datetime);
    }
    if let Ok(date) = NaiveDate::parse_from_str(normalized, "%Y-%m-%d") {
        return format_short_date_with_weekday(date);
    }
    original.to_owned()
}

/// 格式化待办时间为紧凑 chip 样式，用于列表和单条卡片展示。
///
/// 展示规则只影响用户可见文本，不参与时间解析或持久化：
/// - 当前年内显示 `MM-DD`，跨年显示 `YY-MM-DD`；
/// - 只有日期时不显示时间；
/// - 有具体时间时显示 `H:MM`；
/// - 星期始终放在反引号外，例如 `` `07-10 7:00`（五）``。
pub fn format_todo_time_chip_for_display(value: &str) -> String {
    format_todo_time_chip_for_display_with_year(value, current_cn_year())
}

/// 带显式当前年份的版本，供测试和固定时间上下文调用。
pub fn format_todo_time_chip_for_display_with_year(value: &str, current_year: i32) -> String {
    let original = value.trim();
    if original.is_empty() {
        return original.to_owned();
    }
    let normalized = strip_todo_inferred_suffix(original);
    if let Ok(datetime) = DateTime::parse_from_rfc3339(normalized) {
        return format_todo_datetime_chip(
            datetime.with_timezone(&shanghai_offset()).naive_local(),
            current_year,
        );
    }
    if let Some(datetime) = parse_naive_local_datetime(normalized) {
        return format_todo_datetime_chip(datetime, current_year);
    }
    if let Ok(date) = NaiveDate::parse_from_str(normalized, "%Y-%m-%d") {
        return format_todo_date_chip(date, current_year);
    }
    original.to_owned()
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

fn format_todo_datetime_chip(datetime: NaiveDateTime, current_year: i32) -> String {
    let date = datetime.date();
    format!(
        "`{} {}:{:02}`（{}）",
        todo_chip_date_label(date, current_year),
        datetime.hour(),
        datetime.minute(),
        chinese_weekday(date)
    )
}

fn format_todo_date_chip(date: NaiveDate, current_year: i32) -> String {
    format!(
        "`{}`（{}）",
        todo_chip_date_label(date, current_year),
        chinese_weekday(date)
    )
}

fn todo_chip_date_label(date: NaiveDate, current_year: i32) -> String {
    if date.year() == current_year {
        date.format("%m-%d").to_string()
    } else {
        date.format("%y-%m-%d").to_string()
    }
}

fn current_cn_year() -> i32 {
    Utc::now().with_timezone(&shanghai_offset()).year()
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

fn parse_rss_datetime(value: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value)
        .or_else(|_| DateTime::parse_from_rfc2822(value))
        .ok()
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

fn format_datetime_with_offset(datetime: DateTime<FixedOffset>) -> String {
    datetime.format("%Y-%m-%d %H:%M:%S %:z").to_string()
}

fn format_short_date_with_weekday(date: NaiveDate) -> String {
    format!("{}（{}）", date.format("%m-%d"), chinese_weekday(date))
}

fn format_todo_datetime(datetime: NaiveDateTime) -> String {
    format!(
        "{}{:02}:{:02}",
        format_short_date_with_weekday(datetime.date()),
        datetime.hour(),
        datetime.minute()
    )
}

fn chinese_weekday(date: NaiveDate) -> &'static str {
    match date.weekday().number_from_monday() {
        1 => "一",
        2 => "二",
        3 => "三",
        4 => "四",
        5 => "五",
        6 => "六",
        7 => "日",
        _ => unreachable!("weekday should be 1..=7"),
    }
}

fn strip_todo_inferred_suffix(value: &str) -> &str {
    value
        .strip_suffix("（推测）")
        .or_else(|| value.strip_suffix("【推测】"))
        .unwrap_or(value)
        .trim_end()
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

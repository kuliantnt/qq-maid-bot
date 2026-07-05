use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};

use super::{format_date, parse_naive_local_datetime, shanghai_offset};

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
/// - 不使用反引号等 Markdown 语法，避免 QQ Markdown 对行内 code 支持不稳定。
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

fn parse_rss_datetime(value: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(value)
        .or_else(|_| DateTime::parse_from_rfc2822(value))
        .ok()
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

fn format_todo_datetime_chip(datetime: NaiveDateTime, current_year: i32) -> String {
    let date = datetime.date();
    format!(
        "{} {}:{:02}（{}）",
        todo_chip_date_label(date, current_year),
        datetime.hour(),
        datetime.minute(),
        chinese_weekday(date)
    )
}

fn format_todo_date_chip(date: NaiveDate, current_year: i32) -> String {
    format!(
        "{}（{}）",
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

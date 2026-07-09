use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, TimeZone};

/// 日历重复周期单位。
///
/// Day / Week 按本地自然日推进；Month / Year 按日历语义推进，目标月份没有原日期时
/// 夹到该月最后一天，例如 1 月 31 日每月重复会推进到 2 月最后一天。
/// Minute / Hour 按固定时长推进，只对带时间分量的 datetime 锚点有意义；
/// 纯日期锚点没有 minute/hour 语义推进。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarRecurrenceUnit {
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

/// 将 YYYY-MM-DD 日期字符串按日历重复周期推进，所有日期运算都使用 checked 版本。
pub fn shift_local_date_string_by_calendar(
    value: &str,
    interval: u32,
    unit: CalendarRecurrenceUnit,
    cycles: i64,
) -> Option<String> {
    parse_ymd_date(value.trim())
        .and_then(|date| checked_shift_date_by_calendar(date, interval, unit, cycles))
        .map(format_date)
}

/// 将 RFC3339 或本地日期时间字符串按重复周期推进，并尽量保留原始格式类别。
///
/// Minute / Hour 按固定时长推进 datetime（保留 offset/time），
/// Day/Week/Month/Year 继续走日历锚点的日期推进路径。
pub fn shift_timestamp_by_calendar(
    value: &str,
    interval: u32,
    unit: CalendarRecurrenceUnit,
    cycles: i64,
) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(datetime) = DateTime::parse_from_rfc3339(value) {
        let shifted = checked_shift_datetime_by_calendar(datetime, interval, unit, cycles)?;
        return Some(shifted.to_rfc3339());
    }
    for (parse_format, render_format) in [
        ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M:%S"),
        ("%Y-%m-%d %H:%M", "%Y-%m-%d %H:%M"),
        ("%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M:%S"),
        ("%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M"),
    ] {
        if let Ok(datetime) = NaiveDateTime::parse_from_str(value, parse_format) {
            // Minute/Hour 只对带时间的锚点有意义；这里重用 datetime 路径并不能拿到 offset，
            // 因此一律交由 `checked_shift_naive_datetime_by_calendar` 处理，它对 Minute/Hour
            // 回于 Duration 推进、对其他单位回于日历日推进。
            let shifted =
                checked_shift_naive_datetime_by_calendar(datetime, interval, unit, cycles)?;
            return Some(shifted.format(render_format).to_string());
        }
    }
    None
}

/// 计算日期时间锚点按日历周期推进到 `now` 之后需要的周期数。
pub fn cycles_to_advance_datetime_after_calendar(
    anchor: DateTime<FixedOffset>,
    now: DateTime<FixedOffset>,
    interval: u32,
    unit: CalendarRecurrenceUnit,
    max_cycles: i64,
) -> Option<i64> {
    find_cycles_to_advance_after(max_cycles, |cycles| {
        checked_shift_datetime_by_calendar(anchor, interval, unit, cycles).map(|value| value > now)
    })
}

/// 计算本地自然日锚点按日历周期推进到 `now` 之后需要的周期数。
pub fn cycles_to_advance_date_after_calendar(
    anchor: NaiveDate,
    now: NaiveDate,
    interval: u32,
    unit: CalendarRecurrenceUnit,
    max_cycles: i64,
) -> Option<i64> {
    find_cycles_to_advance_after(max_cycles, |cycles| {
        checked_shift_date_by_calendar(anchor, interval, unit, cycles).map(|value| value > now)
    })
}

fn checked_shift_datetime_by_calendar(
    datetime: DateTime<FixedOffset>,
    interval: u32,
    unit: CalendarRecurrenceUnit,
    cycles: i64,
) -> Option<DateTime<FixedOffset>> {
    // Minute / Hour 是固定时长单位，直接在原 timezone 上加 Duration，
    // 不会跨越夏令时边界（本系统使用 Asia/Shanghai 固定 +08:00 offset）。
    if let Some(duration) = duration_for_calendar_unit(interval, unit, cycles) {
        return datetime.checked_add_signed(duration);
    }
    let offset = *datetime.offset();
    let shifted_date =
        checked_shift_date_by_calendar(datetime.date_naive(), interval, unit, cycles)?;
    let shifted = shifted_date.and_time(datetime.time());
    offset.from_local_datetime(&shifted).single()
}

/// 纯 NaiveDateTime 推进，用于本地字符串表达。
///
/// Minute / Hour 走 Duration；其他单位回于日历日推进，与带 offset 的
/// `checked_shift_datetime_by_calendar` 行为一致。
fn checked_shift_naive_datetime_by_calendar(
    datetime: NaiveDateTime,
    interval: u32,
    unit: CalendarRecurrenceUnit,
    cycles: i64,
) -> Option<NaiveDateTime> {
    if let Some(duration) = duration_for_calendar_unit(interval, unit, cycles) {
        return datetime.checked_add_signed(duration);
    }
    let shifted_date = checked_shift_date_by_calendar(datetime.date(), interval, unit, cycles)?;
    Some(shifted_date.and_time(datetime.time()))
}

/// Minute / Hour 推进需要的 Duration；其他单位返回 None 交日历路径处理。
fn duration_for_calendar_unit(
    interval: u32,
    unit: CalendarRecurrenceUnit,
    cycles: i64,
) -> Option<Duration> {
    if interval == 0 || cycles <= 0 {
        return None;
    }
    let total = i64::from(interval).checked_mul(cycles)?;
    match unit {
        CalendarRecurrenceUnit::Minute => Duration::try_minutes(total),
        CalendarRecurrenceUnit::Hour => Duration::try_hours(total),
        CalendarRecurrenceUnit::Day
        | CalendarRecurrenceUnit::Week
        | CalendarRecurrenceUnit::Month
        | CalendarRecurrenceUnit::Year => None,
    }
}

fn checked_shift_date_by_calendar(
    date: NaiveDate,
    interval: u32,
    unit: CalendarRecurrenceUnit,
    cycles: i64,
) -> Option<NaiveDate> {
    if interval == 0 || cycles <= 0 {
        return None;
    }
    let total = i64::from(interval).checked_mul(cycles)?;
    match unit {
        // Minute / Hour 对纯日期锚点没有语义：没有时间分量就无法推进到“下 N 分钟”。
        // 调用方（todo recurrence advance）需对 datetime 锥点走 datetime 推进路径。
        CalendarRecurrenceUnit::Minute | CalendarRecurrenceUnit::Hour => None,
        CalendarRecurrenceUnit::Day => {
            let duration = Duration::try_days(total)?;
            date.checked_add_signed(duration)
        }
        CalendarRecurrenceUnit::Week => {
            let days = total.checked_mul(7)?;
            let duration = Duration::try_days(days)?;
            date.checked_add_signed(duration)
        }
        CalendarRecurrenceUnit::Month => checked_add_months_clamped(date, total),
        CalendarRecurrenceUnit::Year => checked_add_months_clamped(date, total.checked_mul(12)?),
    }
}

fn checked_add_months_clamped(date: NaiveDate, months: i64) -> Option<NaiveDate> {
    let month_zero = i64::from(date.year())
        .checked_mul(12)?
        .checked_add(i64::from(date.month0()))?
        .checked_add(months)?;
    let year = i32::try_from(month_zero.div_euclid(12)).ok()?;
    let month = u32::try_from(month_zero.rem_euclid(12) + 1).ok()?;
    let day = date.day().min(days_in_month(year, month)?);
    NaiveDate::from_ymd_opt(year, month, day)
}

fn days_in_month(year: i32, month: u32) -> Option<u32> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 => {
            if is_leap_year(year) {
                Some(29)
            } else {
                Some(28)
            }
        }
        _ => None,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn find_cycles_to_advance_after(
    max_cycles: i64,
    mut is_after: impl FnMut(i64) -> Option<bool>,
) -> Option<i64> {
    if max_cycles <= 0 {
        return None;
    }
    if matches!(is_after(max_cycles), Some(false)) {
        return None;
    }

    let mut left = 1_i64;
    let mut right = max_cycles;
    while left < right {
        let mid = left + (right - left) / 2;
        match is_after(mid) {
            Some(false) => left = mid + 1,
            Some(true) | None => right = mid,
        }
    }
    matches!(is_after(left), Some(true)).then_some(left)
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

fn format_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

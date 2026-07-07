//! 天气查询指令的处理流程。
//! 负责解析 `/天气城市名` 和 `/城市名天气` 两种格式的指令，
//! 调用天气执行器获取实时天气、预报和可选增强摘要，并格式化为回复文本。
//! 同时处理找不到城市、超时、上游异常等错误场景。

use chrono::{Datelike, NaiveDate, Weekday};
use serde_json::{Value, json};

use crate::{
    error::LlmError,
    runtime::{
        command::{ParsedCommand, parse_slash_command},
        session::SessionRecord,
        weather::{
            AirQualitySummary, DEFAULT_FORECAST_DAYS, WeatherAlert, WeatherLifeIndex,
            WeatherOutcome, WeatherRequest, WeatherSupplement, WeatherSupplementStatus,
        },
    },
    util::time_context::{format_local_time_for_display, local_date_from_timestamp},
};

use super::{
    RespondResponse, RustRespondService,
    common::{CommandBody, clean_string, command_response, session_error, truncate_chars},
};
use qq_maid_common::markdown_strip::strip_markdown_for_chat;

// 城市名最大长度限制
const WEATHER_CITY_MAX_LENGTH: usize = 60;
// 天气查询指令的用法提示
const WEATHER_USAGE_REPLY: &str = "用法：/天气城市名 或 /城市名天气
例如：/天气杭州、/杭州天气";
// 城市名超长时的提示
const WEATHER_TOO_LONG_REPLY: &str = "城市名太长了，请压缩到 60 字以内再试。";
// 找不到城市时的回复
const WEATHER_NOT_FOUND_REPLY: &str = "【天气】

没找到这个城市。可以换成更完整的城市名再试，例如：/天气浙江杭州。";
// 天气服务超时时的回复
const WEATHER_TIMEOUT_REPLY: &str = "【天气】

天气服务超时了，请稍后再试。";
// 上游服务异常时的回复
const WEATHER_UPSTREAM_ERROR_REPLY: &str = "【天气】

天气服务暂时不可用，可能是上游接口、代理或网络配置异常。请稍后再试。";
// 天气回复整体字符上限，避免 QQ 聊天窗口被极长天气消息挤满。
const WEATHER_REPLY_MAX_CHARS: usize = 1200;
// 预警正文摘要的最大长度。标题和有效时间不截断，只压缩说明正文。
const WEATHER_ALERT_SUMMARY_MAX_CHARS: usize = 80;
// 当前天气里最多展示的生活指数条目数，保持移动端一屏可读。
const WEATHER_LIFE_INDEX_MAX_ITEMS: usize = 4;
// 生活指数单项值的截断长度，避免某个指数说明异常拉长整行。
const WEATHER_LIFE_INDEX_VALUE_MAX_CHARS: usize = 18;
// 当前展示保持最多两条预警，延续原先消息长度控制策略。
const WEATHER_ALERT_MAX_ITEMS: usize = 2;

impl RustRespondService {
    /// 处理天气查询指令的主入口。校验参数、调用天气执行器、格式化结果或错误回复。
    pub(super) async fn handle_weather_command(
        &self,
        command: ParsedCommand,
        user_text: &str,
        session: &mut SessionRecord,
    ) -> Result<RespondResponse, LlmError> {
        let city = command.argument.trim();
        if city.is_empty() {
            return Ok(command_response(
                WEATHER_USAGE_REPLY,
                Some(session.session_id.clone()),
                Some(command.action),
            ));
        }
        if city.chars().count() > WEATHER_CITY_MAX_LENGTH {
            return Ok(command_response(
                WEATHER_TOO_LONG_REPLY,
                Some(session.session_id.clone()),
                Some(command.action),
            ));
        }

        let outcome = match self
            .weather_executor
            .weather(WeatherRequest {
                city: city.to_owned(),
                forecast_days: DEFAULT_FORECAST_DAYS,
            })
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                tracing::warn!(
                    error_code = %err.code,
                    error_stage = %err.stage,
                    weather_provider = self.weather_executor.provider_name(),
                    "weather command failed"
                );
                let reply = format_weather_error_reply(&err);
                self.session_store
                    .append_exchange(session, user_text, &reply)
                    .map_err(session_error)?;

                let mut response = command_response(
                    CommandBody::dual(strip_markdown_for_chat(&reply), reply),
                    Some(session.session_id.clone()),
                    Some(command.action),
                );
                response.diagnostics = Some(json!({
                    "backend": "rust",
                    "session_backend": "rust",
                    "used_memory": false,
                    "used_search": false,
                    "used_weather": true,
                    "weather_provider": self.weather_executor.provider_name(),
                    "weather_error_code": err.code,
                    "weather_error_stage": err.stage,
                    "forecast_days": DEFAULT_FORECAST_DAYS,
                }));
                return Ok(response);
            }
        };

        let reply = format_weather_reply(&outcome);
        self.session_store
            .append_exchange(session, user_text, &reply)
            .map_err(session_error)?;

        let mut response = command_response(
            CommandBody::dual(strip_markdown_for_chat(&reply), reply),
            Some(session.session_id.clone()),
            Some(command.action),
        );
        let mut diagnostics = json!({
            "backend": "rust",
            "session_backend": "rust",
            "used_memory": false,
            "used_search": false,
            "used_weather": true,
            "weather_provider": outcome.provider,
            "original_city": city,
            "resolved_name": outcome.location.name,
            "resolved_adm1": outcome.location.admin1,
            "resolved_adm2": outcome.location.admin2,
            "resolved_location_id": outcome.location.id,
            "resolved_lat": outcome.location.latitude,
            "resolved_lon": outcome.location.longitude,
            "weather_elapsed_ms": outcome.elapsed_ms,
            "forecast_days": outcome.forecast_days,
        });
        append_weather_supplement_diagnostics(
            &mut diagnostics,
            "weather_alert",
            &outcome.alerts,
            outcome.alerts.data.as_ref().map(Vec::len).unwrap_or(0),
        );
        append_weather_supplement_diagnostics(
            &mut diagnostics,
            "weather_air_quality",
            &outcome.air_quality,
            usize::from(outcome.air_quality.data.is_some()),
        );
        append_weather_supplement_diagnostics(
            &mut diagnostics,
            "weather_life_indices",
            &outcome.life_indices,
            outcome
                .life_indices
                .data
                .as_ref()
                .map(Vec::len)
                .unwrap_or(0),
        );
        response.diagnostics = Some(diagnostics);
        Ok(response)
    }
}

/// 从用户文本中解析天气查询指令。
/// 支持 `/天气城市名` 和 `/城市名天气` 两种格式。
pub(super) fn parse_weather_command(text: &str) -> Option<ParsedCommand> {
    if let Some(command) = parse_slash_command(text)
        && command.action == "weather"
    {
        return Some(command);
    }

    let text = text.trim();
    if let Some(argument) = text.strip_prefix("/天气") {
        return Some(ParsedCommand {
            action: "weather".to_owned(),
            argument: argument.trim().to_owned(),
            raw_command: "天气".to_owned(),
        });
    }

    let command_text = text.strip_prefix('/')?.trim();
    let argument = command_text.strip_suffix("天气")?.trim();
    if argument.is_empty() {
        return None;
    }
    Some(ParsedCommand {
        action: "weather".to_owned(),
        argument: argument.to_owned(),
        raw_command: "天气".to_owned(),
    })
}

/// 格式化天气预报回复文本，包含当前实况和未来多日预报。
///
/// 这些格式化 helper 只对 `respond` 父模块可见，便于 `respond/tests/weather_format.rs`
/// 统一覆盖内部展示约束，同时避免泄露到更外层模块。
pub(super) fn format_weather_reply(outcome: &WeatherOutcome) -> String {
    let full_location = format_location(
        &outcome.location.name,
        outcome.location.admin2.as_deref(),
        outcome.location.admin1.as_deref(),
        outcome.location.country.as_deref(),
    );
    let title_location = outcome.location.name.trim();
    let reference_date = weather_reference_date(outcome);
    let mut lines = vec![format_weather_title(title_location, &full_location)];
    if let Some(location_detail) = format_location_detail(title_location, &full_location) {
        lines.push(format!("**{location_detail}**"));
    }
    lines.push(String::new());
    lines.extend(format_current_summary(
        &outcome.current,
        outcome.air_quality.data.as_ref(),
    ));

    if should_render_alerts(&outcome.alerts) {
        lines.push(String::new());
        lines.push("## ⚠️ 预警".to_owned());
        lines.push(String::new());
        append_alert_lines(&mut lines, &outcome.alerts, reference_date);
    }

    let displayed_days = outcome.daily.len().min(outcome.forecast_days as usize);
    lines.push(String::new());
    lines.push(format!("## 📅 未来 {} 天", displayed_days));
    lines.push(String::new());
    for day in outcome.daily.iter().take(displayed_days) {
        lines.push(format!(
            "- **{}**：{}",
            format_forecast_day_label(&day.date, reference_date),
            format_daily_summary(day)
        ));
    }

    if let Some(indices) = outcome.life_indices.data.as_ref()
        && let Some(summary) = format_life_indices(indices)
    {
        lines.push(String::new());
        lines.push("## 🧭 生活指数".to_owned());
        lines.push(String::new());
        lines.push(summary);
    }

    lines.push(String::new());
    lines.push("> 数据来源：和风天气".to_owned());
    truncate_chars(&lines.join("\n"), WEATHER_REPLY_MAX_CHARS)
}

fn format_weather_title(name: &str, full_location: &str) -> String {
    let name = name.trim();
    let title = if name.is_empty() {
        full_location.trim()
    } else {
        name
    };
    format!("# 🌦 {title}天气")
}

fn format_location_detail(name: &str, full_location: &str) -> Option<String> {
    let name = name.trim();
    let full_location = full_location.trim();
    if full_location.is_empty() || full_location == name {
        return None;
    }

    if let Some(rest) = full_location.strip_prefix(name) {
        return clean_string(rest.trim_start_matches('，').to_owned());
    }
    Some(full_location.to_owned())
}

fn format_current_summary(
    current: &crate::runtime::weather::CurrentWeather,
    air_quality: Option<&AirQualitySummary>,
) -> Vec<String> {
    let mut lines = vec![format!(
        "**当前 {}｜{}｜{}°C**  ",
        format_short_time(&current.time),
        weather_code_label(current.weather_code),
        format_number(current.temperature_c)
    )];
    let details = format_current_details(current);
    if !details.is_empty() {
        lines.push(format!("{details}  "));
    }
    if let Some(air_quality) = air_quality {
        lines.push(format!("空气质量：{}", format_air_quality(air_quality)));
    }
    lines
}

fn format_current_details(current: &crate::runtime::weather::CurrentWeather) -> String {
    let mut parts = Vec::new();
    if let Some(apparent) = current.apparent_temperature_c {
        parts.push(format!("体感 {}°C", format_number(apparent)));
    }
    if let Some(humidity) = current.humidity_percent {
        parts.push(format!("湿度 {humidity}%"));
    }
    if let Some(wind) = format_wind(
        current.wind_direction.as_deref(),
        current.wind_scale.as_deref(),
    ) {
        parts.push(wind);
    }
    parts.join(" · ")
}

fn should_render_alerts(alerts: &WeatherSupplement<Vec<WeatherAlert>>) -> bool {
    matches!(alerts.status, WeatherSupplementStatus::Available)
        && alerts
            .data
            .as_ref()
            .is_some_and(|alerts| !alerts.is_empty())
}

pub(super) fn append_alert_lines(
    lines: &mut Vec<String>,
    alerts: &WeatherSupplement<Vec<WeatherAlert>>,
    reference_date: Option<NaiveDate>,
) {
    let Some(alerts) = alerts.data.as_ref() else {
        return;
    };
    for (index, alert) in alerts.iter().take(WEATHER_ALERT_MAX_ITEMS).enumerate() {
        lines.push(format!(
            "- {} **{}**  ",
            alert_icon(alert.color_code.as_deref()),
            format_alert_title(alert)
        ));
        if let Some(detail) = format_alert_detail(alert, reference_date) {
            lines.push(format!("  {detail}"));
        }
        if index + 1 < alerts.len().min(WEATHER_ALERT_MAX_ITEMS) {
            lines.push(String::new());
        }
    }
}

pub(super) fn alert_icon(color_code: Option<&str>) -> &'static str {
    match alert_color_name(color_code) {
        Some("蓝色") => "🔵",
        Some("黄色") => "🟡",
        Some("橙色") => "🟠",
        Some("红色") => "🔴",
        _ => "⚠️",
    }
}

fn alert_color_name(color_code: Option<&str>) -> Option<&'static str> {
    match color_code?.trim().to_ascii_lowercase().as_str() {
        "blue" | "蓝" => Some("蓝色"),
        "yellow" | "黄" => Some("黄色"),
        "orange" | "橙" => Some("橙色"),
        "red" | "红" => Some("红色"),
        _ => None,
    }
}

pub(super) fn format_alert_title(alert: &WeatherAlert) -> String {
    if let Some(event_name) = alert
        .event_name
        .as_deref()
        .map(normalize_alert_event_name)
        .filter(|name| !name.is_empty())
    {
        if let Some(color_name) = alert_color_name(alert.color_code.as_deref()) {
            return format!("{event_name}{color_name}预警");
        }
        return if event_name.ends_with("预警") {
            event_name.to_owned()
        } else {
            format!("{event_name}预警")
        };
    }
    trim_alert_headline(&alert.headline, alert.sender_name.as_deref())
}

fn normalize_alert_event_name(name: &str) -> &str {
    name.trim()
        .trim_end_matches("预警信号")
        .trim_end_matches("预警")
        .trim()
}

fn trim_alert_headline(headline: &str, sender_name: Option<&str>) -> String {
    let original = headline.trim();
    let mut value = original;
    if let Some(sender_name) = sender_name.map(str::trim).filter(|value| !value.is_empty())
        && let Some(rest) = value.strip_prefix(sender_name)
    {
        value = rest.trim_start();
    }
    for prefix in ["继续发布", "升级发布", "发布了", "发布", "解除"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            value = rest.trim_start();
            break;
        }
    }
    value = value.trim_start_matches(['：', ':', '，', ',', '。', ' ']);
    if value.is_empty() {
        original.to_owned()
    } else {
        value.to_owned()
    }
}

pub(super) fn format_alert_detail(
    alert: &WeatherAlert,
    reference_date: Option<NaiveDate>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(time_range) = format_alert_time_range(alert, reference_date) {
        parts.push(time_range);
    }
    if let Some(summary) = format_alert_summary(alert) {
        parts.push(summary);
    }
    (!parts.is_empty()).then(|| parts.join("｜"))
}

fn format_alert_time_range(
    alert: &WeatherAlert,
    reference_date: Option<NaiveDate>,
) -> Option<String> {
    match (alert.issued_time.as_deref(), alert.expire_time.as_deref()) {
        (Some(issued), Some(expire)) => Some(format!(
            "{}—{}",
            format_alert_time_point(issued, reference_date),
            format_alert_time_point(expire, reference_date)
        )),
        (None, Some(expire)) => Some(format!(
            "截至 {}",
            format_alert_time_point(expire, reference_date)
        )),
        (Some(issued), None) => Some(format!(
            "发布于 {}",
            format_alert_time_point(issued, reference_date)
        )),
        (None, None) => None,
    }
}

fn format_alert_time_point(value: &str, reference_date: Option<NaiveDate>) -> String {
    let time = format_short_time(value);
    let Some(date) = local_date_from_timestamp(value) else {
        return time;
    };
    let date_label =
        match reference_date.map(|reference| date.signed_duration_since(reference).num_days()) {
            Some(0) => "今日".to_owned(),
            Some(1) => "明日".to_owned(),
            Some(2) => "后天".to_owned(),
            Some(-1) => "昨日".to_owned(),
            _ => date.format("%m-%d").to_string(),
        };
    format!("{date_label} {time}")
}

fn format_alert_summary(alert: &WeatherAlert) -> Option<String> {
    let description = collapse_inline_whitespace(alert.description.as_deref()?);
    if description.is_empty() {
        return None;
    }

    // 和风预警正文里常重复“发布单位 + 时间 + 标题：正文”，标题和时间已单独展示，
    // 这里优先去掉前置样板，只保留用户真正要看的风险描述。
    let mut summary = strip_alert_description_prefix(&description, alert);
    if let Some((prefix, rest)) = split_once_alert_separator(&summary)
        && prefix.contains("预警")
    {
        summary = rest.to_owned();
    }
    summary = summary
        .trim_start_matches(['：', ':', '，', ',', '。', '；', ';', '、', ' '])
        .to_owned();
    let summary = if summary.is_empty() {
        description
    } else {
        summary
    };
    Some(truncate_chars(&summary, WEATHER_ALERT_SUMMARY_MAX_CHARS))
}

fn strip_alert_description_prefix(description: &str, alert: &WeatherAlert) -> String {
    let mut value = description.trim().to_owned();
    if let Some(headline) = clean_string(alert.headline.clone())
        && let Some(rest) = value.strip_prefix(headline.as_str())
    {
        value = rest.trim_start().to_owned();
    }
    if let Some(sender_name) = alert
        .sender_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && let Some(rest) = value.strip_prefix(sender_name)
    {
        value = rest.trim_start().to_owned();
    }
    for prefix in ["继续发布", "升级发布", "发布了", "发布", "解除"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            value = rest.trim_start().to_owned();
            break;
        }
    }
    value
}

fn split_once_alert_separator(text: &str) -> Option<(&str, &str)> {
    text.split_once('：').or_else(|| text.split_once(':'))
}

fn collapse_inline_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn format_air_quality(air_quality: &AirQualitySummary) -> String {
    let mut parts = vec![format!(
        "**AQI {}{}**",
        air_quality.aqi_display,
        air_quality
            .category
            .as_deref()
            .map(|category| format!("（{category}）"))
            .or_else(|| {
                air_quality
                    .level
                    .as_deref()
                    .map(|level| format!("（{level}级）"))
            })
            .unwrap_or_default()
    )];
    if let Some(name) = air_quality
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.to_ascii_uppercase().contains("AQI"))
    {
        parts.push(name.to_owned());
    }
    if let Some(pollutant) = air_quality.primary_pollutant.as_deref() {
        parts.push(format!("首要污染物 {pollutant}"));
    }
    parts.join(" · ")
}

fn format_life_indices(indices: &[WeatherLifeIndex]) -> Option<String> {
    let first_date = indices.first()?.date.as_str();
    let parts = indices
        .iter()
        .filter(|index| index.date == first_date)
        .take(WEATHER_LIFE_INDEX_MAX_ITEMS)
        .filter_map(|index| {
            let category = index
                .category
                .as_deref()
                .or(index.level.as_deref())
                .or(index.text.as_deref())?;
            Some(format!(
                "{}：{}",
                trim_index_name(&index.name),
                truncate_chars(category, WEATHER_LIFE_INDEX_VALUE_MAX_CHARS)
            ))
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("｜"))
}

fn trim_index_name(name: &str) -> &str {
    name.trim().strip_suffix("指数").unwrap_or(name.trim())
}

pub(super) fn weather_reference_date(outcome: &WeatherOutcome) -> Option<NaiveDate> {
    local_date_from_timestamp(&outcome.current.time).or_else(|| {
        outcome
            .daily
            .first()
            .and_then(|day| local_date_from_timestamp(&day.date))
    })
}

fn format_daily_weather_label(day: &crate::runtime::weather::DailyWeather) -> String {
    match (day.weather_day.as_deref(), day.weather_night.as_deref()) {
        (Some(day_text), Some(night_text)) if day_text != night_text => {
            format!("{day_text}转{night_text}")
        }
        (Some(day_text), _) => day_text.to_owned(),
        _ => weather_code_label(day.weather_code).to_owned(),
    }
}

fn format_daily_summary(day: &crate::runtime::weather::DailyWeather) -> String {
    let mut parts = vec![
        format_daily_weather_label(day),
        format!(
            "{}～{}°C",
            format_number(day.temperature_min_c),
            format_number(day.temperature_max_c)
        ),
    ];
    if let Some(wind) = format_wind(
        day.wind_direction_day.as_deref(),
        day.wind_scale_day.as_deref(),
    ) {
        parts.push(wind);
    }
    parts.join("，")
}

pub(super) fn format_forecast_day_label(value: &str, reference_date: Option<NaiveDate>) -> String {
    let Some(date) = local_date_from_timestamp(value) else {
        return value.trim().to_owned();
    };
    let date_label =
        match reference_date.map(|reference| date.signed_duration_since(reference).num_days()) {
            Some(0) => "今天".to_owned(),
            Some(1) => "明天".to_owned(),
            Some(2) => "后天".to_owned(),
            _ => date.format("%m-%d").to_string(),
        };
    format!("{date_label} {}", weekday_label(date.weekday()))
}

fn weekday_label(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "周一",
        Weekday::Tue => "周二",
        Weekday::Wed => "周三",
        Weekday::Thu => "周四",
        Weekday::Fri => "周五",
        Weekday::Sat => "周六",
        Weekday::Sun => "周日",
    }
}

fn format_wind(direction: Option<&str>, scale: Option<&str>) -> Option<String> {
    match (
        direction.map(str::trim).filter(|value| !value.is_empty()),
        scale.map(str::trim).filter(|value| !value.is_empty()),
    ) {
        (Some(direction), Some(scale)) => Some(format!("{direction} {scale}级")),
        (Some(direction), None) => Some(direction.to_owned()),
        (None, Some(scale)) => Some(format!("{scale}级风")),
        (None, None) => None,
    }
}

fn format_location(
    name: &str,
    admin2: Option<&str>,
    admin1: Option<&str>,
    country: Option<&str>,
) -> String {
    let mut parts = vec![name.trim().to_owned()];
    if let Some(admin2) = admin2
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != name)
    {
        parts.push(admin2.to_owned());
    }
    if let Some(admin1) = admin1.map(str::trim).filter(|value| {
        !value.is_empty() && *value != name && !parts.iter().any(|part| part == value)
    }) {
        parts.push(admin1.to_owned());
    }
    if let Some(country) = country.map(str::trim).filter(|value| !value.is_empty()) {
        parts.push(country.to_owned());
    }
    parts.join("，")
}

fn format_number(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn format_short_time(value: &str) -> String {
    let display = format_local_time_for_display(value);
    display
        .split_once(' ')
        .map(|(_, time)| time.get(..5).unwrap_or(time).to_owned())
        .unwrap_or(display)
}

fn append_weather_supplement_diagnostics<T>(
    diagnostics: &mut Value,
    name: &str,
    supplement: &WeatherSupplement<T>,
    count: usize,
) {
    let Some(map) = diagnostics.as_object_mut() else {
        return;
    };
    map.insert(format!("{name}_status"), json!(supplement.status.as_str()));
    map.insert(format!("{name}_count"), json!(count));
    if let Some(zero_result) = supplement.zero_result {
        map.insert(format!("{name}_zero_result"), json!(zero_result));
    }
    if let Some(error_code) = supplement.error_code.as_deref() {
        map.insert(format!("{name}_error_code"), json!(error_code));
    }
    if let Some(error_stage) = supplement.error_stage.as_deref() {
        map.insert(format!("{name}_error_stage"), json!(error_stage));
    }
}

fn format_weather_error_reply(err: &LlmError) -> String {
    match err.code.as_str() {
        "not_found" => WEATHER_NOT_FOUND_REPLY.to_owned(),
        "timeout" => WEATHER_TIMEOUT_REPLY.to_owned(),
        _ => WEATHER_UPSTREAM_ERROR_REPLY.to_owned(),
    }
}

/// 将和风天气的天气代码映射为中文天气描述标签。
pub(super) fn weather_code_label(code: u16) -> &'static str {
    match code {
        100 | 150 => "晴",
        101 | 102 | 151 | 152 => "多云",
        103 | 153 => "晴间多云",
        104 | 154 => "阴",
        300 | 301 | 350 | 351 => "阵雨",
        302 | 303 => "雷阵雨",
        304 => "雷阵雨伴冰雹",
        305 | 309 | 399 => "小雨",
        306 => "中雨",
        307 | 308 | 310 | 311 | 312 => "大雨",
        313 => "冻雨",
        314 => "小到中雨",
        315 => "中到大雨",
        316 => "大到暴雨",
        317 => "暴雨到大暴雨",
        318 => "大暴雨到特大暴雨",
        400 | 401 | 408 => "小雪",
        402 | 409 => "中雪",
        403 | 410 => "大雪",
        404..=406 => "雨夹雪",
        407 => "阵雪",
        499 => "雪",
        500 | 501 | 509 | 510 | 514 | 515 => "雾",
        502 | 511 | 512 | 513 => "霾",
        503 | 504 | 507 | 508 => "沙尘",
        900 => "热",
        901 => "冷",
        _ => "未知天气",
    }
}

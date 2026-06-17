//! 天气查询指令的处理流程。
//! 负责解析 `/天气城市名` 和 `/城市名天气` 两种格式的指令，
//! 调用天气执行器获取实时天气、预报和可选增强摘要，并格式化为回复文本。
//! 同时处理找不到城市、超时、上游异常等错误场景。

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
    util::time_context::{
        format_local_date_with_weekday_for_display, format_local_time_for_display,
    },
};

use super::{
    RespondResponse, RustRespondService,
    common::{command_response, session_error, truncate_chars},
};

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
                    reply,
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
            reply,
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
fn format_weather_reply(outcome: &WeatherOutcome) -> String {
    let location = format_location(
        &outcome.location.name,
        outcome.location.admin2.as_deref(),
        outcome.location.admin1.as_deref(),
        outcome.location.country.as_deref(),
    );
    let current = &outcome.current;
    let current_extra = format_current_extra(current);
    let mut lines = vec![
        format!("【天气】{location}"),
        format!(
            "当前（{}）：{}，{}°C{}{}",
            format_short_time(&current.time),
            weather_code_label(current.weather_code),
            format_number(current.temperature_c),
            current
                .apparent_temperature_c
                .map(|value| format!("，体感 {}°C", format_number(value)))
                .unwrap_or_default(),
            if current_extra.is_empty() {
                String::new()
            } else {
                format!("，{current_extra}")
            }
        ),
    ];

    append_alert_lines(&mut lines, &outcome.alerts);
    if let Some(air_quality) = outcome.air_quality.data.as_ref() {
        lines.push(format!("空气：{}", format_air_quality(air_quality)));
    }
    if let Some(indices) = outcome.life_indices.data.as_ref()
        && let Some(summary) = format_life_indices(indices)
    {
        lines.push(format!("生活指数：{summary}"));
    }

    lines.push(String::new());
    lines.push(format!("今天起 {} 天：", outcome.forecast_days));

    for day in outcome.daily.iter().take(outcome.forecast_days as usize) {
        lines.push(format!(
            "- {}：{}，{}-{}°C{}",
            format_local_date_with_weekday_for_display(&day.date),
            format_daily_weather_label(day),
            format_number(day.temperature_min_c),
            format_number(day.temperature_max_c),
            format_daily_extra(day)
        ));
    }

    lines.push(String::new());
    lines.push("来源：和风天气".to_owned());
    truncate_chars(&lines.join("\n"), 1200)
}

fn append_alert_lines(lines: &mut Vec<String>, alerts: &WeatherSupplement<Vec<WeatherAlert>>) {
    match alerts.status {
        WeatherSupplementStatus::Empty => lines.push("预警：无生效预警".to_owned()),
        WeatherSupplementStatus::Available => {
            if let Some(alerts) = alerts.data.as_ref() {
                for alert in alerts.iter().take(2) {
                    lines.push(format!("预警：{}", format_alert(alert)));
                }
            }
        }
        WeatherSupplementStatus::NotRequested | WeatherSupplementStatus::Failed => {}
    }
}

fn format_alert(alert: &WeatherAlert) -> String {
    let mut label = alert.headline.clone();
    let mut tags = Vec::new();
    if let Some(event_name) = alert.event_name.as_deref() {
        tags.push(event_name);
    }
    if let Some(color) = alert.color_code.as_deref() {
        tags.push(color);
    }
    if !tags.is_empty() {
        label.push_str(&format!("（{}）", tags.join("/")));
    }
    if let Some(description) = alert.description.as_deref() {
        label.push_str(&format!("：{}", truncate_chars(description, 72)));
    }
    truncate_chars(&label, 120)
}

fn format_air_quality(air_quality: &AirQualitySummary) -> String {
    let mut label = String::new();
    if let Some(name) = air_quality.name.as_deref() {
        label.push_str(name);
        label.push(' ');
    }
    label.push_str(&air_quality.aqi_display);
    if let Some(category) = air_quality.category.as_deref() {
        label.push_str(&format!("（{category}）"));
    } else if let Some(level) = air_quality.level.as_deref() {
        label.push_str(&format!("（{level}级）"));
    }
    if let Some(pollutant) = air_quality.primary_pollutant.as_deref() {
        label.push_str(&format!("，首要污染物 {pollutant}"));
    }
    label
}

fn format_life_indices(indices: &[WeatherLifeIndex]) -> Option<String> {
    let first_date = indices.first()?.date.as_str();
    let parts = indices
        .iter()
        .filter(|index| index.date == first_date)
        .take(4)
        .filter_map(|index| {
            let category = index
                .category
                .as_deref()
                .or(index.level.as_deref())
                .or(index.text.as_deref())?;
            Some(format!(
                "{} {}",
                trim_index_name(&index.name),
                truncate_chars(category, 18)
            ))
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("；"))
}

fn trim_index_name(name: &str) -> &str {
    name.trim().strip_suffix("指数").unwrap_or(name.trim())
}

fn format_current_extra(current: &crate::runtime::weather::CurrentWeather) -> String {
    let mut parts = Vec::new();
    if let Some(humidity) = current.humidity_percent {
        parts.push(format!("湿度 {humidity}%"));
    }
    if let Some(wind) = format_wind(
        current.wind_direction.as_deref(),
        current.wind_scale.as_deref(),
    ) {
        parts.push(wind);
    }
    parts.join("，")
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

fn format_daily_extra(day: &crate::runtime::weather::DailyWeather) -> String {
    let mut parts = Vec::new();
    if let Some(wind) = format_wind(
        day.wind_direction_day.as_deref(),
        day.wind_scale_day.as_deref(),
    ) {
        parts.push(wind);
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("，{}", parts.join("，"))
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
fn weather_code_label(code: u16) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::weather::{
        CurrentWeather, DailyWeather, WeatherLocation, WeatherSupplement,
    };

    /// 合并 5 个 parse_weather_command 测试为表驱动测试。
    /// 每个 case 名称对应原独立测试函数，便于失败定位。
    #[test]
    fn parse_weather_command_accepts_variants() {
        struct ExpectedCommand {
            action: &'static str,
            argument: &'static str,
            raw_command: &'static str,
        }

        struct Case {
            name: &'static str,
            input: &'static str,
            expected: Option<ExpectedCommand>,
        }

        let cases = [
            Case {
                name: "parse_weather_command_accepts_attached_city",
                input: "/天气杭州",
                expected: Some(ExpectedCommand {
                    action: "weather",
                    argument: "杭州",
                    raw_command: "天气",
                }),
            },
            Case {
                name: "parse_weather_command_accepts_spaced_city",
                input: "/天气 杭州",
                expected: Some(ExpectedCommand {
                    action: "weather",
                    argument: "杭州",
                    raw_command: "天气",
                }),
            },
            Case {
                name: "parse_weather_command_accepts_city_weather_suffix",
                input: "/杭州天气",
                expected: Some(ExpectedCommand {
                    action: "weather",
                    argument: "杭州",
                    raw_command: "天气",
                }),
            },
            Case {
                name: "parse_weather_command_ignores_plain_city_weather_suffix",
                input: "杭州天气",
                expected: None,
            },
            Case {
                name: "parse_weather_command_keeps_empty_city_for_usage_reply",
                input: "/天气",
                expected: Some(ExpectedCommand {
                    action: "weather",
                    argument: "",
                    raw_command: "天气",
                }),
            },
        ];

        for case in &cases {
            let result = parse_weather_command(case.input);
            match &case.expected {
                None => assert!(
                    result.is_none(),
                    "case '{}' failed: expected None, got {:?}",
                    case.name,
                    result
                ),
                Some(expected) => {
                    let command = result.unwrap_or_else(|| {
                        panic!("case '{}' failed: expected Some, got None", case.name)
                    });
                    assert_eq!(
                        command.action, expected.action,
                        "case '{}' failed: action mismatch",
                        case.name
                    );
                    assert_eq!(
                        command.argument, expected.argument,
                        "case '{}' failed: argument mismatch",
                        case.name
                    );
                    assert_eq!(
                        command.raw_command, expected.raw_command,
                        "case '{}' failed: raw_command mismatch",
                        case.name
                    );
                }
            }
        }
    }

    #[test]
    fn format_weather_reply_includes_current_and_three_forecast_days() {
        let reply = format_weather_reply(&WeatherOutcome {
            location: WeatherLocation {
                id: Some("101210101".to_owned()),
                name: "杭州".to_owned(),
                country: Some("中国".to_owned()),
                admin1: Some("浙江".to_owned()),
                admin2: Some("杭州".to_owned()),
                timezone: Some("Asia/Shanghai".to_owned()),
                latitude: 30.29,
                longitude: 120.16,
            },
            current: CurrentWeather {
                time: "2026-06-12T20:15".to_owned(),
                temperature_c: 27.7,
                apparent_temperature_c: Some(28.5),
                weather_code: 104,
                humidity_percent: Some(86),
                precipitation_mm: Some(1.2),
                pressure_hpa: Some(1006),
                wind_direction: Some("东北风".to_owned()),
                wind_scale: Some("3".to_owned()),
                wind_speed_kmh: Some(6.7),
            },
            daily: vec![
                daily("2026-06-12", 104),
                daily("2026-06-13", 306),
                daily("2026-06-14", 305),
            ],
            provider: "mock-weather".to_owned(),
            elapsed_ms: 7,
            forecast_days: 3,
            alerts: WeatherSupplement::available(vec![
                WeatherAlert {
                    headline: "杭州市气象台发布大风蓝色预警".to_owned(),
                    event_name: Some("大风".to_owned()),
                    severity: Some("minor".to_owned()),
                    color_code: Some("blue".to_owned()),
                    sender_name: Some("杭州市气象台".to_owned()),
                    issued_time: Some("2026-06-12T18:00+08:00".to_owned()),
                    expire_time: Some("2026-06-13T18:00+08:00".to_owned()),
                    description: Some(
                        "预计未来24小时阵风较大，请注意户外高空物品安全。".to_owned(),
                    ),
                },
                WeatherAlert {
                    headline: "杭州市气象台发布雷电黄色预警".to_owned(),
                    event_name: Some("雷电".to_owned()),
                    severity: Some("moderate".to_owned()),
                    color_code: Some("yellow".to_owned()),
                    sender_name: Some("杭州市气象台".to_owned()),
                    issued_time: Some("2026-06-12T19:00+08:00".to_owned()),
                    expire_time: Some("2026-06-13T06:00+08:00".to_owned()),
                    description: Some("局地可能出现雷电活动，短时风雨较明显。".to_owned()),
                },
                WeatherAlert {
                    headline: "第三条预警不应展示".to_owned(),
                    event_name: Some("测试".to_owned()),
                    severity: None,
                    color_code: None,
                    sender_name: None,
                    issued_time: None,
                    expire_time: None,
                    description: None,
                },
            ]),
            air_quality: WeatherSupplement::available(AirQualitySummary {
                code: Some("cn-mee".to_owned()),
                name: Some("AQI（CN）".to_owned()),
                aqi_display: "42".to_owned(),
                level: Some("1".to_owned()),
                category: Some("优".to_owned()),
                primary_pollutant: Some("PM2.5".to_owned()),
            }),
            life_indices: WeatherSupplement::available(vec![
                WeatherLifeIndex {
                    date: "2026-06-12".to_owned(),
                    type_id: "1".to_owned(),
                    name: "运动指数".to_owned(),
                    level: Some("2".to_owned()),
                    category: Some("较适宜".to_owned()),
                    text: Some("适合进行适量户外活动。".to_owned()),
                },
                WeatherLifeIndex {
                    date: "2026-06-12".to_owned(),
                    type_id: "3".to_owned(),
                    name: "穿衣指数".to_owned(),
                    level: Some("6".to_owned()),
                    category: Some("热".to_owned()),
                    text: Some("建议短袖。".to_owned()),
                },
                WeatherLifeIndex {
                    date: "2026-06-13".to_owned(),
                    type_id: "1".to_owned(),
                    name: "运动指数".to_owned(),
                    level: Some("3".to_owned()),
                    category: Some("较不宜".to_owned()),
                    text: Some("次日不在摘要中展示。".to_owned()),
                },
            ]),
        });

        assert!(reply.contains("【天气】杭州，浙江，中国"));
        assert!(reply.contains("当前（20:15）"));
        assert!(reply.contains("今天起 3 天"));
        assert!(reply.contains("06-12（五）"));
        assert!(reply.contains("06-13（六）"));
        assert!(reply.contains("06-14（日）"));
        assert!(reply.contains("湿度 86%"));
        assert!(!reply.contains("气压"));
        assert!(!reply.contains("降水 1.2 mm"));
        assert!(reply.contains("东北风 3级"));
        assert!(reply.contains("预警：杭州市气象台发布大风蓝色预警"));
        assert!(reply.contains("预警：杭州市气象台发布雷电黄色预警"));
        assert!(!reply.contains("第三条预警不应展示"));
        assert!(reply.contains("空气：AQI（CN） 42（优），首要污染物 PM2.5"));
        assert!(reply.contains("生活指数：运动 较适宜；穿衣 热"));
        assert!(!reply.contains("次日不在摘要中展示"));
        assert!(reply.contains("小雨转阴"));
        assert!(!reply.contains("雨量"));
        assert!(reply.contains("来源：和风天气"));
    }

    #[test]
    fn weather_code_label_maps_mixed_rain_snow_range() {
        // 和风天气 404/405/406 同属雨夹雪类，范围模式不能遗漏任一代码。
        for code in [404, 405, 406] {
            assert_eq!(weather_code_label(code), "雨夹雪", "{code}");
        }
    }

    #[test]
    fn append_alert_lines_reports_empty_alerts() {
        let mut lines = Vec::new();

        append_alert_lines(
            &mut lines,
            &WeatherSupplement::<Vec<WeatherAlert>>::empty(Some(true)),
        );

        assert_eq!(lines, vec!["预警：无生效预警"]);
    }

    fn daily(date: &str, weather_code: u16) -> DailyWeather {
        DailyWeather {
            date: date.to_owned(),
            weather_code,
            weather_day: Some("小雨".to_owned()),
            weather_night: Some("阴".to_owned()),
            temperature_max_c: 32.5,
            temperature_min_c: 21.0,
            precipitation_probability_max: Some(69),
            precipitation_mm: Some(2.4),
            humidity_percent: Some(91),
            wind_direction_day: Some("东风".to_owned()),
            wind_scale_day: Some("1-3".to_owned()),
        }
    }
}

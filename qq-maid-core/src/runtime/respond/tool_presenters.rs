//! 非 Todo 业务 Tool 的确定性展示适配器。
//!
//! 通用 Agent 编排层不理解具体业务字段；这里按工具名把已注册业务 Tool 的
//! 安全结构化输出转换为可信响应块，避免模型最终文案覆盖或丢弃真实工具结果。

use serde_json::Value;

use crate::{provider::ToolExecutionResult, util::time_context::format_local_time_for_display};

use super::{
    agent_outcome::{
        OutcomePresentation, ResponseBlock, ToolEffect, ToolExecutionOutcome, ToolOutcomeStatus,
    },
    common::{CommandBody, truncate_chars},
    weather_flow::{format_forecast_day_label, weather_code_label},
};

const WEATHER_TOOL_NAME: &str = "get_weather";
const WEATHER_FACT_MAX_CHARS: usize = 900;

pub(super) fn tool_outcome_from_weather_result(
    result: &ToolExecutionResult,
) -> Option<ToolExecutionOutcome> {
    if result.name != WEATHER_TOOL_NAME {
        return None;
    }

    let status = ToolOutcomeStatus::from_tool_result(result);
    let error_code = structured_error_code(&result.output);
    let block = match status {
        ToolOutcomeStatus::Succeeded => ResponseBlock::FactCard(weather_fact_card(&result.output)),
        ToolOutcomeStatus::Skipped => ResponseBlock::Warning(weather_skip_body(&result.output)),
        ToolOutcomeStatus::RequiresClarification => {
            ResponseBlock::Clarification(CommandBody::plain("请说明要查询哪个城市的天气。"))
        }
        ToolOutcomeStatus::PendingConfirmation | ToolOutcomeStatus::Failed => {
            ResponseBlock::Error(weather_error_body(&result.output))
        }
    };

    Some(ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "weather".to_owned(),
        status,
        effect: ToolEffect::ReadOnly,
        presentation: OutcomePresentation::Trusted,
        blocks: vec![block],
        error_code,
        command: Some("weather".to_owned()),
    })
}

fn weather_fact_card(output: &Value) -> CommandBody {
    let location = output.get("location").unwrap_or(&Value::Null);
    let current = output.get("current").unwrap_or(&Value::Null);
    let daily = output
        .get("daily")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let name = string_field(location, "name").unwrap_or_else(|| "当前城市".to_owned());
    let full_location = full_location_label(location, &name);
    let weather = current
        .get("weather_code")
        .and_then(Value::as_u64)
        .and_then(|code| u16::try_from(code).ok())
        .map(weather_code_label)
        .unwrap_or("未知");
    let temp = number_field(current, "temperature_c")
        .map(format_number)
        .unwrap_or_else(|| "--".to_owned());
    let time = string_field(current, "time")
        .map(|value| short_time(&value))
        .unwrap_or_else(|| "--:--".to_owned());

    let mut text_lines = vec![format!("🌦 {name}天气")];
    let mut markdown_lines = vec![format!("# 🌦 {name}天气")];
    if let Some(detail) = location_detail(&name, &full_location) {
        text_lines.push(detail.clone());
        markdown_lines.push(format!("**{detail}**"));
    }
    text_lines.push(String::new());
    markdown_lines.push(String::new());
    text_lines.push(format!("当前 {time}｜{weather}｜{temp}°C"));
    markdown_lines.push(format!("**当前 {time}｜{weather}｜{temp}°C**  "));
    if let Some(details) = current_details(current) {
        text_lines.push(details.clone());
        markdown_lines.push(format!("{details}  "));
    }
    if let Some(air) = air_quality_summary(output) {
        text_lines.push(air.clone());
        markdown_lines.push(air);
    }

    let forecast = daily
        .iter()
        .take(3)
        .filter_map(format_daily_summary)
        .collect::<Vec<_>>();
    if !forecast.is_empty() {
        text_lines.push(String::new());
        markdown_lines.push(String::new());
        text_lines.push(format!("未来 {} 天", forecast.len()));
        markdown_lines.push(format!("## 未来 {} 天", forecast.len()));
        for line in forecast {
            text_lines.push(format!("- {line}"));
            markdown_lines.push(format!("- **{}**", line));
        }
    }

    CommandBody::dual(
        truncate_chars(&text_lines.join("\n"), WEATHER_FACT_MAX_CHARS),
        truncate_chars(&markdown_lines.join("\n"), WEATHER_FACT_MAX_CHARS),
    )
}

fn format_daily_summary(day: &Value) -> Option<String> {
    let date = string_field(day, "date")?;
    let weather = string_field(day, "weather_day")
        .or_else(|| string_field(day, "weather_night"))
        .or_else(|| {
            day.get("weather_code")
                .and_then(Value::as_u64)
                .and_then(|code| u16::try_from(code).ok())
                .map(weather_code_label)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "未知".to_owned());
    let min = number_field(day, "temperature_min_c").map(format_number);
    let max = number_field(day, "temperature_max_c").map(format_number);
    let temp = match (min, max) {
        (Some(min), Some(max)) => format!("{min}～{max}°C"),
        (None, Some(max)) => format!("最高 {max}°C"),
        (Some(min), None) => format!("最低 {min}°C"),
        (None, None) => "温度未知".to_owned(),
    };
    let mut parts = vec![format_forecast_day_label(&date, None), weather, temp];
    if let Some(probability) = day
        .get("precipitation_probability_max")
        .and_then(Value::as_u64)
    {
        parts.push(format!("降水概率 {probability}%"));
    }
    Some(parts.join("，"))
}

fn current_details(current: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(apparent) = number_field(current, "apparent_temperature_c") {
        parts.push(format!("体感 {}°C", format_number(apparent)));
    }
    if let Some(humidity) = current.get("humidity_percent").and_then(Value::as_u64) {
        parts.push(format!("湿度 {humidity}%"));
    }
    if let Some(precipitation) = number_field(current, "precipitation_mm")
        && precipitation > 0.0
    {
        parts.push(format!("降水 {}mm", format_number(precipitation)));
    }
    if let Some(wind) = format_wind(
        string_field(current, "wind_direction").as_deref(),
        string_field(current, "wind_scale").as_deref(),
    ) {
        parts.push(wind);
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn air_quality_summary(output: &Value) -> Option<String> {
    let air = output.get("air_quality")?.get("summary")?;
    let aqi = string_field(air, "aqi_display")?;
    let category = string_field(air, "category");
    let mut text = format!("空气质量：AQI {aqi}");
    if let Some(category) = category {
        text.push_str(&format!("（{category}）"));
    }
    if let Some(primary) = string_field(air, "primary_pollutant") {
        text.push_str(&format!(" · 首要污染物 {primary}"));
    }
    Some(text)
}

fn weather_error_body(output: &Value) -> CommandBody {
    let code = structured_error_code(output);
    let text = match code.as_deref() {
        Some("not_found") => "【天气】\n\n没找到这个城市。可以换成更完整的城市名再试。",
        Some("timeout") => "【天气】\n\n天气服务超时了，请稍后再试。",
        Some("bad_tool_arguments") => "【天气】\n\n天气查询参数不完整，请说明要查询的城市。",
        _ => "【天气】\n\n天气服务暂时不可用，请稍后再试。",
    };
    CommandBody::plain(text)
}

fn weather_skip_body(output: &Value) -> CommandBody {
    let text = match string_field(output, "reason").as_deref() {
        Some("dependency_previous_call_failed") => {
            "天气查询因前序工具失败已跳过；根因以上方失败信息为准。".to_owned()
        }
        Some(reason) => format!("天气查询已跳过：{reason}。"),
        None => "天气查询已跳过。".to_owned(),
    };
    CommandBody::plain(text)
}

fn full_location_label(location: &Value, name: &str) -> String {
    let mut parts = vec![name.trim().to_owned()];
    for key in ["admin2", "admin1", "country"] {
        if let Some(value) = string_field(location, key)
            && !parts.iter().any(|part| part == &value)
        {
            parts.push(value);
        }
    }
    parts.join("，")
}

fn location_detail(name: &str, full_location: &str) -> Option<String> {
    let name = name.trim();
    let full_location = full_location.trim();
    if full_location.is_empty() || full_location == name {
        None
    } else {
        Some(full_location.to_owned())
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

fn short_time(value: &str) -> String {
    let display = format_local_time_for_display(value);
    display
        .split_once(' ')
        .map(|(_, time)| time.get(..5).unwrap_or(time).to_owned())
        .unwrap_or(display)
}

fn number_field(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn format_number(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn structured_error_code(output: &Value) -> Option<String> {
    output
        .get("error_code")
        .and_then(Value::as_str)
        .or_else(|| {
            output
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

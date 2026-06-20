use reqwest::{StatusCode, Url};
use serde_json::Value;

use crate::error::LlmError;

use super::super::types::WeatherSupplement;

/// 构造需要经纬度路径参数的和风天气 v1 API 路径。
pub(super) fn qweather_coordinate_path(prefix: &str, latitude: f64, longitude: f64) -> String {
    format!(
        "{}/{:.2}/{:.2}",
        prefix.trim_end_matches('/'),
        latitude,
        longitude
    )
}

/// 构造和风天气 API URL，自动处理 scheme 和路径拼接。
pub(super) fn qweather_url(host: &str, path: &str) -> Result<Url, LlmError> {
    let host = normalize_qweather_host(host);
    let path = path.trim_start_matches('/');
    Url::parse(&format!("{host}/{path}"))
        .map_err(|err| LlmError::config(format!("invalid QWeather API URL: {err}")))
}

/// 标准化 API 主机地址：去除末尾斜杠，缺少 scheme 时自动添加 https。
fn normalize_qweather_host(host: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    if host.starts_with("http://") || host.starts_with("https://") {
        return host.to_owned();
    }
    format!("https://{host}")
}

/// 解析 f64 字段，非数字时返回 provider 错误。
pub(super) fn parse_f64_field(value: &str, field: &str) -> Result<f64, LlmError> {
    value
        .trim()
        .parse::<f64>()
        .map_err(|_| LlmError::provider(format!("{field} is not a number"), "provider"))
}

/// 解析可选的 f64 字段。
pub(super) fn parse_optional_f64_field(
    value: Option<&str>,
    field: &str,
) -> Result<Option<f64>, LlmError> {
    value.map(|value| parse_f64_field(value, field)).transpose()
}

/// 解析 u16 字段（天气代码）。
pub(super) fn parse_u16_field(value: &str, field: &str) -> Result<u16, LlmError> {
    value
        .trim()
        .parse::<u16>()
        .map_err(|_| LlmError::provider(format!("{field} is not a weather code"), "provider"))
}

/// 解析可选的 u8 字段（百分比值）。
pub(super) fn parse_optional_u8_field(
    value: Option<&str>,
    field: &str,
) -> Result<Option<u8>, LlmError> {
    value
        .map(|value| {
            value
                .trim()
                .parse::<u8>()
                .map_err(|_| LlmError::provider(format!("{field} is not a percent"), "provider"))
        })
        .transpose()
}

/// 解析可选的 u16 字段（整数值）。
pub(super) fn parse_optional_u16_field(
    value: Option<&str>,
    field: &str,
) -> Result<Option<u16>, LlmError> {
    value
        .map(|value| {
            value
                .trim()
                .parse::<u16>()
                .map_err(|_| LlmError::provider(format!("{field} is not an integer"), "provider"))
        })
        .transpose()
}

/// 过滤空字符串的 Option，将空字符串视为 None。
pub(super) fn non_empty_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// 将 JSON 数值或字符串转换为展示文本。
pub(super) fn value_to_display_string(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => Some(number.to_string()),
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        }
        _ => None,
    }
}

/// 将增强接口结果收敛为可诊断状态；失败只写日志，不影响基础天气结果。
pub(super) fn weather_supplement_or_failed<T>(
    supplement: &'static str,
    result: Result<WeatherSupplement<T>, LlmError>,
) -> WeatherSupplement<T> {
    match result {
        Ok(result) => result,
        Err(err) => {
            tracing::warn!(
                weather_supplement = supplement,
                error_code = %err.code,
                error_stage = %err.stage,
                "optional weather supplement failed"
            );
            WeatherSupplement::failed(&err)
        }
    }
}

/// 将 reqwest 错误映射为 LlmError，超时错误特殊处理。
pub(super) fn map_weather_request_error(err: reqwest::Error) -> LlmError {
    if err.is_timeout() {
        return LlmError::timeout("weather");
    }
    LlmError::http(format!("QWeather request failed: {err}"))
}

/// 检查 HTTP 响应状态码，非成功时根据状态码返回适当的错误。
pub(super) async fn ensure_http_success(
    response: reqwest::Response,
    stage: &str,
) -> Result<reqwest::Response, LlmError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    Err(match status {
        StatusCode::NOT_FOUND => LlmError::new(
            "not_found",
            format!("{stage} returned HTTP {}", status.as_u16()),
            "weather",
        ),
        _ => LlmError::http(format!("{stage} returned HTTP {}", status.as_u16())),
    })
}

/// 将和风天气 API 返回的非成功状态码转换为 LlmError。
pub(super) fn qweather_code_error(stage: &str, code: &str) -> LlmError {
    match code {
        "204" | "404" => LlmError::new(
            "not_found",
            format!("{stage} returned QWeather code {code}"),
            "weather",
        ),
        _ => LlmError::http(format!("{stage} returned QWeather code {code}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{qweather_code_error, qweather_url};

    #[test]
    fn qweather_non_success_code_maps_to_upstream_error() {
        let err = qweather_code_error("QWeather weather now", "401");

        assert_eq!(err.code, "http_error");
        assert_eq!(err.stage, "http");
    }

    #[test]
    fn qweather_url_adds_https_scheme_for_console_host() {
        let url = qweather_url("example.qweatherapi.com", "/geo/v2/city/lookup").unwrap();

        assert_eq!(
            url.as_str(),
            "https://example.qweatherapi.com/geo/v2/city/lookup"
        );
    }
}

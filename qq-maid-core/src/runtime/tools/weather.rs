//! 天气 Tool。
//!
//! 该 Tool 复用现有 `WeatherExecutor`，只做模型工具参数校验和结果脱敏整理。
//! slash `/天气` 命令仍保留在 respond/weather_flow.rs，不通过 Tool Loop。

use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::weather::{
        DEFAULT_FORECAST_DAYS, DailyWeather, DynWeatherExecutor, WeatherOutcome, WeatherRequest,
    },
};

const WEATHER_TOOL_NAME: &str = "get_weather";
const WEATHER_TOOL_CITY_MAX_CHARS: usize = 60;
const WEATHER_TOOL_MAX_FORECAST_DAYS: u8 = 7;

/// 模型可调用的天气查询 Tool。
#[derive(Clone)]
pub struct WeatherTool {
    executor: DynWeatherExecutor,
}

impl WeatherTool {
    pub fn new(executor: DynWeatherExecutor) -> Self {
        Self { executor }
    }
}

#[async_trait]
impl Tool for WeatherTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: WEATHER_TOOL_NAME.to_owned(),
            description: "查询指定城市的实时天气、未来预报、预警、空气质量和生活指数。用于回答是否下雨、是否带伞、温度、风力等天气问题。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "city": {
                        "type": "string",
                        "description": "城市或区县名称，例如杭州、浙江杭州、Hangzhou"
                    },
                    "forecast_days": {
                        "type": ["integer", "null"],
                        "description": "预报天数，1 到 7；不确定时传 null，系统默认 3 天",
                        "minimum": 1,
                        "maximum": WEATHER_TOOL_MAX_FORECAST_DAYS
                    }
                },
                "required": ["city", "forecast_days"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let city = arguments
            .get("city")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                LlmError::new(
                    "bad_tool_arguments",
                    "get_weather requires non-empty city",
                    "tool",
                )
            })?;
        if city.chars().count() > WEATHER_TOOL_CITY_MAX_CHARS {
            return Err(LlmError::new(
                "bad_tool_arguments",
                "city is too long",
                "tool",
            ));
        }
        let forecast_days = arguments
            .get("forecast_days")
            .and_then(Value::as_u64)
            .map(|value| value.clamp(1, WEATHER_TOOL_MAX_FORECAST_DAYS as u64) as u8)
            .unwrap_or(DEFAULT_FORECAST_DAYS);

        let outcome = self
            .executor
            .weather(WeatherRequest {
                city: city.to_owned(),
                forecast_days,
            })
            .await?;
        Ok(ToolOutput::json(weather_tool_output(&outcome)))
    }
}

fn weather_tool_output(outcome: &WeatherOutcome) -> Value {
    json!({
        "provider": outcome.provider,
        "location": {
            "name": outcome.location.name,
            "country": outcome.location.country,
            "admin1": outcome.location.admin1,
            "admin2": outcome.location.admin2,
            "timezone": outcome.location.timezone,
        },
        "current": {
            "time": outcome.current.time,
            "temperature_c": outcome.current.temperature_c,
            "apparent_temperature_c": outcome.current.apparent_temperature_c,
            "weather_code": outcome.current.weather_code,
            "humidity_percent": outcome.current.humidity_percent,
            "precipitation_mm": outcome.current.precipitation_mm,
            "wind_direction": outcome.current.wind_direction,
            "wind_scale": outcome.current.wind_scale,
            "wind_speed_kmh": outcome.current.wind_speed_kmh,
        },
        "daily": outcome
            .daily
            .iter()
            .take(outcome.forecast_days as usize)
            .map(daily_weather_json)
            .collect::<Vec<_>>(),
        // 附加数据只返回摘要状态和可展示内容，不暴露上游 URL、请求参数或内部错误详情。
        "alerts": {
            "status": outcome.alerts.status.as_str(),
            "items": outcome.alerts.data.as_ref().map(|items| {
                items.iter().take(3).map(|alert| json!({
                    "headline": alert.headline,
                    "event_name": alert.event_name,
                    "severity": alert.severity,
                    "color_code": alert.color_code,
                    "issued_time": alert.issued_time,
                    "expire_time": alert.expire_time,
                    "description": alert.description,
                })).collect::<Vec<_>>()
            }).unwrap_or_default(),
        },
        "air_quality": {
            "status": outcome.air_quality.status.as_str(),
            "summary": outcome.air_quality.data.as_ref().map(|air| json!({
                "aqi_display": air.aqi_display,
                "category": air.category,
                "primary_pollutant": air.primary_pollutant,
            })),
        },
        "life_indices": {
            "status": outcome.life_indices.status.as_str(),
            "items": outcome.life_indices.data.as_ref().map(|items| {
                items.iter().take(6).map(|item| json!({
                    "date": item.date,
                    "name": item.name,
                    "category": item.category,
                    "text": item.text,
                })).collect::<Vec<_>>()
            }).unwrap_or_default(),
        },
    })
}

fn daily_weather_json(day: &DailyWeather) -> Value {
    json!({
        "date": day.date,
        "weather_code": day.weather_code,
        "weather_day": day.weather_day,
        "weather_night": day.weather_night,
        "temperature_max_c": day.temperature_max_c,
        "temperature_min_c": day.temperature_min_c,
        "precipitation_probability_max": day.precipitation_probability_max,
        "precipitation_mm": day.precipitation_mm,
        "humidity_percent": day.humidity_percent,
        "wind_direction_day": day.wind_direction_day,
        "wind_scale_day": day.wind_scale_day,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use crate::runtime::weather::{
        AirQualitySummary, CurrentWeather, DailyWeather, WeatherAlert, WeatherExecutor,
        WeatherLifeIndex, WeatherLocation, WeatherSupplement,
    };

    use super::*;

    fn test_context() -> ToolContext {
        ToolContext {
            task_id: "task-1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
        }
    }

    #[derive(Clone, Default)]
    struct MockWeatherExecutor {
        requests: Arc<Mutex<Vec<WeatherRequest>>>,
    }

    #[async_trait]
    impl WeatherExecutor for MockWeatherExecutor {
        async fn weather(&self, req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
            self.requests.lock().unwrap().push(req.clone());
            Ok(WeatherOutcome {
                location: WeatherLocation {
                    id: Some("101210101".to_owned()),
                    name: req.city,
                    country: Some("中国".to_owned()),
                    admin1: Some("浙江".to_owned()),
                    admin2: Some("杭州".to_owned()),
                    timezone: Some("Asia/Shanghai".to_owned()),
                    latitude: 30.29365,
                    longitude: 120.16142,
                },
                current: CurrentWeather {
                    time: "2026-06-12T20:15".to_owned(),
                    temperature_c: 27.7,
                    apparent_temperature_c: Some(28.5),
                    weather_code: 61,
                    humidity_percent: Some(86),
                    precipitation_mm: Some(1.2),
                    pressure_hpa: Some(1006),
                    wind_direction: Some("东北风".to_owned()),
                    wind_scale: Some("3".to_owned()),
                    wind_speed_kmh: Some(6.7),
                },
                daily: vec![DailyWeather {
                    date: "2026-06-12".to_owned(),
                    weather_code: 61,
                    weather_day: Some("小雨".to_owned()),
                    weather_night: Some("小雨".to_owned()),
                    temperature_max_c: 28.0,
                    temperature_min_c: 22.0,
                    precipitation_probability_max: Some(80),
                    precipitation_mm: Some(3.0),
                    humidity_percent: Some(90),
                    wind_direction_day: Some("东北风".to_owned()),
                    wind_scale_day: Some("3".to_owned()),
                }],
                provider: "mock-weather".to_owned(),
                elapsed_ms: 7,
                forecast_days: req.forecast_days,
                alerts: WeatherSupplement::<Vec<WeatherAlert>>::empty(Some(true)),
                air_quality: WeatherSupplement::available(AirQualitySummary {
                    code: Some("cn-mee".to_owned()),
                    name: Some("AQI".to_owned()),
                    aqi_display: "42".to_owned(),
                    level: Some("1".to_owned()),
                    category: Some("优".to_owned()),
                    primary_pollutant: Some("PM2.5".to_owned()),
                }),
                life_indices: WeatherSupplement::<Vec<WeatherLifeIndex>>::default(),
            })
        }

        fn provider_name(&self) -> &'static str {
            "mock-weather"
        }
    }

    #[tokio::test]
    async fn weather_tool_reuses_weather_executor() {
        let executor = MockWeatherExecutor::default();
        let requests = executor.requests.clone();
        let tool = WeatherTool::new(Arc::new(executor));

        let output = tool
            .execute(test_context(), json!({"city": "杭州", "forecast_days": 3}))
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].city, "杭州");
        assert_eq!(requests[0].forecast_days, 3);
        assert_eq!(output.value["provider"], "mock-weather");
        assert_eq!(output.value["current"]["weather_code"], 61);
        assert_eq!(output.value["daily"][0]["weather_day"], "小雨");
    }

    #[tokio::test]
    async fn weather_tool_rejects_empty_city_without_calling_executor() {
        let executor = MockWeatherExecutor::default();
        let requests = executor.requests.clone();
        let tool = WeatherTool::new(Arc::new(executor));

        let err = tool
            .execute(test_context(), json!({"city": " ", "forecast_days": null}))
            .await
            .unwrap_err();

        assert_eq!(err.code, "bad_tool_arguments");
        assert_eq!(requests.lock().unwrap().len(), 0);
    }
}

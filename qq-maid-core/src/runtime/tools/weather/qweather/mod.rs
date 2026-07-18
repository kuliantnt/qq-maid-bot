//! QWeather provider 实现。
//!
//! 这里保留和风天气的 HTTP client、主链路编排以及默认 host helper；
//! 地理查询、增强摘要和通用请求辅助继续拆到内部子模块，避免 `mod.rs`
//! 再次回到所有职责堆叠在一起的状态。

mod geo;
mod supplement;
mod util;

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::{config::AppConfig, error::LlmError, util::metrics::duration_ms};

use self::{
    geo::{QWeatherGeoLocation, QWeatherGeoResponse, lookup_city_query, select_location},
    supplement::{
        QWeatherAirQualityResponse, QWeatherAlertResponse, QWeatherIndicesResponse,
        air_quality_supplement, life_indices_supplement, weather_alert_supplement,
    },
    util::{
        ensure_http_success, map_weather_request_error, non_empty_string, parse_f64_field,
        parse_optional_f64_field, parse_optional_u8_field, parse_optional_u16_field,
        parse_u16_field, qweather_code_error, qweather_coordinate_path, qweather_url,
        weather_supplement_or_failed,
    },
};
use super::types::{
    AirQualitySummary, CurrentWeather, DailyWeather, DynWeatherExecutor, WeatherAlert,
    WeatherExecutor, WeatherLifeIndex, WeatherOutcome, WeatherRequest, WeatherSupplement,
};

/// 和风天气 API 默认主机地址。
pub(crate) const DEFAULT_QWEATHER_API_HOST: &str = "https://api.qweather.com";
/// 和风天气地理 API 默认主机地址。
pub(crate) const DEFAULT_QWEATHER_GEO_HOST: &str = "https://geoapi.qweather.com";
/// 城市查询 API 路径。
const QWEATHER_GEO_CITY_LOOKUP_PATH: &str = "/geo/v2/city/lookup";
/// 实时天气 API 路径。
const QWEATHER_WEATHER_NOW_PATH: &str = "/v7/weather/now";
/// 3 天预报 API 路径。
const QWEATHER_WEATHER_3D_PATH: &str = "/v7/weather/3d";
/// 7 天预报 API 路径。
const QWEATHER_WEATHER_7D_PATH: &str = "/v7/weather/7d";
/// 实时天气预警 API 路径前缀。
const QWEATHER_ALERT_CURRENT_PATH_PREFIX: &str = "/weatheralert/v1/current";
/// 实时空气质量 API 路径前缀。
const QWEATHER_AIR_CURRENT_PATH_PREFIX: &str = "/airquality/v1/current";
/// 天气生活指数 3 天预报 API 路径。
const QWEATHER_INDICES_3D_PATH: &str = "/v7/indices/3d";
/// 和风天气 API 成功响应码。
const QWEATHER_SUCCESS_CODE: &str = "200";
/// 和风天气 API 请求成功但无数据响应码。
const QWEATHER_EMPTY_CODE: &str = "204";
/// 默认查询的常用生活指数：运动、洗车、穿衣、紫外线、感冒。
const QWEATHER_DEFAULT_INDICES_TYPES: &str = "1,2,3,5,9";
/// 和风天气通用空气质量指数代码。
const QWEATHER_QAQI_CODE: &str = "qaqi";
/// 和风天气 v1 增强接口使用 API Key 请求头认证，不沿用 v7 的 query key。
const QWEATHER_API_KEY_HEADER: &str = "X-QW-Api-Key";

/// 根据请求预报天数选择合适的和风天气每日预报接口路径。
///
/// 1～3 天走 `/v7/weather/3d`，4～7 天走 `/v7/weather/7d`。
/// 和风天气每日预报步位固定，不支持任意天数，需 4～7 天时拉取 7 天再在本地截断。
fn daily_forecast_path(forecast_days: u8) -> &'static str {
    if forecast_days <= 3 {
        QWEATHER_WEATHER_3D_PATH
    } else {
        QWEATHER_WEATHER_7D_PATH
    }
}

/// 根据配置构建和风天气执行器。
pub fn build_weather_executor(config: &AppConfig) -> Result<DynWeatherExecutor, LlmError> {
    if config.qweather_api_key.trim().is_empty() {
        return Ok(std::sync::Arc::new(DisabledWeatherExecutor));
    }
    Ok(std::sync::Arc::new(QWeatherExecutor::new(
        config.request_timeout_seconds,
        config.qweather_api_key.clone(),
        config.qweather_api_host.clone(),
        config.qweather_geo_host.clone(),
    )?))
}

/// 未配置 API Key 时使用的显式关闭后端，确保普通部署无需天气凭证也能启动。
struct DisabledWeatherExecutor;

#[async_trait]
impl WeatherExecutor for DisabledWeatherExecutor {
    async fn weather(&self, _req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
        Err(LlmError::config(
            "weather is disabled because QWEATHER_API_KEY is not configured",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "disabled"
    }

    fn is_available(&self) -> bool {
        false
    }
}

/// 和风天气（QWeather）API 执行器。
pub struct QWeatherExecutor {
    /// HTTP 客户端
    client: reqwest::Client,
    /// API 密钥
    api_key: String,
    /// API 主机地址
    api_host: String,
    /// 地理 API 主机地址
    geo_host: String,
}

impl QWeatherExecutor {
    /// 创建新的和风天气执行器。
    pub fn new(
        request_timeout_seconds: u64,
        api_key: String,
        api_host: String,
        geo_host: String,
    ) -> Result<Self, LlmError> {
        if api_key.trim().is_empty() {
            return Err(LlmError::config("QWEATHER_API_KEY must be configured"));
        }
        let client = reqwest::Client::builder()
            .user_agent(format!("qq-maid-core/{}", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(request_timeout_seconds))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build QWeather HTTP client: {err}"))
            })?;
        Ok(Self {
            client,
            api_key,
            api_host,
            geo_host,
        })
    }

    /// 查询城市的地理位置信息。
    async fn lookup_location(&self, city: &str) -> Result<QWeatherGeoLocation, LlmError> {
        let lookup_city = lookup_city_query(city);
        let mut url = qweather_url(&self.geo_host, QWEATHER_GEO_CITY_LOOKUP_PATH)?;
        url.query_pairs_mut()
            .append_pair("location", &lookup_city)
            .append_pair("range", "cn")
            .append_pair("number", "10")
            .append_pair("lang", "zh")
            .append_pair("key", &self.api_key);

        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(map_weather_request_error)?;
        let response = ensure_http_success(response, "QWeather GeoAPI city lookup").await?;
        let body: QWeatherGeoResponse = response.json().await.map_err(|err| {
            LlmError::provider(format!("invalid QWeather GeoAPI JSON: {err}"), "json")
        })?;

        if body.code != QWEATHER_SUCCESS_CODE {
            return Err(qweather_code_error(
                "QWeather GeoAPI city lookup",
                &body.code,
            ));
        }

        select_location(city, body.location)
    }

    /// 获取指定位置的实时天气。
    async fn fetch_current(&self, location_id: &str) -> Result<CurrentWeather, LlmError> {
        let mut url = qweather_url(&self.api_host, QWEATHER_WEATHER_NOW_PATH)?;
        url.query_pairs_mut()
            .append_pair("location", location_id)
            .append_pair("lang", "zh")
            .append_pair("unit", "m")
            .append_pair("key", &self.api_key);

        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(map_weather_request_error)?;
        let response = ensure_http_success(response, "QWeather weather now").await?;
        let body: QWeatherNowResponse = response.json().await.map_err(|err| {
            LlmError::provider(format!("invalid QWeather weather now JSON: {err}"), "json")
        })?;
        if body.code != QWEATHER_SUCCESS_CODE {
            return Err(qweather_code_error("QWeather weather now", &body.code));
        }

        Ok(CurrentWeather {
            time: body.now.obs_time,
            temperature_c: parse_f64_field(&body.now.temp, "QWeather now temp")?,
            apparent_temperature_c: parse_optional_f64_field(
                body.now.feels_like.as_deref(),
                "QWeather now feelsLike",
            )?,
            weather_code: parse_u16_field(&body.now.icon, "QWeather now icon")?,
            humidity_percent: parse_optional_u8_field(
                body.now.humidity.as_deref(),
                "QWeather now humidity",
            )?,
            precipitation_mm: parse_optional_f64_field(
                body.now.precip.as_deref(),
                "QWeather now precip",
            )?,
            pressure_hpa: parse_optional_u16_field(
                body.now.pressure.as_deref(),
                "QWeather now pressure",
            )?,
            wind_direction: non_empty_string(body.now.wind_dir),
            wind_scale: non_empty_string(body.now.wind_scale),
            wind_speed_kmh: parse_optional_f64_field(
                body.now.wind_speed.as_deref(),
                "QWeather now windSpeed",
            )?,
        })
    }

    /// 获取指定位置的未来每日天气预报。
    ///
    /// 和风天气每日预报接口只按固定档位提供：1～3 天走 `/v7/weather/3d`，
    /// 4～7 天走 `/v7/weather/7d`。上游不支持任意天数，因此需要 4～7 天时
    /// 拉取 7 天再由本地截取到目标天数。返回条数始终不超过 `forecast_days`，
    /// 实际条数由调用方再次修正 `forecast_days`，避免声称返回了上游没有的数据。
    async fn fetch_daily(
        &self,
        location_id: &str,
        forecast_days: u8,
    ) -> Result<Vec<DailyWeather>, LlmError> {
        let path = daily_forecast_path(forecast_days);
        let mut url = qweather_url(&self.api_host, path)?;
        url.query_pairs_mut()
            .append_pair("location", location_id)
            .append_pair("lang", "zh")
            .append_pair("unit", "m")
            .append_pair("key", &self.api_key);

        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(map_weather_request_error)?;
        let response = ensure_http_success(response, "QWeather weather daily").await?;
        let body: QWeatherDailyResponse = response.json().await.map_err(|err| {
            LlmError::provider(
                format!("invalid QWeather weather daily JSON: {err}"),
                "json",
            )
        })?;
        if body.code != QWEATHER_SUCCESS_CODE {
            return Err(qweather_code_error("QWeather weather daily", &body.code));
        }

        let mut daily = body
            .daily
            .into_iter()
            .map(|day| {
                Ok(DailyWeather {
                    date: day.fx_date,
                    weather_code: parse_u16_field(&day.icon_day, "QWeather daily iconDay")?,
                    weather_day: non_empty_string(day.text_day),
                    weather_night: non_empty_string(day.text_night),
                    temperature_max_c: parse_f64_field(&day.temp_max, "QWeather daily tempMax")?,
                    temperature_min_c: parse_f64_field(&day.temp_min, "QWeather daily tempMin")?,
                    precipitation_probability_max: parse_optional_u8_field(
                        day.pop.as_deref(),
                        "QWeather daily pop",
                    )?,
                    precipitation_mm: parse_optional_f64_field(
                        day.precip.as_deref(),
                        "QWeather daily precip",
                    )?,
                    humidity_percent: parse_optional_u8_field(
                        day.humidity.as_deref(),
                        "QWeather daily humidity",
                    )?,
                    wind_direction_day: non_empty_string(day.wind_dir_day),
                    wind_scale_day: non_empty_string(day.wind_scale_day),
                })
            })
            .collect::<Result<Vec<_>, LlmError>>()?;

        if daily.is_empty() {
            return Err(LlmError::provider(
                "QWeather weather daily missing daily weather",
                "provider",
            ));
        }
        // 上游每日预报只提供 3d/7d 档位，且个别地区可能不足 7 天。
        // 截断到请求天数，调用方据此修正 forecast_days，避免天气回复里
        // 声称返回了上游实际没有的天数。
        daily.truncate(forecast_days as usize);
        Ok(daily)
    }

    /// 获取实时天气预警。该能力是天气回复的增强信息，失败时由上层降级处理。
    async fn fetch_alerts(
        &self,
        latitude: f64,
        longitude: f64,
    ) -> Result<WeatherSupplement<Vec<WeatherAlert>>, LlmError> {
        let path =
            qweather_coordinate_path(QWEATHER_ALERT_CURRENT_PATH_PREFIX, latitude, longitude);
        let mut url = qweather_url(&self.api_host, &path)?;
        url.query_pairs_mut()
            .append_pair("localTime", "true")
            .append_pair("lang", "zh");

        let response = self
            .client
            .get(url)
            .header(QWEATHER_API_KEY_HEADER, &self.api_key)
            .send()
            .await
            .map_err(map_weather_request_error)?;
        let response = ensure_http_success(response, "QWeather weather alert current").await?;
        let body: QWeatherAlertResponse = response.json().await.map_err(|err| {
            LlmError::provider(
                format!("invalid QWeather weather alert current JSON: {err}"),
                "json",
            )
        })?;
        Ok(weather_alert_supplement(body))
    }

    /// 获取实时空气质量。优先展示当地标准，回退到 QAQI，再回退到第一个可用指数。
    async fn fetch_air_quality(
        &self,
        latitude: f64,
        longitude: f64,
    ) -> Result<WeatherSupplement<AirQualitySummary>, LlmError> {
        let path = qweather_coordinate_path(QWEATHER_AIR_CURRENT_PATH_PREFIX, latitude, longitude);
        let mut url = qweather_url(&self.api_host, &path)?;
        url.query_pairs_mut().append_pair("lang", "zh");

        let response = self
            .client
            .get(url)
            .header(QWEATHER_API_KEY_HEADER, &self.api_key)
            .send()
            .await
            .map_err(map_weather_request_error)?;
        let response = ensure_http_success(response, "QWeather air quality current").await?;
        let body: QWeatherAirQualityResponse = response.json().await.map_err(|err| {
            LlmError::provider(
                format!("invalid QWeather air quality current JSON: {err}"),
                "json",
            )
        })?;
        Ok(air_quality_supplement(body))
    }

    /// 获取常用生活指数。只取一组常用类型，避免在天气回复里堆叠过多长文本。
    async fn fetch_life_indices(
        &self,
        location_id: &str,
    ) -> Result<WeatherSupplement<Vec<WeatherLifeIndex>>, LlmError> {
        let mut url = qweather_url(&self.api_host, QWEATHER_INDICES_3D_PATH)?;
        url.query_pairs_mut()
            .append_pair("location", location_id)
            .append_pair("type", QWEATHER_DEFAULT_INDICES_TYPES)
            .append_pair("lang", "zh")
            .append_pair("key", &self.api_key);

        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(map_weather_request_error)?;
        let response = ensure_http_success(response, "QWeather weather indices 3d").await?;
        let body: QWeatherIndicesResponse = response.json().await.map_err(|err| {
            LlmError::provider(
                format!("invalid QWeather weather indices 3d JSON: {err}"),
                "json",
            )
        })?;
        life_indices_supplement(body)
    }
}

#[async_trait]
impl WeatherExecutor for QWeatherExecutor {
    async fn weather(&self, req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
        let city = req.city.trim();
        if city.is_empty() {
            return Err(LlmError::new(
                "bad_request",
                "city must not be empty",
                "weather",
            ));
        }
        let requested_days = req.forecast_days.max(1);
        let started = std::time::Instant::now();
        let location = self.lookup_location(city).await?;
        let location_id = location.id.clone();
        let weather_location = location.to_weather_location()?;
        let current = self.fetch_current(&location_id).await?;
        let daily = self.fetch_daily(&location_id, requested_days).await?;
        // 上游每日预报档位只有 3d/7d，且个别地区可能不足 7 天。
        // 用真实返回条数覆盖请求天数，保证回复和诊断里的预报天数与上游一致。
        let forecast_days = daily.len() as u8;
        // 预警、空气质量和生活指数是增强信息：失败只影响附加段落，
        // 不能破坏实时天气和逐日预报的主链路可用性。
        let (alerts, air_quality, life_indices) = tokio::join!(
            self.fetch_alerts(weather_location.latitude, weather_location.longitude),
            self.fetch_air_quality(weather_location.latitude, weather_location.longitude),
            self.fetch_life_indices(&location_id)
        );

        Ok(WeatherOutcome {
            location: weather_location,
            current,
            daily,
            provider: "qweather".to_owned(),
            elapsed_ms: duration_ms(started.elapsed()),
            forecast_days,
            alerts: weather_supplement_or_failed("alert", alerts),
            air_quality: weather_supplement_or_failed("air_quality", air_quality),
            life_indices: weather_supplement_or_failed("life_indices", life_indices),
        })
    }

    fn provider_name(&self) -> &'static str {
        "qweather"
    }
}

/// 和风天气实时天气 API 响应。
#[derive(Debug, Deserialize)]
struct QWeatherNowResponse {
    code: String,
    now: QWeatherNow,
}

/// 和风天气实时天气数据。
#[derive(Debug, Deserialize)]
struct QWeatherNow {
    #[serde(rename = "obsTime")]
    obs_time: String,
    temp: String,
    #[serde(rename = "feelsLike")]
    feels_like: Option<String>,
    icon: String,
    humidity: Option<String>,
    precip: Option<String>,
    pressure: Option<String>,
    #[serde(rename = "windDir")]
    wind_dir: Option<String>,
    #[serde(rename = "windScale")]
    wind_scale: Option<String>,
    #[serde(rename = "windSpeed")]
    wind_speed: Option<String>,
}

/// 和风天气 3 天预报 API 响应。
#[derive(Debug, Deserialize)]
struct QWeatherDailyResponse {
    code: String,
    #[serde(default)]
    daily: Vec<QWeatherDaily>,
}

/// 和风天气每日预报数据。
#[derive(Debug, Deserialize)]
struct QWeatherDaily {
    #[serde(rename = "fxDate")]
    fx_date: String,
    #[serde(rename = "textDay")]
    text_day: Option<String>,
    #[serde(rename = "textNight")]
    text_night: Option<String>,
    #[serde(rename = "tempMax")]
    temp_max: String,
    #[serde(rename = "tempMin")]
    temp_min: String,
    #[serde(rename = "iconDay")]
    icon_day: String,
    pop: Option<String>,
    precip: Option<String>,
    humidity: Option<String>,
    #[serde(rename = "windDirDay")]
    wind_dir_day: Option<String>,
    #[serde(rename = "windScaleDay")]
    wind_scale_day: Option<String>,
}

/// 返回默认的和风天气 API 主机地址。
pub fn default_qweather_api_host() -> String {
    DEFAULT_QWEATHER_API_HOST.to_owned()
}

/// 返回默认的和风天气地理 API 主机地址。
pub fn default_qweather_geo_host() -> String {
    DEFAULT_QWEATHER_GEO_HOST.to_owned()
}

/// 根据 API 主机地址推导地理 API 主机地址（默认与 API 同主机）。
pub fn qweather_geo_host_from_api_host(api_host: &str) -> String {
    api_host.to_owned()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        Json, Router,
        extract::{OriginalUri, State},
        http::{HeaderMap, StatusCode as AxumStatusCode},
        response::IntoResponse,
        routing::get,
    };
    use tokio::{net::TcpListener, sync::Mutex};

    use super::{
        DisabledWeatherExecutor, QWEATHER_AIR_CURRENT_PATH_PREFIX,
        QWEATHER_ALERT_CURRENT_PATH_PREFIX, QWEATHER_API_KEY_HEADER, QWEATHER_WEATHER_3D_PATH,
        QWEATHER_WEATHER_7D_PATH, QWeatherExecutor, daily_forecast_path,
    };
    use crate::runtime::tools::weather::types::{
        WeatherExecutor, WeatherRequest, WeatherSupplementStatus,
    };

    #[tokio::test]
    async fn disabled_weather_backend_is_unavailable_and_never_calls_upstream() {
        let executor = DisabledWeatherExecutor;
        assert!(!executor.is_available());
        assert_eq!(executor.provider_name(), "disabled");

        let err = executor
            .weather(WeatherRequest {
                city: "杭州".to_owned(),
                forecast_days: 3,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, "config");
        assert!(err.message.contains("QWEATHER_API_KEY"));
    }

    #[derive(Debug, Default)]
    struct MockQWeatherV1State {
        requests: Vec<MockQWeatherV1Request>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct MockQWeatherV1Request {
        path: String,
        api_key: Option<String>,
        authorization: Option<String>,
    }

    async fn mock_qweather_v1_handler(
        State(state): State<Arc<Mutex<MockQWeatherV1State>>>,
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        {
            let mut state = state.lock().await;
            state.requests.push(MockQWeatherV1Request {
                path: uri.path().to_owned(),
                api_key: header_value(&headers, QWEATHER_API_KEY_HEADER),
                authorization: header_value(&headers, "authorization"),
            });
        }

        if uri.path().starts_with(QWEATHER_ALERT_CURRENT_PATH_PREFIX) {
            return (
                AxumStatusCode::OK,
                Json(serde_json::json!({
                    "metadata": { "zeroResult": false },
                    "alerts": [{
                        "messageType": { "code": "alert" },
                        "eventType": { "name": "雷电" },
                        "color": { "code": "yellow" },
                        "headline": "北京市气象台发布雷电黄色预警信号"
                    }]
                })),
            )
                .into_response();
        }

        if uri.path().starts_with(QWEATHER_AIR_CURRENT_PATH_PREFIX) {
            return (
                AxumStatusCode::OK,
                Json(serde_json::json!({
                    "metadata": { "zeroResult": false },
                    "indexes": [{
                        "code": "cn-mee",
                        "name": "AQI（CN）",
                        "aqiDisplay": "42",
                        "category": "优"
                    }]
                })),
            )
                .into_response();
        }

        AxumStatusCode::NOT_FOUND.into_response()
    }

    fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    async fn spawn_mock_qweather_v1() -> (String, Arc<Mutex<MockQWeatherV1State>>) {
        let state = Arc::new(Mutex::new(MockQWeatherV1State::default()));
        let app = Router::new()
            .route(
                "/weatheralert/v1/current/{lat}/{lon}",
                get(mock_qweather_v1_handler),
            )
            .route(
                "/airquality/v1/current/{lat}/{lon}",
                get(mock_qweather_v1_handler),
            )
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), state)
    }

    /// 生成 N 条和风天气每日预报 JSON，用于 mock `/v7/weather/3d` 与 `/7d` 返回体。
    fn mock_daily_json(count: usize) -> serde_json::Value {
        let daily = (1..=count)
            .map(|index| {
                serde_json::json!({
                    "fxDate": format!("2026-06-{index:02}"),
                    "textDay": "多云",
                    "textNight": "阴",
                    "tempMax": "30",
                    "tempMin": "20",
                    "iconDay": "101",
                    "pop": "10",
                    "precip": "0.0",
                    "humidity": "80",
                    "windDirDay": "东风",
                    "windScaleDay": "1-3",
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({ "code": "200", "daily": daily })
    }

    async fn mock_qweather_daily_handler(
        State(state): State<Arc<Mutex<Vec<String>>>>,
        OriginalUri(uri): OriginalUri,
    ) -> impl IntoResponse {
        {
            let mut state = state.lock().await;
            state.push(uri.path().to_owned());
        }
        let count = match uri.path() {
            QWEATHER_WEATHER_3D_PATH => 3,
            QWEATHER_WEATHER_7D_PATH => 7,
            _ => return AxumStatusCode::NOT_FOUND.into_response(),
        };
        (AxumStatusCode::OK, Json(mock_daily_json(count))).into_response()
    }

    /// 模拟和风天气每日预报接口，返回服务地址与按顺序记录的请求路径列表。
    async fn spawn_mock_qweather_daily() -> (String, Arc<Mutex<Vec<String>>>) {
        let paths = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v7/weather/3d", get(mock_qweather_daily_handler))
            .route("/v7/weather/7d", get(mock_qweather_daily_handler))
            .with_state(paths.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), paths)
    }

    #[test]
    fn daily_forecast_path_picks_endpoint_by_days() {
        // 1～3 天走 3d 步位，4～7 天走 7d 步位。
        for days in 1..=3 {
            assert_eq!(daily_forecast_path(days), QWEATHER_WEATHER_3D_PATH);
        }
        for days in 4..=7 {
            assert_eq!(daily_forecast_path(days), QWEATHER_WEATHER_7D_PATH);
        }
    }

    #[tokio::test]
    async fn qweather_fetch_daily_selects_path_and_truncates_to_requested_days() {
        let (api_host, paths) = spawn_mock_qweather_daily().await;
        let executor = QWeatherExecutor::new(
            5,
            "test-qweather-key".to_owned(),
            api_host.clone(),
            api_host,
        )
        .unwrap();

        // 请求 3 天调 3d 接口，返回 3 条。
        let daily_3 = executor.fetch_daily("101210101", 3).await.unwrap();
        assert_eq!(daily_3.len(), 3);
        // 请求 7 天调 7d 接口，返回 7 条。
        let daily_7 = executor.fetch_daily("101210101", 7).await.unwrap();
        assert_eq!(daily_7.len(), 7);
        // 请求 5 天仍调 7d 接口，本地截断到 5 条。
        let daily_5 = executor.fetch_daily("101210101", 5).await.unwrap();
        assert_eq!(daily_5.len(), 5);
        // 请求 1 天调 3d 接口，本地截断到 1 条。
        let daily_1 = executor.fetch_daily("101210101", 1).await.unwrap();
        assert_eq!(daily_1.len(), 1);

        let paths = paths.lock().await;
        assert_eq!(
            *paths,
            vec![
                QWEATHER_WEATHER_3D_PATH,
                QWEATHER_WEATHER_7D_PATH,
                QWEATHER_WEATHER_7D_PATH,
                QWEATHER_WEATHER_3D_PATH,
            ]
        );
    }

    #[tokio::test]
    async fn qweather_v1_supplements_use_api_key_header() {
        let (api_host, state) = spawn_mock_qweather_v1().await;
        let executor = QWeatherExecutor::new(
            5,
            "test-qweather-key".to_owned(),
            api_host.clone(),
            api_host,
        )
        .unwrap();

        let alerts = executor.fetch_alerts(39.90, 116.40).await.unwrap();
        let air_quality = executor.fetch_air_quality(39.90, 116.40).await.unwrap();

        assert_eq!(alerts.status, WeatherSupplementStatus::Available);
        assert_eq!(air_quality.status, WeatherSupplementStatus::Available);

        let state = state.lock().await;
        assert_eq!(state.requests.len(), 2);
        assert!(
            state
                .requests
                .iter()
                .any(|request| request.path == "/weatheralert/v1/current/39.90/116.40")
        );
        assert!(
            state
                .requests
                .iter()
                .any(|request| request.path == "/airquality/v1/current/39.90/116.40")
        );
        for request in &state.requests {
            assert_eq!(request.api_key.as_deref(), Some("test-qweather-key"));
            assert_eq!(request.authorization, None);
        }
    }
}

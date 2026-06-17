//! 天气查询模块。
//!
//! 基于和风天气（QWeather）API 获取实时天气、未来天气预报和可选增强摘要。
//! 支持城市名称模糊匹配、行政区划偏好排序等功能。

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;

use crate::{config::AppConfig, error::LlmError, util::metrics::duration_ms};

/// 默认预报天数。
pub const DEFAULT_FORECAST_DAYS: u8 = 3;
/// 和风天气 API 默认主机地址。
const DEFAULT_QWEATHER_API_HOST: &str = "https://api.qweather.com";
/// 和风天气地理 API 默认主机地址。
const DEFAULT_QWEATHER_GEO_HOST: &str = "https://geoapi.qweather.com";
/// 城市查询 API 路径。
const QWEATHER_GEO_CITY_LOOKUP_PATH: &str = "/geo/v2/city/lookup";
/// 实时天气 API 路径。
const QWEATHER_WEATHER_NOW_PATH: &str = "/v7/weather/now";
/// 3 天预报 API 路径。
const QWEATHER_WEATHER_3D_PATH: &str = "/v7/weather/3d";
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
/// 地名的行政区划偏好映射，用于消除同名地点歧义。
///
/// 格式：(用户输入的短名称, 期望的完整行政区划名)
const WEATHER_PLACE_PREFERENCES: &[(&str, &str)] = &[
    ("西湖", "杭州市西湖区"),
    ("西湖区", "杭州市西湖区"),
    ("萧山", "杭州市萧山区"),
    ("萧山区", "杭州市萧山区"),
    ("江北", "重庆市江北区"),
    ("江北区", "重庆市江北区"),
];
/// 城市查询覆盖映射，用于某些地名直接查询不到时改用更准确的查询词。
const WEATHER_LOOKUP_OVERRIDES: &[(&str, &str)] = &[("江北", "重庆江北"), ("江北区", "重庆江北")];

/// 天气查询请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeatherRequest {
    /// 城市名称
    pub city: String,
    /// 预报天数
    pub forecast_days: u8,
}

/// 地理位置信息。
#[derive(Debug, Clone, PartialEq)]
pub struct WeatherLocation {
    /// 和风天气城市 ID
    pub id: Option<String>,
    /// 城市名称
    pub name: String,
    /// 国家
    pub country: Option<String>,
    /// 省级行政区
    pub admin1: Option<String>,
    /// 地级行政区
    pub admin2: Option<String>,
    /// 时区
    pub timezone: Option<String>,
    /// 纬度
    pub latitude: f64,
    /// 经度
    pub longitude: f64,
}

/// 当前实时天气。
#[derive(Debug, Clone, PartialEq)]
pub struct CurrentWeather {
    /// 观测时间
    pub time: String,
    /// 温度（摄氏度）
    pub temperature_c: f64,
    /// 体感温度（摄氏度）
    pub apparent_temperature_c: Option<f64>,
    /// 天气状况代码
    pub weather_code: u16,
    /// 相对湿度百分比
    pub humidity_percent: Option<u8>,
    /// 降水量（毫米）
    pub precipitation_mm: Option<f64>,
    /// 气压（hPa）
    pub pressure_hpa: Option<u16>,
    /// 风向描述
    pub wind_direction: Option<String>,
    /// 风力等级
    pub wind_scale: Option<String>,
    /// 风速（公里/小时）
    pub wind_speed_kmh: Option<f64>,
}

/// 每日天气预报。
#[derive(Debug, Clone, PartialEq)]
pub struct DailyWeather {
    /// 预报日期
    pub date: String,
    /// 天气状况代码
    pub weather_code: u16,
    /// 白天天气描述
    pub weather_day: Option<String>,
    /// 夜间天气描述
    pub weather_night: Option<String>,
    /// 最高温度（摄氏度）
    pub temperature_max_c: f64,
    /// 最低温度（摄氏度）
    pub temperature_min_c: f64,
    /// 最大降水概率百分比
    pub precipitation_probability_max: Option<u8>,
    /// 降水量（毫米）
    pub precipitation_mm: Option<f64>,
    /// 相对湿度百分比
    pub humidity_percent: Option<u8>,
    /// 白天风向
    pub wind_direction_day: Option<String>,
    /// 白天风力等级
    pub wind_scale_day: Option<String>,
}

/// 实时天气预警摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeatherAlert {
    /// 预警标题或简要描述
    pub headline: String,
    /// 预警事件名称
    pub event_name: Option<String>,
    /// 预警严重程度原始字段
    pub severity: Option<String>,
    /// 预警颜色原始代码
    pub color_code: Option<String>,
    /// 发布机构
    pub sender_name: Option<String>,
    /// 发布时间
    pub issued_time: Option<String>,
    /// 失效时间
    pub expire_time: Option<String>,
    /// 详细描述
    pub description: Option<String>,
}

/// 实时空气质量摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirQualitySummary {
    /// AQI 类型代码
    pub code: Option<String>,
    /// AQI 类型名称
    pub name: Option<String>,
    /// AQI 展示值
    pub aqi_display: String,
    /// AQI 等级
    pub level: Option<String>,
    /// AQI 类别
    pub category: Option<String>,
    /// 首要污染物名称
    pub primary_pollutant: Option<String>,
}

/// 天气生活指数摘要。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeatherLifeIndex {
    /// 预报日期
    pub date: String,
    /// 指数类型 ID
    pub type_id: String,
    /// 指数名称
    pub name: String,
    /// 指数等级
    pub level: Option<String>,
    /// 指数类别
    pub category: Option<String>,
    /// 指数说明
    pub text: Option<String>,
}

/// 附加天气数据的查询状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WeatherSupplementStatus {
    /// 未请求，主要用于测试或兼容旧构造。
    NotRequested,
    /// 请求并解析成功且有可展示数据。
    Available,
    /// 请求并解析成功，但上游明确无数据或结果为空。
    Empty,
    /// 请求、业务状态码或解析失败。
    Failed,
}

impl WeatherSupplementStatus {
    /// 转换为 diagnostics 中使用的稳定字符串。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::Available => "data",
            Self::Empty => "empty",
            Self::Failed => "error",
        }
    }
}

/// 附加天气数据及其诊断信息。
#[derive(Debug, Clone, PartialEq)]
pub struct WeatherSupplement<T> {
    /// 查询状态
    pub status: WeatherSupplementStatus,
    /// 成功时的可展示数据
    pub data: Option<T>,
    /// 上游是否明确返回 zeroResult
    pub zero_result: Option<bool>,
    /// 失败时的错误码
    pub error_code: Option<String>,
    /// 失败时的错误阶段
    pub error_stage: Option<String>,
}

impl<T> Default for WeatherSupplement<T> {
    fn default() -> Self {
        Self {
            status: WeatherSupplementStatus::NotRequested,
            data: None,
            zero_result: None,
            error_code: None,
            error_stage: None,
        }
    }
}

impl<T> WeatherSupplement<T> {
    /// 构造成功且有数据的附加结果。
    pub fn available(data: T) -> Self {
        Self {
            status: WeatherSupplementStatus::Available,
            data: Some(data),
            zero_result: None,
            error_code: None,
            error_stage: None,
        }
    }

    /// 构造成功但无数据的附加结果。
    pub fn empty(zero_result: Option<bool>) -> Self {
        Self {
            status: WeatherSupplementStatus::Empty,
            data: None,
            zero_result,
            error_code: None,
            error_stage: None,
        }
    }

    /// 构造失败的附加结果。只保留诊断分类，避免把上游 URL 或凭据写进响应。
    pub fn failed(err: &LlmError) -> Self {
        Self {
            status: WeatherSupplementStatus::Failed,
            data: None,
            zero_result: None,
            error_code: Some(err.code.clone()),
            error_stage: Some(err.stage.clone()),
        }
    }
}

/// 天气查询结果。
#[derive(Debug, Clone, PartialEq)]
pub struct WeatherOutcome {
    /// 地理位置信息
    pub location: WeatherLocation,
    /// 当前实时天气
    pub current: CurrentWeather,
    /// 逐日预报列表
    pub daily: Vec<DailyWeather>,
    /// 服务提供商名称
    pub provider: String,
    /// 查询耗时（毫秒）
    pub elapsed_ms: u64,
    /// 预报天数
    pub forecast_days: u8,
    /// 实时天气预警
    pub alerts: WeatherSupplement<Vec<WeatherAlert>>,
    /// 实时空气质量
    pub air_quality: WeatherSupplement<AirQualitySummary>,
    /// 常用生活指数
    pub life_indices: WeatherSupplement<Vec<WeatherLifeIndex>>,
}

/// 天气查询执行器 trait。
#[async_trait]
pub trait WeatherExecutor: Send + Sync {
    /// 查询天气。
    async fn weather(&self, req: WeatherRequest) -> Result<WeatherOutcome, LlmError>;
    /// 返回服务提供商名称。
    fn provider_name(&self) -> &'static str;
}

/// 动态派发的天气查询执行器。
pub type DynWeatherExecutor = Arc<dyn WeatherExecutor>;

/// 根据配置构建和风天气执行器。
pub fn build_weather_executor(config: &AppConfig) -> Result<DynWeatherExecutor, LlmError> {
    Ok(Arc::new(QWeatherExecutor::new(
        config.request_timeout_seconds,
        config.qweather_api_key.clone(),
        config.qweather_api_host.clone(),
        config.qweather_geo_host.clone(),
    )?))
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
            .user_agent(format!("qq-maid-llm/{}", env!("CARGO_PKG_VERSION")))
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

    /// 获取指定位置的未来 3 天天气预报。
    async fn fetch_daily(&self, location_id: &str) -> Result<Vec<DailyWeather>, LlmError> {
        let mut url = qweather_url(&self.api_host, QWEATHER_WEATHER_3D_PATH)?;
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
        let response = ensure_http_success(response, "QWeather weather 3d").await?;
        let body: QWeatherDailyResponse = response.json().await.map_err(|err| {
            LlmError::provider(format!("invalid QWeather weather 3d JSON: {err}"), "json")
        })?;
        if body.code != QWEATHER_SUCCESS_CODE {
            return Err(qweather_code_error("QWeather weather 3d", &body.code));
        }

        let daily = body
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
                "QWeather weather 3d missing daily weather",
                "provider",
            ));
        }
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
        let forecast_days = req.forecast_days.max(1);
        let started = std::time::Instant::now();
        let location = self.lookup_location(city).await?;
        let location_id = location.id.clone();
        let weather_location = location.to_weather_location()?;
        let current = self.fetch_current(&location_id).await?;
        let daily = self.fetch_daily(&location_id).await?;
        // 预警、空气质量和生活指数是增强信息：失败只影响附加段落，
        // 不能破坏实时天气和三天预报的主链路可用性。
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

/// 和风天气地理编码 API 响应。
#[derive(Debug, Deserialize)]
struct QWeatherGeoResponse {
    /// 状态码
    code: String,
    /// 地理位置列表
    #[serde(default)]
    location: Vec<QWeatherGeoLocation>,
}

/// 和风天气地理编码结果。
#[derive(Debug, Clone, Deserialize)]
struct QWeatherGeoLocation {
    /// 地点名称
    name: String,
    /// 地点 ID（用于后续天气查询）
    id: String,
    /// 纬度（字符串）
    lat: String,
    /// 经度（字符串）
    lon: String,
    /// 省级行政区
    adm1: String,
    /// 地级行政区
    adm2: String,
    /// 国家
    country: String,
    /// 时区
    tz: String,
    /// 区域排名（数字越小优先级越高）
    rank: String,
}

impl QWeatherGeoLocation {
    /// 转换为公开的 WeatherLocation 类型。
    fn to_weather_location(&self) -> Result<WeatherLocation, LlmError> {
        Ok(WeatherLocation {
            id: Some(self.id.clone()),
            name: self.name.clone(),
            country: Some(self.country.clone()).filter(|value| !value.trim().is_empty()),
            admin1: Some(self.adm1.clone()).filter(|value| !value.trim().is_empty()),
            admin2: Some(self.adm2.clone()).filter(|value| !value.trim().is_empty()),
            timezone: Some(self.tz.clone()).filter(|value| !value.trim().is_empty()),
            latitude: parse_f64_field(&self.lat, "QWeather location lat")?,
            longitude: parse_f64_field(&self.lon, "QWeather location lon")?,
        })
    }

    /// 获取排名数值，用于地点优先级比较。
    fn rank_value(&self) -> i64 {
        self.rank.trim().parse::<i64>().unwrap_or(0)
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

/// 和风天气 v1 API 通用元数据。
#[derive(Debug, Deserialize)]
struct QWeatherV1Metadata {
    /// true 表示请求成功但无数据。
    #[serde(rename = "zeroResult")]
    zero_result: Option<bool>,
}

/// 和风天气实时预警 API 响应。
#[derive(Debug, Deserialize)]
struct QWeatherAlertResponse {
    metadata: Option<QWeatherV1Metadata>,
    #[serde(default)]
    alerts: Vec<QWeatherAlert>,
}

/// 和风天气实时预警数据。
#[derive(Debug, Deserialize)]
struct QWeatherAlert {
    #[serde(rename = "senderName")]
    sender_name: Option<String>,
    #[serde(rename = "issuedTime")]
    issued_time: Option<String>,
    #[serde(rename = "messageType")]
    message_type: Option<QWeatherAlertMessageType>,
    #[serde(rename = "eventType")]
    event_type: Option<QWeatherAlertEventType>,
    severity: Option<String>,
    color: Option<QWeatherAlertColor>,
    #[serde(rename = "expireTime")]
    expire_time: Option<String>,
    headline: Option<String>,
    description: Option<String>,
}

/// 和风天气预警消息性质。
#[derive(Debug, Deserialize)]
struct QWeatherAlertMessageType {
    code: Option<String>,
}

/// 和风天气预警事件类型。
#[derive(Debug, Deserialize)]
struct QWeatherAlertEventType {
    name: Option<String>,
}

/// 和风天气预警颜色字段。
#[derive(Debug, Deserialize)]
struct QWeatherAlertColor {
    code: Option<String>,
}

impl QWeatherAlert {
    /// 官方字段明确表示取消时过滤；不根据时间自行推断是否仍然生效。
    fn is_cancelled(&self) -> bool {
        self.message_type
            .as_ref()
            .and_then(|message_type| message_type.code.as_deref())
            .map(|code| code.trim().eq_ignore_ascii_case("cancel"))
            .unwrap_or(false)
    }

    /// 转换为公开预警摘要。标题缺失时用事件名或描述兜底，仍避免展示空预警。
    fn into_weather_alert(self) -> Option<WeatherAlert> {
        let event_name = self
            .event_type
            .and_then(|event_type| non_empty_string(event_type.name));
        let description = non_empty_string(self.description);
        let headline = non_empty_string(self.headline)
            .or_else(|| event_name.clone())
            .or_else(|| description.clone())?;
        Some(WeatherAlert {
            headline,
            event_name,
            severity: non_empty_string(self.severity),
            color_code: self.color.and_then(|color| non_empty_string(color.code)),
            sender_name: non_empty_string(self.sender_name),
            issued_time: non_empty_string(self.issued_time),
            expire_time: non_empty_string(self.expire_time),
            description,
        })
    }
}

/// 和风天气实时空气质量 API 响应。
#[derive(Debug, Deserialize)]
struct QWeatherAirQualityResponse {
    metadata: Option<QWeatherV1Metadata>,
    #[serde(default)]
    indexes: Vec<QWeatherAirQualityIndex>,
}

/// 和风天气空气质量指数。
#[derive(Debug, Clone, Deserialize)]
struct QWeatherAirQualityIndex {
    code: Option<String>,
    name: Option<String>,
    aqi: Option<Value>,
    #[serde(rename = "aqiDisplay")]
    aqi_display: Option<String>,
    level: Option<String>,
    category: Option<String>,
    #[serde(rename = "primaryPollutant")]
    primary_pollutant: Option<QWeatherAirQualityPollutant>,
}

/// 和风天气首要污染物摘要。
#[derive(Debug, Clone, Deserialize)]
struct QWeatherAirQualityPollutant {
    code: Option<String>,
    name: Option<String>,
}

impl QWeatherAirQualityIndex {
    /// 转换为公开空气质量摘要；没有任何可展示 AQI 值时视为无数据。
    fn into_air_quality_summary(self) -> Option<AirQualitySummary> {
        let aqi_display = non_empty_string(self.aqi_display)
            .or_else(|| self.aqi.as_ref().and_then(value_to_display_string))?;
        let primary_pollutant = self.primary_pollutant.and_then(|pollutant| {
            non_empty_string(pollutant.name).or_else(|| non_empty_string(pollutant.code))
        });
        Some(AirQualitySummary {
            code: non_empty_string(self.code),
            name: non_empty_string(self.name),
            aqi_display,
            level: non_empty_string(self.level),
            category: non_empty_string(self.category),
            primary_pollutant,
        })
    }
}

/// 和风天气生活指数 API 响应。
#[derive(Debug, Deserialize)]
struct QWeatherIndicesResponse {
    code: String,
    #[serde(default)]
    daily: Vec<QWeatherIndexDaily>,
}

/// 和风天气单条生活指数预报。
#[derive(Debug, Deserialize)]
struct QWeatherIndexDaily {
    date: Option<String>,
    #[serde(rename = "type")]
    type_id: Option<String>,
    name: Option<String>,
    level: Option<String>,
    category: Option<String>,
    text: Option<String>,
}

impl QWeatherIndexDaily {
    /// 转换为公开生活指数摘要。核心标识字段缺失时跳过该条，保留其他可用记录。
    fn into_weather_life_index(self) -> Option<WeatherLifeIndex> {
        Some(WeatherLifeIndex {
            date: non_empty_string(self.date)?,
            type_id: non_empty_string(self.type_id)?,
            name: non_empty_string(self.name)?,
            level: non_empty_string(self.level),
            category: non_empty_string(self.category),
            text: non_empty_string(self.text),
        })
    }
}

/// 从多个候选地点中选择最匹配的一个。
///
/// 选择策略：
/// 1. 只考虑中国境内的地点
/// 2. 优先使用 `WEATHER_PLACE_PREFERENCES` 配置的偏好地点
/// 3. 其次尝试完全匹配名称
/// 4. 最后按和风天气排名选择（数字越小越优）
/// 5. 如果多个地点排名相同，则视为歧义返回错误
fn select_location(
    original_city: &str,
    candidates: Vec<QWeatherGeoLocation>,
) -> Result<QWeatherGeoLocation, LlmError> {
    let candidates = candidates
        .into_iter()
        .filter(|location| is_china_country(&location.country))
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return Err(LlmError::new(
            "not_found",
            format!("QWeather GeoAPI found no China match for `{original_city}`"),
            "weather",
        ));
    }
    if candidates.len() == 1 {
        return Ok(candidates.into_iter().next().unwrap());
    }

    if let Some(preferred) = preferred_location(original_city, &candidates) {
        return Ok(preferred);
    }

    if let Some(exact) = exact_location(original_city, &candidates) {
        return Ok(exact);
    }

    let mut ranked = candidates;
    ranked.sort_by_key(QWeatherGeoLocation::rank_value);
    let top_rank = ranked[0].rank_value();
    let second_rank = ranked.get(1).map(QWeatherGeoLocation::rank_value);
    if second_rank.is_none_or(|rank| top_rank < rank) {
        return Ok(ranked.remove(0));
    }

    Err(LlmError::new(
        "not_found",
        format!("QWeather GeoAPI found multiple ambiguous matches for `{original_city}`"),
        "weather",
    ))
}

/// 获取用于 API 查询的城市名称，应用覆盖映射。
fn lookup_city_query(city: &str) -> String {
    let key = normalize_place_key(city);
    WEATHER_LOOKUP_OVERRIDES
        .iter()
        .find_map(|(alias, query)| (normalize_place_key(alias) == key).then_some(*query))
        .unwrap_or(city)
        .to_owned()
}

/// 在候选中寻找与原始城市名完全匹配的地点（唯一时返回）。
fn exact_location(
    original_city: &str,
    candidates: &[QWeatherGeoLocation],
) -> Option<QWeatherGeoLocation> {
    let key = normalize_place_key(original_city);
    let exact = candidates
        .iter()
        .filter(|location| {
            location
                .match_keys()
                .iter()
                .any(|candidate| place_keys_match(candidate, &key))
        })
        .collect::<Vec<_>>();
    (exact.len() == 1).then(|| exact[0]).cloned()
}

/// 根据 `WEATHER_PLACE_PREFERENCES` 配置查找首选地点。
fn preferred_location(
    original_city: &str,
    candidates: &[QWeatherGeoLocation],
) -> Option<QWeatherGeoLocation> {
    let key = normalize_place_key(original_city);
    let target = WEATHER_PLACE_PREFERENCES
        .iter()
        .find_map(|(alias, preferred)| (normalize_place_key(alias) == key).then_some(*preferred))?;
    let target = normalize_admin_place_key(target);

    candidates
        .iter()
        .find(|location| {
            location
                .match_keys()
                .iter()
                .any(|candidate| normalize_admin_place_key(candidate) == target)
        })
        .cloned()
}

/// 判断国家名称是否指向中国。
fn is_china_country(country: &str) -> bool {
    matches!(
        normalize_place_key(country).as_str(),
        "中国" | "中华人民共和国" | "china" | "cn" | "prc"
    )
}

impl QWeatherGeoLocation {
    /// 生成该地点的所有匹配键，用于与用户输入进行模糊匹配。
    fn match_keys(&self) -> Vec<String> {
        let adm2_city = append_city_suffix(&self.adm2);
        vec![
            self.name.clone(),
            format!("{}{}", self.adm2, self.name),
            format!("{}{}", adm2_city, self.name),
            format!("{}{}{}", self.adm1, self.adm2, self.name),
            format!("{}{}{}", self.adm1, adm2_city, self.name),
        ]
    }
}

/// 为地名追加"市"后缀（如果尚未包含合适的行政区划后缀）。
fn append_city_suffix(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.ends_with('市')
        || trimmed.ends_with("自治州")
        || trimmed.ends_with('盟')
        || trimmed.ends_with("地区")
    {
        return trimmed.to_owned();
    }
    format!("{trimmed}市")
}

/// 标准化地名：去除首尾空格、转为小写、移除空格和逗号。
fn normalize_place_key(input: &str) -> String {
    input
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '\u{3000}', ',', '，'], "")
}

/// 标准化行政区划键：在 `normalize_place_key` 基础上移除"省"、"市"等后缀。
fn normalize_admin_place_key(input: &str) -> String {
    normalize_place_key(input)
        .replace("自治州", "")
        .replace("地区", "")
        .replace(['省', '市', '区', '县', '乡', '镇', '盟'], "")
}

/// 判断候选地点键是否与标准化查询字符串匹配。
fn place_keys_match(candidate: &str, normalized_query: &str) -> bool {
    normalize_place_key(candidate) == normalized_query
        || normalize_admin_place_key(candidate) == normalize_admin_place_key(normalized_query)
}

/// 构造需要经纬度路径参数的和风天气 v1 API 路径。
fn qweather_coordinate_path(prefix: &str, latitude: f64, longitude: f64) -> String {
    format!(
        "{}/{:.2}/{:.2}",
        prefix.trim_end_matches('/'),
        latitude,
        longitude
    )
}

/// 构造和风天气 API URL，自动处理 scheme 和路径拼接。
fn qweather_url(host: &str, path: &str) -> Result<Url, LlmError> {
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
fn parse_f64_field(value: &str, field: &str) -> Result<f64, LlmError> {
    value
        .trim()
        .parse::<f64>()
        .map_err(|_| LlmError::provider(format!("{field} is not a number"), "provider"))
}

/// 解析可选的 f64 字段。
fn parse_optional_f64_field(value: Option<&str>, field: &str) -> Result<Option<f64>, LlmError> {
    value.map(|value| parse_f64_field(value, field)).transpose()
}

/// 解析 u16 字段（天气代码）。
fn parse_u16_field(value: &str, field: &str) -> Result<u16, LlmError> {
    value
        .trim()
        .parse::<u16>()
        .map_err(|_| LlmError::provider(format!("{field} is not a weather code"), "provider"))
}

/// 解析可选的 u8 字段（百分比值）。
fn parse_optional_u8_field(value: Option<&str>, field: &str) -> Result<Option<u8>, LlmError> {
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
fn parse_optional_u16_field(value: Option<&str>, field: &str) -> Result<Option<u16>, LlmError> {
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
fn non_empty_string(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// 将预警响应转换为附加摘要状态。
fn weather_alert_supplement(body: QWeatherAlertResponse) -> WeatherSupplement<Vec<WeatherAlert>> {
    let zero_result = body.metadata.and_then(|metadata| metadata.zero_result);
    let alerts = body
        .alerts
        .into_iter()
        .filter(|alert| !alert.is_cancelled())
        .filter_map(QWeatherAlert::into_weather_alert)
        .collect::<Vec<_>>();

    if zero_result == Some(true) || alerts.is_empty() {
        return WeatherSupplement::empty(zero_result);
    }
    WeatherSupplement::available(alerts)
}

/// 将空气质量响应转换为附加摘要状态。
fn air_quality_supplement(
    body: QWeatherAirQualityResponse,
) -> WeatherSupplement<AirQualitySummary> {
    let zero_result = body.metadata.and_then(|metadata| metadata.zero_result);
    if zero_result == Some(true) {
        return WeatherSupplement::empty(zero_result);
    }
    let Some(index) = select_air_quality_index(body.indexes) else {
        return WeatherSupplement::empty(zero_result);
    };
    let Some(summary) = index.into_air_quality_summary() else {
        return WeatherSupplement::empty(zero_result);
    };

    WeatherSupplement::available(summary)
}

/// 将生活指数响应转换为附加摘要状态。
fn life_indices_supplement(
    body: QWeatherIndicesResponse,
) -> Result<WeatherSupplement<Vec<WeatherLifeIndex>>, LlmError> {
    if body.code == QWEATHER_EMPTY_CODE {
        return Ok(WeatherSupplement::empty(None));
    }
    if body.code != QWEATHER_SUCCESS_CODE {
        return Err(qweather_code_error(
            "QWeather weather indices 3d",
            &body.code,
        ));
    }

    let indices = body
        .daily
        .into_iter()
        .filter_map(QWeatherIndexDaily::into_weather_life_index)
        .collect::<Vec<_>>();
    if indices.is_empty() {
        return Ok(WeatherSupplement::empty(None));
    }
    Ok(WeatherSupplement::available(indices))
}

/// 从空气质量索引列表中选择最适合展示的一项。
///
/// 和风 v1 会返回当地 AQI 与 QAQI；当地标准不是固定代码，
/// 因此只把官方通用 `qaqi` 作为回退标记，其余可用指数视为当地标准。
fn select_air_quality_index(
    indexes: Vec<QWeatherAirQualityIndex>,
) -> Option<QWeatherAirQualityIndex> {
    let indexes = indexes
        .into_iter()
        .filter(air_quality_index_has_value)
        .collect::<Vec<_>>();
    indexes
        .iter()
        .find(|index| !is_qaqi_index(index))
        .cloned()
        .or_else(|| indexes.iter().find(|index| is_qaqi_index(index)).cloned())
        .or_else(|| indexes.into_iter().next())
}

/// 判断空气质量索引是否有可展示 AQI 值。
fn air_quality_index_has_value(index: &QWeatherAirQualityIndex) -> bool {
    index
        .aqi_display
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || index
            .aqi
            .as_ref()
            .and_then(value_to_display_string)
            .is_some()
}

/// 判断空气质量索引是否为和风通用 QAQI。
fn is_qaqi_index(index: &QWeatherAirQualityIndex) -> bool {
    index
        .code
        .as_deref()
        .is_some_and(|code| code.trim().eq_ignore_ascii_case(QWEATHER_QAQI_CODE))
}

/// 将 JSON 数值或字符串转换为展示文本。
fn value_to_display_string(value: &Value) -> Option<String> {
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
fn weather_supplement_or_failed<T>(
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
fn map_weather_request_error(err: reqwest::Error) -> LlmError {
    if err.is_timeout() {
        return LlmError::timeout("weather");
    }
    LlmError::http(format!("QWeather request failed: {err}"))
}

/// 检查 HTTP 响应状态码，非成功时根据状态码返回适当的错误。
async fn ensure_http_success(
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
fn qweather_code_error(stage: &str, code: &str) -> LlmError {
    match code {
        "204" | "404" => LlmError::new(
            "not_found",
            format!("{stage} returned QWeather code {code}"),
            "weather",
        ),
        _ => LlmError::http(format!("{stage} returned QWeather code {code}")),
    }
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
    use super::*;
    use axum::{
        Json, Router,
        extract::{OriginalUri, State},
        http::{HeaderMap, StatusCode as AxumStatusCode},
        response::IntoResponse,
        routing::get,
    };
    use tokio::{net::TcpListener, sync::Mutex};

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

    fn location(name: &str, adm1: &str, adm2: &str, rank: &str) -> QWeatherGeoLocation {
        QWeatherGeoLocation {
            name: name.to_owned(),
            id: format!("{adm2}-{name}"),
            lat: "30.25".to_owned(),
            lon: "120.16".to_owned(),
            adm1: adm1.to_owned(),
            adm2: adm2.to_owned(),
            country: "中国".to_owned(),
            tz: "Asia/Shanghai".to_owned(),
            rank: rank.to_owned(),
        }
    }

    // ---- candidate builders (shared by table-driven tests) ----

    fn west_lake_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("西湖区", "重庆市", "重庆", "20"),
            location("西湖区", "浙江省", "杭州", "10"),
        ]
    }

    fn xiaoshan_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("萧山区", "浙江省", "杭州", "10"),
            location("萧山区", "其它省", "其它", "20"),
        ]
    }

    fn jiangbei_chongqing_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("江北区", "重庆市", "重庆", "20"),
            location("江北区", "浙江省", "宁波", "10"),
        ]
    }

    fn hangzhou_exact_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("杭州", "浙江省", "杭州", "11"),
            location("萧山", "浙江省", "杭州", "23"),
            location("桐庐", "浙江省", "杭州", "33"),
        ]
    }

    fn jiangbei_rank_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("重庆", "重庆市", "重庆", "11"),
            location("永川", "重庆市", "重庆", "23"),
            location("北碚", "重庆市", "重庆", "35"),
        ]
    }

    fn west_lake_no_district_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("西湖乡", "台湾省", "苗栗县", "77"),
            location("西湖", "浙江省", "杭州", "35"),
            location("西湖", "江西省", "南昌", "35"),
        ]
    }

    fn no_china_candidate() -> Vec<QWeatherGeoLocation> {
        vec![QWeatherGeoLocation {
            country: "美国".to_owned(),
            ..location("西湖区", "浙江省", "杭州", "10")
        }]
    }

    fn ambiguous_same_rank_candidates() -> Vec<QWeatherGeoLocation> {
        vec![
            location("重名地", "甲省", "甲市", "10"),
            location("重名地", "乙省", "乙市", "10"),
        ]
    }

    // ---- table-driven select_location tests ----

    /// 合并 8 个 select_location 成功路径测试为表驱动测试。
    /// 每个 case 名称对应原独立测试函数，便于失败定位。
    #[test]
    fn select_location_prefers_best_match() {
        struct Case {
            /// 原测试函数名，失败时用于定位
            name: &'static str,
            query: &'static str,
            candidates: Vec<QWeatherGeoLocation>,
            expected_adm1: &'static str,
            expected_adm2: &'static str,
            expected_name: &'static str,
        }

        let cases = [
            Case {
                name: "select_location_prefers_hangzhou_west_lake_for_short_name",
                query: "西湖",
                candidates: west_lake_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "西湖区",
            },
            Case {
                name: "select_location_prefers_hangzhou_west_lake_for_district_name",
                query: "西湖区",
                candidates: west_lake_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "西湖区",
            },
            Case {
                name: "select_location_prefers_hangzhou_xiaoshan_for_short_name",
                query: "萧山",
                candidates: xiaoshan_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "萧山区",
            },
            Case {
                name: "select_location_prefers_hangzhou_xiaoshan_for_district_name",
                query: "萧山区",
                candidates: xiaoshan_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "萧山区",
            },
            Case {
                name: "select_location_prefers_chongqing_jiangbei",
                query: "江北",
                candidates: jiangbei_chongqing_candidates(),
                expected_adm1: "重庆市",
                expected_adm2: "重庆",
                expected_name: "江北区",
            },
            Case {
                name: "select_location_prefers_exact_city_name_before_rank",
                query: "杭州",
                candidates: hangzhou_exact_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "杭州",
            },
            Case {
                name: "select_location_treats_lower_qweather_rank_as_better",
                query: "江北",
                candidates: jiangbei_rank_candidates(),
                expected_adm1: "重庆市",
                expected_adm2: "重庆",
                expected_name: "重庆",
            },
            Case {
                name: "preference_matches_qweather_locations_without_district_suffix",
                query: "西湖区",
                candidates: west_lake_no_district_candidates(),
                expected_adm1: "浙江省",
                expected_adm2: "杭州",
                expected_name: "西湖",
            },
        ];

        for case in &cases {
            let selected = select_location(case.query, case.candidates.clone())
                .unwrap_or_else(|e| panic!("case '{}' failed: unwrap error {:?}", case.name, e));
            assert_eq!(
                selected.adm1, case.expected_adm1,
                "case '{}' failed: adm1 mismatch",
                case.name
            );
            assert_eq!(
                selected.adm2, case.expected_adm2,
                "case '{}' failed: adm2 mismatch",
                case.name
            );
            assert_eq!(
                selected.name, case.expected_name,
                "case '{}' failed: name mismatch",
                case.name
            );
        }
    }

    /// 合并 2 个 select_location not_found 错误路径测试。
    #[test]
    fn select_location_returns_not_found() {
        struct Case {
            name: &'static str,
            query: &'static str,
            candidates: Vec<QWeatherGeoLocation>,
        }

        let cases = [
            Case {
                name: "select_location_returns_not_found_for_no_china_match",
                query: "西湖",
                candidates: no_china_candidate(),
            },
            Case {
                name: "select_location_returns_not_found_for_ambiguous_same_rank",
                query: "重名地",
                candidates: ambiguous_same_rank_candidates(),
            },
        ];

        for case in &cases {
            let err = select_location(case.query, case.candidates.clone()).expect_err(&format!(
                "case '{}' failed: expected Err, got Ok",
                case.name
            ));
            assert_eq!(
                err.code, "not_found",
                "case '{}' failed: expected code 'not_found', got '{}'",
                case.name, err.code
            );
        }
    }

    // ---- non-table-driven tests (different functions) ----

    #[test]
    fn lookup_city_query_uses_chongqing_for_short_jiangbei() {
        assert_eq!(lookup_city_query("江北"), "重庆江北");
        assert_eq!(lookup_city_query("江北区"), "重庆江北");
        assert_eq!(lookup_city_query("宁波江北"), "宁波江北");
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

    #[test]
    fn weather_alert_supplement_parses_alerts_and_zero_result() {
        let body: QWeatherAlertResponse = serde_json::from_value(serde_json::json!({
            "metadata": { "zeroResult": false },
            "alerts": [
                {
                    "senderName": "杭州市气象台",
                    "issuedTime": "2026-06-12T18:00+08:00",
                    "messageType": { "code": "alert" },
                    "eventType": { "name": "大风" },
                    "severity": "minor",
                    "color": { "code": "blue" },
                    "expireTime": "2026-06-13T18:00+08:00",
                    "headline": "大风蓝色预警",
                    "description": "预计未来24小时阵风较大。"
                },
                {
                    "messageType": { "code": "cancel" },
                    "eventType": { "name": "雷电" },
                    "headline": "取消的预警不展示"
                }
            ]
        }))
        .unwrap();

        let supplement = weather_alert_supplement(body);

        assert_eq!(supplement.status, WeatherSupplementStatus::Available);
        let alerts = supplement.data.unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].headline, "大风蓝色预警");
        assert_eq!(alerts[0].color_code.as_deref(), Some("blue"));

        let empty_body: QWeatherAlertResponse = serde_json::from_value(serde_json::json!({
            "metadata": { "zeroResult": true },
            "alerts": []
        }))
        .unwrap();
        let empty = weather_alert_supplement(empty_body);

        assert_eq!(empty.status, WeatherSupplementStatus::Empty);
        assert_eq!(empty.zero_result, Some(true));
    }

    #[test]
    fn air_quality_supplement_prefers_local_then_qaqi_then_first_available() {
        let local_body: QWeatherAirQualityResponse = serde_json::from_value(serde_json::json!({
            "metadata": { "zeroResult": false },
            "indexes": [
                { "code": "qaqi", "name": "QAQI", "aqi": 0.8, "aqiDisplay": "0.8", "category": "Excellent" },
                { "code": "cn-mee", "name": "AQI（CN）", "aqi": 42, "aqiDisplay": "42", "category": "优",
                  "primaryPollutant": { "code": "pm2p5", "name": "PM2.5" } }
            ]
        }))
        .unwrap();

        let local = air_quality_supplement(local_body).data.unwrap();
        assert_eq!(local.code.as_deref(), Some("cn-mee"));
        assert_eq!(local.aqi_display, "42");
        assert_eq!(local.primary_pollutant.as_deref(), Some("PM2.5"));

        let qaqi_body: QWeatherAirQualityResponse = serde_json::from_value(serde_json::json!({
            "indexes": [
                { "code": "qaqi", "name": "QAQI", "aqi": 0.9, "category": "Excellent" }
            ]
        }))
        .unwrap();
        let qaqi = air_quality_supplement(qaqi_body).data.unwrap();
        assert_eq!(qaqi.code.as_deref(), Some("qaqi"));
        assert_eq!(qaqi.aqi_display, "0.9");

        let fallback_body: QWeatherAirQualityResponse = serde_json::from_value(serde_json::json!({
            "indexes": [
                { "name": "Unknown AQI", "aqiDisplay": "11" }
            ]
        }))
        .unwrap();
        let fallback = air_quality_supplement(fallback_body).data.unwrap();
        assert_eq!(fallback.name.as_deref(), Some("Unknown AQI"));
        assert_eq!(fallback.aqi_display, "11");
    }

    #[test]
    fn life_indices_supplement_parses_empty_and_error_codes() {
        let body: QWeatherIndicesResponse = serde_json::from_value(serde_json::json!({
            "code": "200",
            "daily": [
                { "date": "2026-06-12", "type": "1", "name": "运动指数", "level": "2", "category": "较适宜", "text": "适合适量运动。" },
                { "date": "2026-06-12", "type": "3", "name": "穿衣指数", "level": "6", "category": "热", "text": "建议短袖。" },
                { "date": "", "type": "5", "name": "紫外线指数", "category": "强" }
            ]
        }))
        .unwrap();

        let supplement = life_indices_supplement(body).unwrap();
        assert_eq!(supplement.status, WeatherSupplementStatus::Available);
        let indices = supplement.data.unwrap();
        assert_eq!(indices.len(), 2);
        assert_eq!(indices[0].name, "运动指数");

        let empty_body = QWeatherIndicesResponse {
            code: QWEATHER_EMPTY_CODE.to_owned(),
            daily: Vec::new(),
        };
        assert_eq!(
            life_indices_supplement(empty_body).unwrap().status,
            WeatherSupplementStatus::Empty
        );

        let err = life_indices_supplement(QWeatherIndicesResponse {
            code: "401".to_owned(),
            daily: Vec::new(),
        })
        .unwrap_err();
        assert_eq!(err.code, "http_error");
    }

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

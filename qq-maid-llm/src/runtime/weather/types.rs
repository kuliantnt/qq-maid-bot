use std::sync::Arc;

use async_trait::async_trait;

use crate::error::LlmError;

/// 默认预报天数。
pub const DEFAULT_FORECAST_DAYS: u8 = 3;

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

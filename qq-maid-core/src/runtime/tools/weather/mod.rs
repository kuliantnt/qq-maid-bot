//! 天气查询业务工具模块。
//!
//! 这里保留 `runtime::tools::weather` 的稳定导出面：
//! 对外公开天气领域模型、执行器 trait、默认配置 helper 和 QWeather 构建入口；
//! QWeather 的地点匹配、增强摘要和请求细节分别下沉到内部子模块。

mod qweather;
mod tool;
mod types;

pub use tool::WeatherTool;
pub(crate) use tool::{WEATHER_TOOL_NAME, route};

pub(crate) use qweather::{DEFAULT_QWEATHER_API_HOST, DEFAULT_QWEATHER_GEO_HOST};
pub use qweather::{
    QWeatherExecutor, build_weather_executor, default_qweather_api_host, default_qweather_geo_host,
    qweather_geo_host_from_api_host,
};
pub use types::{
    AirQualitySummary, CurrentWeather, DEFAULT_FORECAST_DAYS, DailyWeather, DynWeatherExecutor,
    WeatherAlert, WeatherExecutor, WeatherLifeIndex, WeatherLocation, WeatherOutcome,
    WeatherRequest, WeatherSupplement, WeatherSupplementStatus,
};

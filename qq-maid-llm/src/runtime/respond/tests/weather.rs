use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::support::*;
use crate::{error::LlmError, runtime::weather::WeatherSupplement};

#[tokio::test]
async fn weather_command_uses_weather_executor_and_returns_forecast() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let provider = MockProvider::with_counter(provider_calls.clone());
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let weather = MockWeatherExecutor::with_counter(weather_calls.clone());
    let inspector = weather.clone();
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        provider,
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(weather),
        None,
        None,
        None,
    );

    let response = service.respond(message("/天气杭州")).await.unwrap();
    let text = response.text.clone().unwrap();
    let markdown = response.markdown.clone().unwrap();

    assert_eq!(response.command.as_deref(), Some("weather"));
    assert!(text.starts_with("🌦 杭州天气"));
    assert!(text.contains("当前 20:15｜"));
    assert!(text.contains("体感 28.5°C · 湿度 86% · 东北风 3级"));
    assert!(text.contains("⚠️ 预警"));
    assert!(text.contains("· 🔵 大风蓝色预警"));
    assert!(text.contains("· 🟡 雷电黄色预警"));
    assert!(text.contains("📅 未来 3 天"));
    assert!(text.contains("· 今天 周五：多云转阴，21～32.5°C，东风 1-3级"));
    assert!(text.contains("· 明天 周六：小雨，22.2～26°C，东北风 3级"));
    assert!(text.contains("· 后天 周日：毛毛雨转阴，21.3～26.6°C，东风 1-3级"));
    assert!(!text.contains("第三条预警不应进入回复"));
    assert!(text.contains("空气质量：AQI 42（优） · 首要污染物 PM2.5"));
    assert!(text.contains("🧭 生活指数"));
    assert!(text.contains("运动：较适宜｜穿衣：热｜紫外线：强"));
    assert!(text.contains("数据来源：和风天气"));
    assert!(!text.contains("\n| "));
    assert!(markdown.starts_with("# 🌦 杭州天气"));
    assert!(markdown.contains("**当前 20:15｜"));
    assert!(markdown.contains("## ⚠️ 预警"));
    assert!(markdown.contains("- 🔵 **大风蓝色预警**"));
    assert!(markdown.contains("## 📅 未来 3 天"));
    assert!(markdown.contains("- **今天 周五**：多云转阴，21～32.5°C，东风 1-3级"));
    assert!(markdown.contains("空气质量：**AQI 42（优）** · 首要污染物 PM2.5"));
    assert!(markdown.contains("## 🧭 生活指数"));
    assert!(markdown.contains("> 数据来源：和风天气"));
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    assert_eq!(weather_calls.load(Ordering::SeqCst), 1);

    let requests = inspector.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].city, "杭州");
    assert_eq!(requests[0].forecast_days, 3);
    let diagnostics = response.diagnostics.unwrap();
    assert!(diagnostics["used_weather"].as_bool().unwrap());
    assert_eq!(diagnostics["weather_provider"], "mock-weather");
    assert_eq!(diagnostics["original_city"], "杭州");
    assert_eq!(diagnostics["resolved_name"], "杭州");
    assert_eq!(diagnostics["resolved_adm1"], "浙江");
    assert_eq!(diagnostics["resolved_adm2"], "杭州");
    assert_eq!(diagnostics["weather_elapsed_ms"], 7);
    assert_eq!(diagnostics["weather_alert_status"], "data");
    assert_eq!(diagnostics["weather_alert_count"], 3);
    assert_eq!(diagnostics["weather_air_quality_status"], "data");
    assert_eq!(diagnostics["weather_air_quality_count"], 1);
    assert_eq!(diagnostics["weather_life_indices_status"], "data");
    assert_eq!(diagnostics["weather_life_indices_count"], 4);
}

#[tokio::test]
async fn weather_command_trims_city_without_normalizing_alias() {
    let weather = MockWeatherExecutor::new();
    let inspector = weather.clone();
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(weather),
        None,
        None,
        None,
    );

    service.respond(message("/天气 温州 ")).await.unwrap();

    let requests = inspector.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].city, "温州");
}

#[tokio::test]
async fn weather_command_accepts_city_weather_suffix() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let weather = MockWeatherExecutor::new();
    let inspector = weather.clone();
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        MockProvider::with_counter(provider_calls.clone()),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(weather),
        None,
        None,
        None,
    );

    let response = service.respond(message("/杭州天气")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("weather"));
    assert!(response.text.unwrap().starts_with("🌦 杭州天气"));
    assert!(response.markdown.unwrap().starts_with("# 🌦 杭州天气"));
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);

    let requests = inspector.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].city, "杭州");
    assert_eq!(requests[0].forecast_days, 3);
}

#[tokio::test]
async fn weather_command_ignores_plain_city_weather_suffix() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        MockProvider::with_counter(provider_calls.clone()),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::with_counter(weather_calls.clone())),
        None,
        None,
        None,
    );

    let response = service.respond(message("杭州天气")).await.unwrap();

    assert!(response.text.unwrap().contains("回复：杭州天气"));
    assert_eq!(provider_calls.load(Ordering::SeqCst), 1);
    assert_eq!(weather_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn weather_command_accepts_spaced_city_and_reports_error() {
    let weather = FailingWeatherExecutor {
        err: LlmError::timeout("weather"),
    };
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(weather),
        None,
        None,
        None,
    );

    let response = service.respond(message("/天气 杭州")).await.unwrap();
    let text = response.text.clone().unwrap();

    assert!(text.contains("天气服务超时"));
    assert!(
        response
            .markdown
            .as_deref()
            .is_some_and(|markdown| markdown.contains("天气服务超时"))
    );
    assert_eq!(response.command.as_deref(), Some("weather"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["weather_provider"], "mock-weather");
    assert_eq!(diagnostics["weather_error_code"], "timeout");
    assert_eq!(diagnostics["weather_error_stage"], "weather");
    assert_eq!(diagnostics["forecast_days"], 3);
}

#[tokio::test]
async fn weather_command_keeps_forecast_when_supplements_fail_or_empty() {
    let weather = SupplementWeatherExecutor {
        alerts: WeatherSupplement::failed(&LlmError::http("alert failed")),
        air_quality: WeatherSupplement::empty(Some(true)),
        life_indices: WeatherSupplement::failed(&LlmError::provider("bad indices", "json")),
    };
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(weather),
        None,
        None,
        None,
    );

    let response = service.respond(message("/天气 杭州")).await.unwrap();
    let text = response.text.clone().unwrap();
    let markdown = response.markdown.clone().unwrap();

    assert!(text.starts_with("🌦 杭州天气"));
    assert!(text.contains("当前 20:15｜"));
    assert!(text.contains("📅 未来 1 天"));
    assert!(!text.contains("天气服务暂时不可用"));
    assert!(!text.contains("⚠️ 预警"));
    assert!(!text.contains("空气质量："));
    assert!(!text.contains("🧭 生活指数"));
    assert!(markdown.starts_with("# 🌦 杭州天气"));
    assert!(markdown.contains("**当前 20:15｜"));
    assert!(markdown.contains("## 📅 未来 1 天"));
    assert!(!markdown.contains("## ⚠️ 预警"));
    assert!(!markdown.contains("空气质量："));
    assert!(!markdown.contains("## 🧭 生活指数"));

    let diagnostics = response.diagnostics.unwrap();
    assert!(diagnostics["weather_error_code"].is_null());
    assert_eq!(diagnostics["weather_alert_status"], "error");
    assert_eq!(diagnostics["weather_alert_error_code"], "http_error");
    assert_eq!(diagnostics["weather_air_quality_status"], "empty");
    assert_eq!(diagnostics["weather_air_quality_count"], 0);
    assert_eq!(diagnostics["weather_air_quality_zero_result"], true);
    assert_eq!(diagnostics["weather_life_indices_status"], "error");
    assert_eq!(diagnostics["weather_life_indices_error_stage"], "json");
}

#[tokio::test]
async fn weather_command_requires_city() {
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let weather = MockWeatherExecutor::with_counter(weather_calls.clone());
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(weather),
        None,
        None,
        None,
    );

    let response = service.respond(message("/天气")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("weather"));
    assert!(response.text.unwrap().contains("用法：/天气城市名"));
    assert_eq!(weather_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn weather_command_accepts_english_alias() {
    let weather = MockWeatherExecutor::new();
    let inspector = weather.clone();
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(weather),
        None,
        None,
        None,
    );

    let response = service.respond(message("/weather Hangzhou")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("weather"));
    assert!(response.text.unwrap().starts_with("🌦 杭州天气"));
    assert!(response.markdown.unwrap().starts_with("# 🌦 杭州天气"));

    let requests = inspector.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].city, "Hangzhou");
}

#[tokio::test]
async fn weather_command_rejects_too_long_city_without_calling_executor() {
    let weather_calls = Arc::new(AtomicUsize::new(0));
    let weather = MockWeatherExecutor::with_counter(weather_calls.clone());
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(weather),
        None,
        None,
        None,
    );

    let city = "杭".repeat(61);
    let response = service
        .respond(message(&format!("/天气{city}")))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("weather"));
    assert!(response.text.unwrap().contains("城市名太长了"));
    assert_eq!(weather_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn weather_command_maps_not_found_error_to_reply_and_diagnostics() {
    let weather = FailingWeatherExecutor {
        err: LlmError::new("not_found", "city not found", "weather"),
    };
    let (service, _) = test_service_with_provider_base_title_query_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(weather),
        None,
        None,
        None,
    );

    let response = service.respond(message("/天气 不存在")).await.unwrap();
    let text = response.text.clone().unwrap();

    assert!(text.contains("没找到这个城市"));
    assert!(
        response
            .markdown
            .as_deref()
            .is_some_and(|markdown| markdown.contains("没找到这个城市"))
    );
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["weather_provider"], "mock-weather");
    assert_eq!(diagnostics["weather_error_code"], "not_found");
    assert_eq!(diagnostics["weather_error_stage"], "weather");
}

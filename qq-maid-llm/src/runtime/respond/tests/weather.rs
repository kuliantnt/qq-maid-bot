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
    let text = response.text.unwrap();

    assert_eq!(response.command.as_deref(), Some("weather"));
    assert!(text.contains("【天气】"));
    assert!(text.contains("当前"));
    assert!(text.contains("今天起 3 天"));
    assert!(text.contains("06-12（五）"));
    assert!(text.contains("06-13（六）"));
    assert!(text.contains("06-14（日）"));
    assert!(text.contains("预警：杭州市气象台发布大风蓝色预警"));
    assert!(text.contains("预警：杭州市气象台发布雷电黄色预警"));
    assert!(!text.contains("第三条预警不应进入回复"));
    assert!(text.contains("空气：AQI（CN） 42（优），首要污染物 PM2.5"));
    assert!(text.contains("生活指数：运动 较适宜；穿衣 热；紫外线 强"));
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    assert_eq!(weather_calls.load(Ordering::SeqCst), 1);

    let requests = inspector.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].city, "杭州");
    assert_eq!(requests[0].forecast_days, 3);
    let diagnostics = response.diagnostics.unwrap();
    assert!(diagnostics["used_weather"].as_bool().unwrap());
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
    assert!(response.text.unwrap().contains("【天气】"));
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
    let text = response.text.unwrap();

    assert!(text.contains("天气服务超时"));
    assert_eq!(response.command.as_deref(), Some("weather"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["weather_error_code"], "timeout");
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
    let text = response.text.unwrap();

    assert!(text.contains("当前"));
    assert!(text.contains("今天起 3 天"));
    assert!(!text.contains("天气服务暂时不可用"));
    assert!(!text.contains("预警："));
    assert!(!text.contains("空气："));
    assert!(!text.contains("生活指数："));

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

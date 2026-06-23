use std::sync::Arc;

use chrono::NaiveDate;

use super::support::*;
use crate::error::LlmError;

#[tokio::test]
async fn train_command_defaults_to_today_and_uses_executor() {
    let train = MockTrainExecutor::new();
    let inspector = train.clone();
    let (service, _) = test_service_with_provider_base_title_query_weather_train_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(train),
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );

    let response = service.respond(message("/火车 G1")).await.unwrap();
    let text = response.text.as_deref().unwrap();
    let markdown = response.markdown.as_deref().unwrap();

    assert_eq!(response.command.as_deref(), Some("train"));
    assert!(text.contains("🚄 G1 列车时刻"));
    assert!(text.contains("日期："));
    assert!(text.contains("行程：北京南 → 上海虹桥"));
    assert!(text.contains("站序 / 车站 / 到达 / 出发 / 停留"));
    assert!(text.contains("1 / 北京南 / --:-- / 06:30 / --"));
    assert!(text.contains("2 / 南京南 / 10:13 / 10:15 / 2 分钟"));
    assert!(markdown.contains("| 站序 | 车站 | 到达 | 出发 | 停留 |"));
    assert!(markdown.contains("> 时刻信息来自铁路查询服务"));

    let requests = inspector.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].train_code, "G1");
    let today = crate::util::time_context::request_time_context().local_date();
    assert_eq!(requests[0].travel_date, today);

    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["used_train"], true);
    assert_eq!(diagnostics["train_provider"], "mock-train");
    assert_eq!(diagnostics["date_provided"], false);
}

#[tokio::test]
async fn train_command_accepts_explicit_relative_date() {
    let train = MockTrainExecutor::new();
    let inspector = train.clone();
    let (service, _) = test_service_with_provider_base_title_query_weather_train_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(train),
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );

    let response = service.respond(message("/火车 d1234 明天")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("train"));
    let requests = inspector.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].train_code, "D1234");
    assert_eq!(
        requests[0].travel_date,
        crate::util::time_context::request_time_context().local_date() + chrono::Duration::days(1)
    );
}

#[tokio::test]
async fn train_command_requires_code() {
    let response = test_service().respond(message("/火车")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("train"));
    assert_eq!(
        response.text.as_deref(),
        Some("请先提供车次，例如：/火车 G1")
    );
}

#[tokio::test]
async fn train_command_rejects_invalid_date() {
    let response = test_service()
        .respond(message("/火车 G1 下周一"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("train"));
    assert_eq!(
        response.text.as_deref(),
        Some("日期暂时只支持 今天、明天、后天、YYYY-MM-DD、YYYY年M月D日 或 M月D日。")
    );
}

#[tokio::test]
async fn train_command_surfaces_no_schedule_error() {
    let (service, _) = test_service_with_provider_base_title_query_weather_train_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(FailingTrainExecutor {
            err: LlmError::new(
                "no_schedule",
                "no train schedule found for the requested date",
                "train",
            ),
        }),
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );

    let response = service
        .respond(message("/火车 G1 2026-06-28"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("train"));
    assert_eq!(response.text.as_deref(), Some("该日期未查询到开行信息。"));
}

#[tokio::test]
async fn train_command_surfaces_timeout_error() {
    let (service, _) = test_service_with_provider_base_title_query_weather_train_and_models(
        MockProvider::new(),
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(FailingTrainExecutor {
            err: LlmError::timeout("train"),
        }),
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );

    let response = service.respond(message("/火车 G1")).await.unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("铁路时刻服务超时"));
    let diagnostics = response.diagnostics.unwrap();
    assert_eq!(diagnostics["train_error_code"], "timeout");
    assert_eq!(diagnostics["train_error_stage"], "train");
}

#[test]
fn train_api_response_parses_cross_day_stop() {
    let schedule = crate::runtime::train::TrainSchedule {
        train_code: "1461".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
        start_station: "北京".to_owned(),
        end_station: "上海".to_owned(),
        stops: vec![crate::runtime::train::TrainStop {
            station_no: 16,
            station_name: "蚌埠".to_owned(),
            arrive_time: Some("00:47".to_owned()),
            departure_time: Some("00:51".to_owned()),
            stopover_minutes: Some(4),
            day_difference: 1,
            station_train_code: "1461".to_owned(),
        }],
    };

    let rendered = super::super::train_flow::format_train_schedule_reply(&schedule);
    assert!(
        rendered
            .text
            .contains("16 / 蚌埠（+1天） / 00:47 / 00:51 / 4 分钟")
    );
}

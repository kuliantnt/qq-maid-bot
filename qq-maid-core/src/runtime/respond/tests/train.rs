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
        Arc::new(MockWebSearchExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(train),
        TestModelOptions {
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
    // 始发站：到达 --，出发取实际发车时间，停留显示始发。
    assert!(text.contains("1 / 北京南 / -- / 06:30 / 始发"));
    // 中间站：到发时间和停留分钟数保持原逻辑。
    assert!(text.contains("2 / 南京南 / 10:13 / 10:15 / 2 分钟"));
    // 终到站：到达取实际到达时间，出发 --，停留显示终到。
    assert!(text.contains("3 / 上海虹桥 / 11:24 / -- / 终到"));
    assert!(markdown.contains("| 站序 | 车站 | 到达 | 出发 | 停留 |"));
    assert!(markdown.contains("| 1 | 北京南 | -- | 06:30 | 始发 |"));
    assert!(markdown.contains("| 3 | 上海虹桥 | 11:24 | -- | 终到 |"));
    assert!(markdown.contains("> 当前展示为当日计划时刻"));

    let requests = inspector.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].train_code, "G1");
    let today = qq_maid_common::time_context::request_time_context().local_date();
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
        Arc::new(MockWebSearchExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(train),
        TestModelOptions {
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
        qq_maid_common::time_context::request_time_context().local_date()
            + chrono::Duration::days(1)
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
        Arc::new(MockWebSearchExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(FailingTrainExecutor {
            err: LlmError::new(
                "no_schedule",
                "no train schedule found for the requested date",
                "train",
            ),
        }),
        TestModelOptions {
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
        Arc::new(MockWebSearchExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(FailingTrainExecutor {
            err: LlmError::timeout("train"),
        }),
        TestModelOptions {
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
    // 使用三站时刻表，让跨日站落在中间站位置，既覆盖 (+N天) 后缀渲染，
    // 也保持中间站停留分钟数原逻辑。
    let schedule = crate::runtime::tools::train::TrainSchedule {
        train_code: "1461".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
        start_station: "北京".to_owned(),
        end_station: "上海".to_owned(),
        stops: vec![
            crate::runtime::tools::train::TrainStop {
                station_no: 1,
                station_name: "北京".to_owned(),
                arrive_time: None,
                departure_time: Some("21:17".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                day_difference_reliable: true,
                station_train_code: "1461".to_owned(),
            },
            crate::runtime::tools::train::TrainStop {
                station_no: 16,
                station_name: "蚌埠".to_owned(),
                arrive_time: Some("00:47".to_owned()),
                departure_time: Some("00:51".to_owned()),
                stopover_minutes: Some(4),
                day_difference: 1,
                day_difference_reliable: true,
                station_train_code: "1461".to_owned(),
            },
            crate::runtime::tools::train::TrainStop {
                station_no: 25,
                station_name: "上海".to_owned(),
                arrive_time: Some("08:00".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 1,
                day_difference_reliable: true,
                station_train_code: "1461".to_owned(),
            },
        ],
        full_train_code: None,
        corporation: None,
        train_style: None,
        dept_train: None,
    };

    let rendered = super::super::train_flow::format_train_schedule_reply(&schedule);
    assert!(
        rendered
            .text
            .contains("16 / 蚌埠（+1天） / 00:47 / 00:51 / 4 分钟")
    );
}

#[test]
fn train_schedule_renders_single_stop_without_origin_terminal_marks() {
    // 接口意外只返回一个站点时，不应同时硬标为始发和终到，
    // 保留原始到发数据，停留显示 --。
    let schedule = crate::runtime::tools::train::TrainSchedule {
        train_code: "G1".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 25).unwrap(),
        start_station: "北京南".to_owned(),
        end_station: "上海虹桥".to_owned(),
        stops: vec![crate::runtime::tools::train::TrainStop {
            station_no: 1,
            station_name: "北京南".to_owned(),
            arrive_time: Some("06:30".to_owned()),
            departure_time: Some("06:30".to_owned()),
            stopover_minutes: Some(0),
            day_difference: 0,
            day_difference_reliable: true,
            station_train_code: "G1".to_owned(),
        }],
        full_train_code: None,
        corporation: None,
        train_style: None,
        dept_train: None,
    };

    let rendered = super::super::train_flow::format_train_schedule_reply(&schedule);
    assert!(rendered.text.contains("1 / 北京南 / 06:30 / 06:30 / --"));
    assert!(!rendered.text.contains("始发"));
    assert!(!rendered.text.contains("终到"));
}

#[test]
fn train_schedule_renders_optional_info_when_present() {
    // 12306 返回了完整车次、担当客运段、车型信息、配属时，时刻表应展示对应行。
    let schedule = crate::runtime::tools::train::TrainSchedule {
        train_code: "D3233".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 25).unwrap(),
        start_station: "杭州东".to_owned(),
        end_station: "厦门".to_owned(),
        stops: vec![
            crate::runtime::tools::train::TrainStop {
                station_no: 1,
                station_name: "杭州东".to_owned(),
                arrive_time: None,
                departure_time: Some("14:32".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                day_difference_reliable: true,
                station_train_code: "D3233".to_owned(),
            },
            crate::runtime::tools::train::TrainStop {
                station_no: 17,
                station_name: "厦门".to_owned(),
                arrive_time: Some("21:58".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 0,
                day_difference_reliable: true,
                station_train_code: "D3233".to_owned(),
            },
        ],
        full_train_code: Some("D3233/D3234".to_owned()),
        corporation: Some("南昌客运段".to_owned()),
        train_style: Some("CRH2A".to_owned()),
        dept_train: Some("南昌车辆段".to_owned()),
    };

    let rendered = super::super::train_flow::format_train_schedule_reply(&schedule);
    assert!(rendered.text.contains("完整车次：D3233/D3234"));
    assert!(rendered.text.contains("担当客运段：南昌客运段"));
    assert!(rendered.text.contains("车型信息：CRH2A"));
    assert!(rendered.text.contains("配属：南昌车辆段"));
    let markdown = rendered.markdown.as_deref().unwrap_or("");
    assert!(markdown.contains("**完整车次：** D3233/D3234"));
    assert!(markdown.contains("**担当客运段：** 南昌客运段"));
}

#[test]
fn train_schedule_omits_optional_info_when_absent() {
    // 12306 未返回可选字段时，时刻表不应出现对应行。
    let schedule = crate::runtime::tools::train::TrainSchedule {
        train_code: "G1".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 25).unwrap(),
        start_station: "北京南".to_owned(),
        end_station: "上海虹桥".to_owned(),
        stops: vec![
            crate::runtime::tools::train::TrainStop {
                station_no: 1,
                station_name: "北京南".to_owned(),
                arrive_time: None,
                departure_time: Some("06:30".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                day_difference_reliable: true,
                station_train_code: "G1".to_owned(),
            },
            crate::runtime::tools::train::TrainStop {
                station_no: 2,
                station_name: "上海虹桥".to_owned(),
                arrive_time: Some("11:24".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 0,
                day_difference_reliable: true,
                station_train_code: "G1".to_owned(),
            },
        ],
        full_train_code: None,
        corporation: None,
        train_style: None,
        dept_train: None,
    };

    let rendered = super::super::train_flow::format_train_schedule_reply(&schedule);
    assert!(!rendered.text.contains("完整车次"));
    assert!(!rendered.text.contains("担当客运段"));
    assert!(!rendered.text.contains("车型信息"));
    assert!(!rendered.text.contains("配属"));
    let markdown = rendered.markdown.as_deref().unwrap_or("");
    assert!(!markdown.contains("完整车次"));
    assert!(!markdown.contains("担当客运段"));
}

//! 火车行程 Todo 子流程测试。
//!
//! 覆盖：
//! - 结构化输入识别为火车行程并创建 Todo；
//! - 自然语言输入识别；
//! - 跨日行程时间计算；
//! - 出发站不存在、到达站不存在、站点顺序错误；
//! - 12306 接口超时/异常；
//! - 缺少日期字段；
//! - 普通 Todo 输入不被误识别为火车行程；
//! - 座位号、站台为空时正确展示；
//! - 确认/取消 pending 行为。

use std::sync::Arc;

use chrono::NaiveDate;

use super::support::*;
use crate::{
    error::LlmError,
    runtime::{
        respond::RustRespondService,
        todo::{TodoStore, TodoTimePrecision},
        train::{TrainSchedule, TrainStop},
    },
};

/// 构造 G34 杭州东→北京南 的固定时刻表。
fn g34_schedule() -> TrainSchedule {
    TrainSchedule {
        train_code: "G34".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
        start_station: "杭州东".to_owned(),
        end_station: "北京南".to_owned(),
        stops: vec![
            TrainStop {
                station_no: 1,
                station_name: "杭州东".to_owned(),
                arrive_time: None,
                departure_time: Some("07:05".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                station_train_code: "G34".to_owned(),
            },
            TrainStop {
                station_no: 2,
                station_name: "南京南".to_owned(),
                arrive_time: Some("09:20".to_owned()),
                departure_time: Some("09:22".to_owned()),
                stopover_minutes: Some(2),
                day_difference: 0,
                station_train_code: "G34".to_owned(),
            },
            TrainStop {
                station_no: 3,
                station_name: "北京南".to_owned(),
                arrive_time: Some("11:40".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 0,
                station_train_code: "G34".to_owned(),
            },
        ],
    }
}

/// 构造 Z281 杭州→西安 跨日时刻表。
fn z281_schedule() -> TrainSchedule {
    TrainSchedule {
        train_code: "Z281".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
        start_station: "杭州".to_owned(),
        end_station: "西安".to_owned(),
        stops: vec![
            TrainStop {
                station_no: 1,
                station_name: "杭州".to_owned(),
                arrive_time: None,
                departure_time: Some("23:40".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                station_train_code: "Z281".to_owned(),
            },
            TrainStop {
                station_no: 5,
                station_name: "西安".to_owned(),
                arrive_time: Some("08:15".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 1,
                station_train_code: "Z281".to_owned(),
            },
        ],
    }
}

/// 构造 K20 始发日次日中途上车的时刻表。
fn k20_midway_next_day_schedule() -> TrainSchedule {
    TrainSchedule {
        train_code: "K20".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
        start_station: "始发站".to_owned(),
        end_station: "终到站".to_owned(),
        stops: vec![
            TrainStop {
                station_no: 1,
                station_name: "始发站".to_owned(),
                arrive_time: None,
                departure_time: Some("22:00".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                station_train_code: "K20".to_owned(),
            },
            TrainStop {
                station_no: 5,
                station_name: "中途站".to_owned(),
                arrive_time: Some("07:50".to_owned()),
                departure_time: Some("08:00".to_owned()),
                stopover_minutes: Some(10),
                day_difference: 1,
                station_train_code: "K20".to_owned(),
            },
            TrainStop {
                station_no: 8,
                station_name: "终到站".to_owned(),
                arrive_time: Some("12:30".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 1,
                station_train_code: "K20".to_owned(),
            },
        ],
    }
}

/// 构造纯数字车次 1461 北京→上海 的固定时刻表。
fn train_1461_schedule() -> TrainSchedule {
    TrainSchedule {
        train_code: "1461".to_owned(),
        travel_date: NaiveDate::from_ymd_opt(2026, 6, 24).unwrap(),
        start_station: "北京".to_owned(),
        end_station: "上海".to_owned(),
        stops: vec![
            TrainStop {
                station_no: 1,
                station_name: "北京".to_owned(),
                arrive_time: None,
                departure_time: Some("16:00".to_owned()),
                stopover_minutes: None,
                day_difference: 0,
                station_train_code: "1461".to_owned(),
            },
            TrainStop {
                station_no: 16,
                station_name: "蚌埠".to_owned(),
                arrive_time: Some("00:47".to_owned()),
                departure_time: Some("00:51".to_owned()),
                stopover_minutes: Some(4),
                day_difference: 1,
                station_train_code: "1461".to_owned(),
            },
            TrainStop {
                station_no: 28,
                station_name: "上海".to_owned(),
                arrive_time: Some("08:10".to_owned()),
                departure_time: None,
                stopover_minutes: None,
                day_difference: 1,
                station_train_code: "1461".to_owned(),
            },
        ],
    }
}

fn service_with_seeded_train(executor: Arc<SeededTrainExecutor>) -> RustRespondService {
    build_service_with_seeded_train(executor).1
}

fn build_service_with_seeded_train(
    executor: Arc<SeededTrainExecutor>,
) -> (std::path::PathBuf, RustRespondService) {
    let provider = MockProvider::new();
    let (service, base) = test_service_with_provider_base_title_query_weather_train_and_models(
        provider,
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        executor,
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );
    (base, service)
}

#[tokio::test]
async fn train_todo_add_structured_input_creates_pending_draft() {
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G34", g34_schedule()));
    let inspector = Arc::clone(&executor);
    let service = service_with_seeded_train(executor);

    let response = service
        .respond(message("/todo add G34 杭州东 北京南 明天 05车12A 8站台"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_train_add"));
    let text = response.text.as_deref().unwrap();
    let markdown = response.markdown.as_deref().unwrap();
    assert!(text.contains("待确认新增待办"));
    assert!(text.contains("标题：🚄 G34 杭州东 → 北京南"));
    assert!(text.contains("出发：2026-06-24 07:05"));
    assert!(text.contains("到达：2026-06-24 11:40"));
    assert!(text.contains("座位：05车12A"));
    assert!(text.contains("站台：8站台"));
    assert!(markdown.contains("# 待确认新增待办"));

    // 确认 12306 被调用一次，车次为大写 G34。
    let requests = inspector.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].train_code, "G34");

    // 确认前不应写入 Todo。
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    assert!(service.todo_store.list_pending(&owner).unwrap().is_empty());

    // 确认后写入 Todo。
    let confirmed = service.respond(message("确认")).await.unwrap();
    let confirmed_text = confirmed.text.as_deref().unwrap();
    assert!(confirmed_text.contains("已新增待办"));
    assert!(confirmed_text.contains("G34"));
    assert!(confirmed_text.contains("杭州东 → 北京南"));

    let items = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert!(item.title.contains("G34"));
    assert!(item.title.contains("杭州东"));
    assert!(item.title.contains("北京南"));
    assert_eq!(item.due_at.as_deref(), Some("2026-06-24 07:05:00"));
    assert_eq!(item.time_precision, TodoTimePrecision::DateTime);
    assert!(item.detail.as_deref().unwrap().contains("座位：05车12A"));
    assert!(item.detail.as_deref().unwrap().contains("站台：8站台"));
    assert_eq!(
        item.raw_text.as_deref(),
        Some("G34 杭州东 北京南 明天 05车12A 8站台")
    );

    // 重复确认时 pending 已清空，不应重复创建火车 Todo。
    service.respond(message("确认")).await.unwrap();
    let items = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(items.len(), 1);
}

#[tokio::test]
async fn train_todo_add_natural_language_creates_draft() {
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G34", g34_schedule()));
    let service = service_with_seeded_train(executor);

    let response = service
        .respond(message("/todo add 明天坐 G34 从杭州东去北京南"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_train_add"));
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("标题：🚄 G34 杭州东 → 北京南"));
    // 自然语言输入没有座位和站台。
    assert!(!text.contains("座位"));
    assert!(!text.contains("站台"));
}

#[tokio::test]
async fn obvious_train_todo_add_uses_train_parse_and_train_executor_once() {
    let provider = MockProvider::new();
    let provider_inspector = provider.clone();
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G34", g34_schedule()));
    let train_inspector = Arc::clone(&executor);
    let (service, _base) = test_service_with_provider_base_title_query_weather_train_and_models(
        provider,
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        executor,
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );

    let response = service
        .respond(message("/todo add G34 杭州东 北京南 明天"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_train_add"));

    let llm_requests = provider_inspector.requests();
    assert_eq!(llm_requests.len(), 1);
    assert_eq!(
        llm_requests[0]
            .metadata
            .get("todo_operation")
            .map(String::as_str),
        Some("train_add")
    );
    assert_eq!(train_inspector.requests().len(), 1);
}

#[tokio::test]
async fn numeric_train_todo_add_uses_train_parse_and_train_executor_once() {
    let provider = MockProvider::new();
    let provider_inspector = provider.clone();
    let executor =
        Arc::new(SeededTrainExecutor::new().with_schedule("1461", train_1461_schedule()));
    let train_inspector = Arc::clone(&executor);
    let (service, _base) = test_service_with_provider_base_title_query_weather_train_and_models(
        provider,
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        executor,
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );

    let response = service
        .respond(message("/todo add 1461 北京 上海 明天"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_train_add"));

    let llm_requests = provider_inspector.requests();
    assert_eq!(llm_requests.len(), 1);
    assert_eq!(
        llm_requests[0]
            .metadata
            .get("todo_operation")
            .map(String::as_str),
        Some("train_add")
    );
    let train_requests = train_inspector.requests();
    assert_eq!(train_requests.len(), 1);
    assert_eq!(train_requests[0].train_code, "1461");
}

#[tokio::test]
async fn train_todo_add_cross_day_trip_computes_arrival_date() {
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("Z281", z281_schedule()));
    let service = service_with_seeded_train(executor);

    let response = service
        .respond(message("/todo add Z281 杭州 西安 2026-06-24"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_train_add"));
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("出发：2026-06-24 23:40"));
    // 跨日到达应为 2026-06-25。
    assert!(text.contains("到达：2026-06-25 08:15"));

    // 确认后 Todo 的 due_at 应为出发时间。
    service.respond(message("确认")).await.unwrap();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let items = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(items[0].due_at.as_deref(), Some("2026-06-24 23:40:00"));
}

#[tokio::test]
async fn train_todo_add_midway_next_day_uses_boarding_date_not_start_day() {
    let executor =
        Arc::new(SeededTrainExecutor::new().with_schedule("K20", k20_midway_next_day_schedule()));
    let inspector = Arc::clone(&executor);
    let service = service_with_seeded_train(executor);

    let response = service
        .respond(message("/todo add K20 中途站 终到站 2026-06-25"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_train_add"));
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("出发：2026-06-25 08:00"));
    assert!(text.contains("到达：2026-06-25 12:30"));

    let requests = inspector.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].travel_date,
        NaiveDate::from_ymd_opt(2026, 6, 25).unwrap()
    );
    assert_eq!(
        requests[1].travel_date,
        NaiveDate::from_ymd_opt(2026, 6, 24).unwrap()
    );

    service.respond(message("确认")).await.unwrap();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let items = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(items[0].due_at.as_deref(), Some("2026-06-25 08:00:00"));
}

#[tokio::test]
async fn train_todo_add_rejects_same_start_and_arrival_station() {
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G34", g34_schedule()));
    let service = service_with_seeded_train(executor);

    let response = service
        .respond(message("/todo add G34 杭州东 杭州东 明天"))
        .await
        .unwrap();
    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("出发站和到达站不能相同")
    );
}

#[tokio::test]
async fn train_todo_add_rejects_same_middle_and_arrival_station() {
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G34", g34_schedule()));
    let service = service_with_seeded_train(executor);

    let response = service
        .respond(message("/todo add G34 南京南 南京南 明天"))
        .await
        .unwrap();
    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("出发站和到达站不能相同")
    );
}

#[tokio::test]
async fn train_todo_add_invalid_time_does_not_create_pending_or_todo() {
    let mut schedule = g34_schedule();
    schedule.stops[0].departure_time = Some("25:99".to_owned());
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G34", schedule));
    let service = service_with_seeded_train(executor);

    let response = service
        .respond(message("/todo add G34 杭州东 北京南 明天"))
        .await
        .unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("时间字段异常"));
    assert!(text.contains("25:99"));

    let owner = TodoStore::owner(Some("u1"), "group:g1");
    assert!(service.todo_store.list_pending(&owner).unwrap().is_empty());
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
}

#[tokio::test]
async fn train_todo_add_hhmmss_time_keeps_seconds() {
    let mut schedule = g34_schedule();
    schedule.stops[0].departure_time = Some("07:05:30".to_owned());
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G34", schedule));
    let service = service_with_seeded_train(executor);

    service
        .respond(message("/todo add G34 杭州东 北京南 明天"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let items = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(items[0].due_at.as_deref(), Some("2026-06-24 07:05:30"));
}

#[tokio::test]
async fn train_todo_add_rejects_missing_from_station() {
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G50", g34_schedule()));
    let service = service_with_seeded_train(executor);

    // G50 的 from_station 是“上海”，不在 G34 时刻表中。
    let response = service
        .respond(message("/todo add G50 上海 北京南 2026-06-24"))
        .await
        .unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("未找到“上海”站"));
    // 不应创建 pending。
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    assert!(service.todo_store.list_pending(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn train_todo_add_rejects_reversed_station_order() {
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G88", g34_schedule()));
    let service = service_with_seeded_train(executor);

    // G88 的 from_station 是“北京南”，to_station 是“杭州东”，顺序与 G34 时刻表相反。
    let response = service
        .respond(message("/todo add G88 北京南 杭州东 2026-06-24"))
        .await
        .unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("运行方向"));
    assert!(text.contains("北京南"));
    assert!(text.contains("杭州东"));
}

#[tokio::test]
async fn train_todo_add_surfaces_timeout_error() {
    let executor =
        Arc::new(SeededTrainExecutor::new().with_failing("G34", LlmError::timeout("train")));
    let service = service_with_seeded_train(executor);

    let response = service
        .respond(message("/todo add G34 杭州东 北京南 明天"))
        .await
        .unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("铁路时刻服务暂时不可用"));
    assert!(text.contains("没有创建 Todo"));
}

#[tokio::test]
async fn train_todo_add_surfaces_no_schedule_error() {
    let executor = Arc::new(SeededTrainExecutor::new().with_failing(
        "G34",
        LlmError::new(
            "no_schedule",
            "no train schedule found for the requested date",
            "train",
        ),
    ));
    let service = service_with_seeded_train(executor);

    let response = service
        .respond(message("/todo add G34 杭州东 北京南 明天"))
        .await
        .unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("该日期未查询到开行信息"));
}

#[tokio::test]
async fn train_todo_add_rejects_missing_date() {
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G99", g34_schedule()));
    let service = service_with_seeded_train(executor);

    // G99 的 travel_date 为 null。
    let response = service
        .respond(message("/todo add G99 杭州东 北京南"))
        .await
        .unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("乘车日期"));
}

#[tokio::test]
async fn train_todo_add_cancel_does_not_write() {
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G34", g34_schedule()));
    let service = service_with_seeded_train(executor);

    service
        .respond(message("/todo add G34 杭州东 北京南 明天"))
        .await
        .unwrap();
    let cancelled = service.respond(message("取消")).await.unwrap();
    assert!(cancelled.text.as_deref().unwrap().contains("已取消"));

    let owner = TodoStore::owner(Some("u1"), "group:g1");
    assert!(service.todo_store.list_pending(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn train_todo_add_pending_wait_reply() {
    let executor = Arc::new(SeededTrainExecutor::new().with_schedule("G34", g34_schedule()));
    let service = service_with_seeded_train(executor);

    service
        .respond(message("/todo add G34 杭州东 北京南 明天"))
        .await
        .unwrap();
    // 非确认非取消的回复应提示等待。
    let response = service.respond(message("随便说点什么")).await.unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("已完成外部数据校验"));
    assert!(text.contains("暂不支持直接修改"));
}

#[tokio::test]
async fn train_todo_add_invalid_json_does_not_fall_back_to_normal_todo() {
    let executor = Arc::new(SeededTrainExecutor::new());
    let inspector = Arc::clone(&executor);
    let service = service_with_seeded_train(executor);

    // 明显火车输入触发 mock 返回非 JSON，应显式失败，不能回退普通 Todo 保存。
    let response = service
        .respond(message(
            "/todo add G34 杭州东 北京南 train-invalid-json 明天",
        ))
        .await
        .unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("模型没有返回合法 JSON"));
    assert!(inspector.requests().is_empty());

    let owner = TodoStore::owner(Some("u1"), "group:g1");
    assert!(service.todo_store.list_pending(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn train_todo_add_not_train_json_falls_back_to_normal_todo() {
    let executor = Arc::new(SeededTrainExecutor::new());
    let inspector = Arc::clone(&executor);
    let provider = MockProvider::new();
    let provider_inspector = provider.clone();
    let (service, _base) = test_service_with_provider_base_title_query_weather_train_and_models(
        provider,
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        executor,
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );

    let response = service
        .respond(message(
            "/todo add 明天坐高铁从会议室去机房 train-not-train",
        ))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_add"));
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("待确认新增待办"));
    assert!(text.contains("会议室到机房检查"));
    assert!(inspector.requests().is_empty());

    let llm_requests = provider_inspector.requests();
    assert_eq!(llm_requests.len(), 2);
    assert_eq!(
        llm_requests[0]
            .metadata
            .get("todo_operation")
            .map(String::as_str),
        Some("train_add")
    );
    assert_eq!(
        llm_requests[1]
            .metadata
            .get("todo_operation")
            .map(String::as_str),
        Some("add")
    );
}

#[tokio::test]
async fn train_like_bug_todo_add_uses_normal_todo_flow() {
    let executor = Arc::new(SeededTrainExecutor::new());
    let inspector = Arc::clone(&executor);
    let provider = MockProvider::new();
    let provider_inspector = provider.clone();
    let (service, _base) = test_service_with_provider_base_title_query_weather_train_and_models(
        provider,
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        executor,
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );

    let response = service
        .respond(message("/todo add G34 版本 bug 明天修"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_add"));
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("待确认新增待办"));
    assert!(text.contains("G34 版本 bug"));
    assert!(inspector.requests().is_empty());

    let llm_requests = provider_inspector.requests();
    assert_eq!(llm_requests.len(), 1);
    assert_eq!(
        llm_requests[0]
            .metadata
            .get("todo_operation")
            .map(String::as_str),
        Some("add")
    );
}

#[tokio::test]
async fn train_like_k20_bug_todo_add_uses_normal_todo_flow() {
    let executor = Arc::new(SeededTrainExecutor::new());
    let inspector = Arc::clone(&executor);
    let provider = MockProvider::new();
    let provider_inspector = provider.clone();
    let (service, _base) = test_service_with_provider_base_title_query_weather_train_and_models(
        provider,
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        executor,
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );

    let response = service
        .respond(message("/todo add K20-回归问题 今天跟进"))
        .await
        .unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_add"));
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("待确认新增待办"));
    assert!(text.contains("K20-回归问题"));
    assert!(inspector.requests().is_empty());

    let llm_requests = provider_inspector.requests();
    assert_eq!(llm_requests.len(), 1);
    assert_eq!(
        llm_requests[0]
            .metadata
            .get("todo_operation")
            .map(String::as_str),
        Some("add")
    );
}

#[tokio::test]
async fn normal_todo_add_not_treated_as_train() {
    let executor = Arc::new(SeededTrainExecutor::new());
    let inspector = Arc::clone(&executor);
    let provider = MockProvider::new();
    let provider_inspector = provider.clone();
    let (service, _base) = test_service_with_provider_base_title_query_weather_train_and_models(
        provider,
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        executor,
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
    );

    let response = service.respond(message("/todo add 买牛奶")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_add"));
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("待确认新增待办"));
    assert!(text.contains("买牛奶"));
    assert!(!text.contains("火车行程"));
    assert!(inspector.requests().is_empty());

    let llm_requests = provider_inspector.requests();
    assert_eq!(llm_requests.len(), 1);
    assert_eq!(
        llm_requests[0]
            .metadata
            .get("todo_operation")
            .map(String::as_str),
        Some("add")
    );

    // 确认普通 Todo 写入。
    service.respond(message("确认")).await.unwrap();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let items = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "买牛奶");
}

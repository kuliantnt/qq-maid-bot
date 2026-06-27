use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use chrono::{Duration, NaiveDate};
use serde_json::{Value, json};
use uuid::Uuid;

use super::super::{
    RespondExecutors, RespondRequest, RespondServiceOptions, RespondStores, RustRespondService,
    common::empty_respond_request,
};
use crate::{
    config::DEFAULT_RSS_SUMMARY_MAX_CHARS,
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider,
        types::{ChatRequest, ChatRole, TokenUsage},
    },
    runtime::{
        knowledge::KnowledgeIndex,
        memory::MemoryStore,
        prompt::PromptConfig,
        query::{QueryExecutor, QueryOutcome, QueryRequest, QuerySource},
        rss::{RssFetchConfig, RssFetcher, RssStore},
        session::{SessionMeta, SessionStore},
        todo::{TodoItem, TodoStatus, TodoStore, TodoTimePrecision},
        train::{TrainExecutor, TrainSchedule, TrainScheduleRequest, TrainStop},
        weather::{
            AirQualitySummary, CurrentWeather, DailyWeather, WeatherAlert, WeatherExecutor,
            WeatherLifeIndex, WeatherLocation, WeatherOutcome, WeatherRequest, WeatherSupplement,
        },
    },
    storage::{APP_MIGRATIONS, database::SqliteDatabase, knowledge::KnowledgeStore},
    util::{metrics::LlmMetrics, time_context::request_time_context},
};

#[derive(Clone)]
pub(super) struct MockProvider {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    title_replies: Arc<Mutex<Vec<Result<String, LlmError>>>>,
}

pub(super) struct MockQueryExecutor;

#[derive(Clone)]
pub(super) struct MockWeatherExecutor {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<WeatherRequest>>>,
}

pub(super) struct FailingWeatherExecutor {
    pub(super) err: LlmError,
}

pub(super) struct SupplementWeatherExecutor {
    pub(super) alerts: WeatherSupplement<Vec<WeatherAlert>>,
    pub(super) air_quality: WeatherSupplement<AirQualitySummary>,
    pub(super) life_indices: WeatherSupplement<Vec<WeatherLifeIndex>>,
}

pub(super) struct FailingQueryExecutor {
    pub(super) err: LlmError,
}

#[derive(Clone)]
pub(super) struct MockTrainExecutor {
    requests: Arc<Mutex<Vec<TrainScheduleRequest>>>,
}

/// 可按车次注入固定时刻表的火车执行器，用于火车行程 Todo 测试。
///
/// 未配置的车次回退到默认的北京南→上海虹桥时刻表，保持与 `MockTrainExecutor` 一致的行为。
pub(super) struct SeededTrainExecutor {
    pub(super) requests: Arc<Mutex<Vec<TrainScheduleRequest>>>,
    pub(super) schedules: std::collections::HashMap<String, TrainSchedule>,
    pub(super) dated_schedules:
        std::collections::HashMap<(String, chrono::NaiveDate), TrainSchedule>,
    pub(super) failing_codes: std::collections::HashMap<String, LlmError>,
}

pub(super) struct FailingTrainExecutor {
    pub(super) err: LlmError,
}

pub(super) struct TestModelOptions {
    pub(super) todo_model: Option<String>,
    pub(super) memory_model: Option<String>,
    pub(super) compact_model: Option<String>,
    pub(super) translation_model: Option<String>,
}

impl MockProvider {
    pub(super) fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
            title_replies: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(super) fn with_counter(calls: Arc<AtomicUsize>) -> Self {
        Self {
            calls,
            requests: Arc::new(Mutex::new(Vec::new())),
            title_replies: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(super) fn with_title_replies(replies: Vec<Result<&str, LlmError>>) -> Self {
        Self {
            title_replies: Arc::new(Mutex::new(
                replies
                    .into_iter()
                    .map(|result| result.map(str::to_owned))
                    .collect(),
            )),
            ..Self::new()
        }
    }

    pub(super) fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl MockWeatherExecutor {
    pub(super) fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(super) fn with_counter(calls: Arc<AtomicUsize>) -> Self {
        Self {
            calls,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(super) fn requests(&self) -> Vec<WeatherRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl MockTrainExecutor {
    pub(super) fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(super) fn requests(&self) -> Vec<TrainScheduleRequest> {
        self.requests.lock().unwrap().clone()
    }
}

fn mock_weather_alerts() -> WeatherSupplement<Vec<WeatherAlert>> {
    WeatherSupplement::available(vec![
        WeatherAlert {
            headline: "杭州市气象台发布大风蓝色预警".to_owned(),
            event_name: Some("大风".to_owned()),
            severity: Some("minor".to_owned()),
            color_code: Some("blue".to_owned()),
            sender_name: Some("杭州市气象台".to_owned()),
            issued_time: Some("2026-06-12T18:00+08:00".to_owned()),
            expire_time: Some("2026-06-13T18:00+08:00".to_owned()),
            description: Some("预计未来24小时阵风较大，请注意户外高空物品安全。".to_owned()),
        },
        WeatherAlert {
            headline: "杭州市气象台发布雷电黄色预警".to_owned(),
            event_name: Some("雷电".to_owned()),
            severity: Some("moderate".to_owned()),
            color_code: Some("yellow".to_owned()),
            sender_name: Some("杭州市气象台".to_owned()),
            issued_time: Some("2026-06-12T19:00+08:00".to_owned()),
            expire_time: Some("2026-06-13T06:00+08:00".to_owned()),
            description: Some("局地可能出现雷电活动，短时风雨较明显。".to_owned()),
        },
        WeatherAlert {
            headline: "第三条预警不应进入回复".to_owned(),
            event_name: Some("测试".to_owned()),
            severity: None,
            color_code: None,
            sender_name: None,
            issued_time: None,
            expire_time: None,
            description: None,
        },
    ])
}

fn mock_air_quality() -> WeatherSupplement<AirQualitySummary> {
    WeatherSupplement::available(AirQualitySummary {
        code: Some("cn-mee".to_owned()),
        name: Some("AQI（CN）".to_owned()),
        aqi_display: "42".to_owned(),
        level: Some("1".to_owned()),
        category: Some("优".to_owned()),
        primary_pollutant: Some("PM2.5".to_owned()),
    })
}

fn mock_life_indices() -> WeatherSupplement<Vec<WeatherLifeIndex>> {
    WeatherSupplement::available(vec![
        WeatherLifeIndex {
            date: "2026-06-12".to_owned(),
            type_id: "1".to_owned(),
            name: "运动指数".to_owned(),
            level: Some("2".to_owned()),
            category: Some("较适宜".to_owned()),
            text: Some("适合进行适量户外活动。".to_owned()),
        },
        WeatherLifeIndex {
            date: "2026-06-12".to_owned(),
            type_id: "3".to_owned(),
            name: "穿衣指数".to_owned(),
            level: Some("6".to_owned()),
            category: Some("热".to_owned()),
            text: Some("建议短袖。".to_owned()),
        },
        WeatherLifeIndex {
            date: "2026-06-12".to_owned(),
            type_id: "5".to_owned(),
            name: "紫外线指数".to_owned(),
            level: Some("4".to_owned()),
            category: Some("强".to_owned()),
            text: Some("注意防晒。".to_owned()),
        },
        WeatherLifeIndex {
            date: "2026-06-13".to_owned(),
            type_id: "1".to_owned(),
            name: "运动指数".to_owned(),
            level: Some("3".to_owned()),
            category: Some("较不宜".to_owned()),
            text: Some("次日不在摘要中展示。".to_owned()),
        },
    ])
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req.clone());
        if req.metadata.get("purpose").map(String::as_str) == Some("session_title") {
            let reply = self.title_replies.lock().unwrap().remove(0)?;
            return Ok(ChatOutcome {
                reply,
                metrics: LlmMetrics {
                    provider: "mock".to_owned(),
                    model: req.model.unwrap_or_else(|| "mock-model".to_owned()),
                    stream: false,
                    ttfe_ms: None,
                    ttft_ms: None,
                    total_latency_ms: 1,
                },
                usage: Some(TokenUsage {
                    input_tokens: None,
                    cached_input_tokens: None,
                    output_tokens: None,
                    total_tokens: None,
                }),
                fallback_used: false,
            });
        }
        let last_user = req
            .messages
            .iter()
            .rev()
            .find(|message| message.role == ChatRole::User)
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let metrics_model = req.model.clone().unwrap_or_else(|| "mock-model".to_owned());
        let reply = match req.metadata.get("purpose").map(String::as_str) {
            Some("todo_parse") => mock_todo_parse_reply(&last_user),
            Some("memory_draft") => mock_memory_draft_reply(
                &last_user,
                req.metadata.get("memory_operation").map(String::as_str),
            ),
            _ if last_user.contains("给 codex") => "# 标题\n- hello".to_owned(),
            _ => format!("回复：{last_user}"),
        };
        Ok(ChatOutcome {
            reply,
            metrics: LlmMetrics {
                provider: "mock".to_owned(),
                model: metrics_model,
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: Some(TokenUsage {
                input_tokens: None,
                cached_input_tokens: None,
                output_tokens: None,
                total_tokens: None,
            }),
            fallback_used: false,
        })
    }

    fn name(&self) -> &'static str {
        "mock"
    }

    fn model(&self) -> &str {
        "mock-model"
    }

    fn stream_enabled(&self) -> bool {
        false
    }
}

#[async_trait]
impl QueryExecutor for MockQueryExecutor {
    async fn query(&self, req: QueryRequest) -> Result<QueryOutcome, LlmError> {
        Ok(QueryOutcome {
            answer: format!("web answer: {}", req.query),
            sources: vec![QuerySource {
                title: "Source A".to_owned(),
                url: "https://a.test".to_owned(),
                snippet: "snippet".to_owned(),
            }],
            provider: "mock-query".to_owned(),
            elapsed_ms: 7,
        })
    }

    fn provider_name(&self) -> &'static str {
        "mock-query"
    }
}

#[async_trait]
impl WeatherExecutor for MockWeatherExecutor {
    async fn weather(&self, req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req.clone());
        Ok(WeatherOutcome {
            location: WeatherLocation {
                id: Some("101210101".to_owned()),
                name: "杭州".to_owned(),
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
                weather_code: 3,
                humidity_percent: Some(86),
                precipitation_mm: Some(1.2),
                pressure_hpa: Some(1006),
                wind_direction: Some("东北风".to_owned()),
                wind_scale: Some("3".to_owned()),
                wind_speed_kmh: Some(6.7),
            },
            daily: vec![
                DailyWeather {
                    date: "2026-06-12".to_owned(),
                    weather_code: 3,
                    weather_day: Some("多云".to_owned()),
                    weather_night: Some("阴".to_owned()),
                    temperature_max_c: 32.5,
                    temperature_min_c: 21.0,
                    precipitation_probability_max: Some(2),
                    precipitation_mm: Some(0.0),
                    humidity_percent: Some(83),
                    wind_direction_day: Some("东风".to_owned()),
                    wind_scale_day: Some("1-3".to_owned()),
                },
                DailyWeather {
                    date: "2026-06-13".to_owned(),
                    weather_code: 61,
                    weather_day: Some("小雨".to_owned()),
                    weather_night: Some("小雨".to_owned()),
                    temperature_max_c: 26.0,
                    temperature_min_c: 22.2,
                    precipitation_probability_max: Some(69),
                    precipitation_mm: Some(3.1),
                    humidity_percent: Some(90),
                    wind_direction_day: Some("东北风".to_owned()),
                    wind_scale_day: Some("3".to_owned()),
                },
                DailyWeather {
                    date: "2026-06-14".to_owned(),
                    weather_code: 51,
                    weather_day: Some("毛毛雨".to_owned()),
                    weather_night: Some("阴".to_owned()),
                    temperature_max_c: 26.6,
                    temperature_min_c: 21.3,
                    precipitation_probability_max: Some(69),
                    precipitation_mm: Some(1.8),
                    humidity_percent: Some(88),
                    wind_direction_day: Some("东风".to_owned()),
                    wind_scale_day: Some("1-3".to_owned()),
                },
            ],
            provider: "mock-weather".to_owned(),
            elapsed_ms: 7,
            forecast_days: req.forecast_days,
            alerts: mock_weather_alerts(),
            air_quality: mock_air_quality(),
            life_indices: mock_life_indices(),
        })
    }

    fn provider_name(&self) -> &'static str {
        "mock-weather"
    }
}

#[async_trait]
impl WeatherExecutor for SupplementWeatherExecutor {
    async fn weather(&self, req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
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
                weather_code: 3,
                humidity_percent: Some(86),
                precipitation_mm: None,
                pressure_hpa: None,
                wind_direction: Some("东北风".to_owned()),
                wind_scale: Some("3".to_owned()),
                wind_speed_kmh: Some(6.7),
            },
            daily: vec![DailyWeather {
                date: "2026-06-12".to_owned(),
                weather_code: 3,
                weather_day: Some("多云".to_owned()),
                weather_night: Some("阴".to_owned()),
                temperature_max_c: 32.5,
                temperature_min_c: 21.0,
                precipitation_probability_max: Some(2),
                precipitation_mm: Some(0.0),
                humidity_percent: Some(83),
                wind_direction_day: Some("东风".to_owned()),
                wind_scale_day: Some("1-3".to_owned()),
            }],
            provider: "mock-weather".to_owned(),
            elapsed_ms: 7,
            forecast_days: req.forecast_days,
            alerts: self.alerts.clone(),
            air_quality: self.air_quality.clone(),
            life_indices: self.life_indices.clone(),
        })
    }

    fn provider_name(&self) -> &'static str {
        "mock-weather"
    }
}

#[async_trait]
impl WeatherExecutor for FailingWeatherExecutor {
    async fn weather(&self, _req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
        Err(self.err.clone())
    }

    fn provider_name(&self) -> &'static str {
        "mock-weather"
    }
}

#[async_trait]
impl QueryExecutor for FailingQueryExecutor {
    async fn query(&self, _req: QueryRequest) -> Result<QueryOutcome, LlmError> {
        Err(self.err.clone())
    }

    fn provider_name(&self) -> &'static str {
        "mock-query"
    }
}

#[async_trait]
impl TrainExecutor for MockTrainExecutor {
    async fn query_train_schedule(
        &self,
        req: TrainScheduleRequest,
    ) -> Result<TrainSchedule, LlmError> {
        self.requests.lock().unwrap().push(req.clone());
        Ok(TrainSchedule {
            train_code: req.train_code.clone(),
            travel_date: req.travel_date,
            start_station: "北京南".to_owned(),
            end_station: "上海虹桥".to_owned(),
            stops: vec![
                TrainStop {
                    station_no: 1,
                    station_name: "北京南".to_owned(),
                    arrive_time: None,
                    departure_time: Some("06:30".to_owned()),
                    stopover_minutes: None,
                    day_difference: 0,
                    day_difference_reliable: true,
                    station_train_code: req.train_code.clone(),
                },
                TrainStop {
                    station_no: 2,
                    station_name: "南京南".to_owned(),
                    arrive_time: Some("10:13".to_owned()),
                    departure_time: Some("10:15".to_owned()),
                    stopover_minutes: Some(2),
                    day_difference: 0,
                    day_difference_reliable: true,
                    station_train_code: req.train_code.clone(),
                },
                TrainStop {
                    station_no: 3,
                    station_name: "上海虹桥".to_owned(),
                    arrive_time: Some("11:24".to_owned()),
                    departure_time: None,
                    stopover_minutes: None,
                    day_difference: 0,
                    day_difference_reliable: true,
                    station_train_code: req.train_code.clone(),
                },
            ],
            full_train_code: None,
            corporation: None,
            train_style: None,
            dept_train: None,
        })
    }

    fn provider_name(&self) -> &'static str {
        "mock-train"
    }
}

impl SeededTrainExecutor {
    pub(super) fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            schedules: std::collections::HashMap::new(),
            dated_schedules: std::collections::HashMap::new(),
            failing_codes: std::collections::HashMap::new(),
        }
    }

    /// 注入指定车次的固定时刻表。
    pub(super) fn with_schedule(mut self, train_code: &str, schedule: TrainSchedule) -> Self {
        self.schedules
            .insert(train_code.to_ascii_uppercase(), schedule);
        self
    }

    /// 注入指定车次在指定查询日期的固定时刻表。
    ///
    /// 用于模拟同车次在不同 `startDay` 下返回不同经停结果，覆盖火车 Todo
    /// 回看候选始发日时不能首错即退的场景。
    pub(super) fn with_schedule_on(
        mut self,
        train_code: &str,
        travel_date: chrono::NaiveDate,
        schedule: TrainSchedule,
    ) -> Self {
        self.dated_schedules
            .insert((train_code.to_ascii_uppercase(), travel_date), schedule);
        self
    }

    /// 注入指定车次的失败响应。
    pub(super) fn with_failing(mut self, train_code: &str, err: LlmError) -> Self {
        self.failing_codes
            .insert(train_code.to_ascii_uppercase(), err);
        self
    }

    pub(super) fn requests(&self) -> Vec<TrainScheduleRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl TrainExecutor for SeededTrainExecutor {
    async fn query_train_schedule(
        &self,
        req: TrainScheduleRequest,
    ) -> Result<TrainSchedule, LlmError> {
        self.requests.lock().unwrap().push(req.clone());
        let upper = req.train_code.to_ascii_uppercase();
        if let Some(schedule) = self.dated_schedules.get(&(upper.clone(), req.travel_date)) {
            let mut schedule = schedule.clone();
            schedule.train_code = req.train_code.clone();
            schedule.travel_date = req.travel_date;
            return Ok(schedule);
        }
        if let Some(err) = self.failing_codes.get(&upper) {
            return Err(err.clone());
        }
        if let Some(schedule) = self.schedules.get(&upper) {
            // 返回注入的 schedule，但用请求中的车次和日期覆盖，保持一致性。
            let mut schedule = schedule.clone();
            schedule.train_code = req.train_code.clone();
            schedule.travel_date = req.travel_date;
            return Ok(schedule);
        }
        // 未注入的车次回退到默认时刻表。
        Ok(TrainSchedule {
            train_code: req.train_code.clone(),
            travel_date: req.travel_date,
            start_station: "北京南".to_owned(),
            end_station: "上海虹桥".to_owned(),
            stops: vec![
                TrainStop {
                    station_no: 1,
                    station_name: "北京南".to_owned(),
                    arrive_time: None,
                    departure_time: Some("06:30".to_owned()),
                    stopover_minutes: None,
                    day_difference: 0,
                    day_difference_reliable: true,
                    station_train_code: req.train_code.clone(),
                },
                TrainStop {
                    station_no: 2,
                    station_name: "南京南".to_owned(),
                    arrive_time: Some("10:13".to_owned()),
                    departure_time: Some("10:15".to_owned()),
                    stopover_minutes: Some(2),
                    day_difference: 0,
                    day_difference_reliable: true,
                    station_train_code: req.train_code.clone(),
                },
                TrainStop {
                    station_no: 3,
                    station_name: "上海虹桥".to_owned(),
                    arrive_time: Some("11:24".to_owned()),
                    departure_time: None,
                    stopover_minutes: None,
                    day_difference: 0,
                    day_difference_reliable: true,
                    station_train_code: req.train_code.clone(),
                },
            ],
            full_train_code: None,
            corporation: None,
            train_style: None,
            dept_train: None,
        })
    }

    fn provider_name(&self) -> &'static str {
        "mock-seeded-train"
    }
}

#[async_trait]
impl TrainExecutor for FailingTrainExecutor {
    async fn query_train_schedule(
        &self,
        _req: TrainScheduleRequest,
    ) -> Result<TrainSchedule, LlmError> {
        Err(self.err.clone())
    }

    fn provider_name(&self) -> &'static str {
        "mock-train"
    }
}

fn write_prompt_set(dir: &std::path::Path) {
    fs::create_dir_all(dir).unwrap();
    for file_name in crate::runtime::prompt::PROMPT_FILES {
        fs::write(dir.join(file_name), format!("{file_name} content")).unwrap();
    }
}

fn mock_revision_input(prompt: &str) -> Option<Value> {
    let (_, json_text) = prompt.split_once("修订输入 JSON：")?;
    serde_json::from_str(json_text.trim()).ok()
}

fn mock_current_memory_content(prompt: &str) -> Option<String> {
    mock_revision_input(prompt)?
        .get("current_draft")?
        .get("content")?
        .as_str()
        .map(str::to_owned)
}

fn mock_current_todo_draft(prompt: &str) -> Option<Value> {
    mock_revision_input(prompt)?.get("current_draft").cloned()
}

fn mock_revision_user_input(prompt: &str) -> String {
    mock_revision_input(prompt)
        .and_then(|value| {
            value
                .get("user_input")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn mock_memory_draft_reply(prompt: &str, operation: Option<&str>) -> String {
    if prompt.contains("invalid-memory-revision") {
        return "无法整理".to_owned();
    }
    if prompt.contains("invalid-memory-create") {
        return "不是 JSON".to_owned();
    }
    if prompt.contains("null-memory-create") || prompt.contains("null-memory-revision") {
        return json!({ "content": null }).to_string();
    }
    if prompt.contains("empty-memory-create") || prompt.contains("empty-memory-revision") {
        return json!({ "content": "" }).to_string();
    }
    if prompt.contains("fenced-memory-create") || prompt.contains("fenced-memory-revision") {
        return format!(
            "```json\n{}\n```",
            json!({ "content": "前台不确定时先询问本人再记录" })
        );
    }
    if prompt.contains("先询问本人再记录") {
        return json!({ "content": "前台不确定时先询问本人再记录" }).to_string();
    }
    if prompt.contains("如果不确定前台，请礼貌询问") {
        return json!({ "content": "前台不确定时请礼貌询问" }).to_string();
    }
    if matches!(operation, Some("create_revise" | "update_revise")) {
        return json!({ "content": mock_current_memory_content(prompt) }).to_string();
    }
    if matches!(operation, Some("create")) {
        return json!({ "content": null }).to_string();
    }
    format!("回复：{prompt}")
}

fn mock_todo_parse_reply(prompt: &str) -> String {
    if prompt.contains("invalid-json") {
        return "不是 JSON".to_owned();
    }
    // 火车行程识别分支：操作为 train_add 时，根据用户原文判断是否为火车行程。
    if prompt.contains("操作：train_add") {
        return mock_train_todo_parse_reply(prompt);
    }
    if prompt.contains("操作：add_revise") || prompt.contains("操作：edit_revise") {
        return mock_todo_revise_reply(prompt);
    }
    if prompt.contains("操作：edit_patch") {
        if prompt.contains("时间需要改成这个月底之前完成") {
            return json!({
                "due_date": "2026-06-30",
                "due_at": null,
                "time_precision": "inferred"
            })
            .to_string();
        }
        if prompt.contains("理解错了，实际上标题还是示例项目审查，内容是之前的标题")
        {
            return json!({
                "title": "示例项目审查",
                "detail": "之前的标题"
            })
            .to_string();
        }
        if prompt.contains("内容改成 示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成")
        {
            return json!({
                "detail": "示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成",
                "due_date": "2026-06-30",
                "due_at": null,
                "time_precision": "inferred"
            })
            .to_string();
        }
        if prompt.contains("先做一份给负责人看看") {
            return json!({
                "detail": "先做一份示例材料给负责人看看，再根据反馈调整",
                "due_date": "2026-06-30",
                "due_at": null,
                "time_precision": "inferred"
            })
            .to_string();
        }
        if prompt.contains("月底前需要和负责人理一下") {
            return json!({
                "title": "示例材料需要重新做",
                "detail": "需要和负责人理一下",
                "due_date": "2026-06-30",
                "due_at": null,
                "time_precision": "inferred"
            })
            .to_string();
        }
        if prompt.contains("示例系统维保 - 2026 做完了") {
            return json!({
                "title": "示例系统维保 - 2026"
            })
            .to_string();
        }
        if prompt.contains("改成明天检查服务") || prompt.contains("明天检查服务") {
            return json!({
                "title": "检查服务"
            })
            .to_string();
        }
        return json!({}).to_string();
    }
    if prompt.contains("无时间") || prompt.contains("买牛奶") {
        return json!({
            "title": "买牛奶",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("引用里的待办") {
        return json!({
            "title": "引用里的待办",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("三天后检查日志") {
        return json!({
            "title": "检查日志",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("G34 版本 bug 明天修") {
        return json!({
            "title": "G34 版本 bug",
            "detail": "明天修",
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("K20-回归问题 今天跟进") {
        return json!({
            "title": "K20-回归问题",
            "detail": "今天跟进",
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("train-not-train") {
        return json!({
            "title": "会议室到机房检查",
            "detail": "普通待办，不是火车行程",
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("2026年6月15日提交报告") {
        return json!({
            "title": "提交报告",
            "detail": null,
            "due_date": "2026-06-15",
            "due_at": null,
            "time_precision": "date"
        })
        .to_string();
    }
    if prompt.contains("月底复盘") {
        return json!({
            "title": "复盘",
            "detail": null,
            "due_date": "2026-06-30",
            "due_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if prompt.contains("先做一份给负责人看看") {
        return json!({
            "title": "示例材料需要重新做",
            "detail": "先做一份示例材料给负责人看看，再根据反馈调整",
            "due_date": "2026-06-30",
            "due_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if prompt.contains("月底前需要和负责人理一下") {
        return json!({
            "title": "示例材料需要重新做",
            "detail": "需要和负责人理一下",
            "due_date": "2026-06-30",
            "due_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if prompt.contains("示例系统维保 - 2026 做完了") {
        return json!({
            "title": "示例系统维保 - 2026",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("检查服务器") || prompt.contains("检查 server") {
        return json!({
            "title": "检查服务器",
            "detail": "server",
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("查交通") || prompt.contains("交通") {
        return json!({
            "title": "查交通",
            "detail": "交通",
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("检查数据库") || prompt.contains("数据库") {
        return json!({
            "title": "检查数据库",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if prompt.contains("改成明天检查服务") || prompt.contains("明天检查服务") {
        return json!({
            "title": "检查服务",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    json!({
        "title": "待办",
        "detail": null,
        "due_date": null,
        "due_at": null,
        "time_precision": "none"
    })
    .to_string()
}

fn mock_todo_revise_reply(prompt: &str) -> String {
    let user_input = mock_revision_user_input(prompt);
    if user_input.contains("标题改成准备材料")
        || user_input.contains("详情补充先发负责人")
        || user_input.contains("时间这个月底前")
    {
        return json!({
            "title": "准备材料",
            "detail": "先发负责人",
            "due_date": "2026-06-30",
            "due_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if user_input.contains("时间需要改成这个月底之前完成") {
        return json!({
            "title": "示例系统维保 - 2026",
            "detail": null,
            "due_date": "2026-06-30",
            "due_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if user_input.contains("理解错了，实际上标题还是示例项目审查，内容是之前的标题")
    {
        return json!({
            "title": "示例项目审查",
            "detail": "之前的标题",
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    if user_input.contains("内容改成 示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成")
    {
        return json!({
            "title": "示例项目审查",
            "detail": "示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成",
            "due_date": "2026-06-30",
            "due_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if user_input.contains("先做一份给负责人看看") {
        return json!({
            "title": "示例材料需要重新做",
            "detail": "先做一份示例材料给负责人看看，再根据反馈调整",
            "due_date": "2026-06-30",
            "due_at": null,
            "time_precision": "inferred"
        })
        .to_string();
    }
    if user_input.contains("改成明天检查服务") || user_input.contains("明天检查服务")
    {
        return json!({
            "title": "检查服务",
            "detail": null,
            "due_date": null,
            "due_at": null,
            "time_precision": "none"
        })
        .to_string();
    }
    mock_current_todo_draft(prompt)
        .unwrap_or_else(|| {
            json!({
                "title": "待办",
                "detail": null,
                "due_date": null,
                "due_at": null,
                "time_precision": "none"
            })
        })
        .to_string()
}

/// 火车行程识别 mock：根据用户原文判断是否输出 kind=train。
///
/// 测试用 mock 只覆盖关键字段识别；真实时刻由 MockTrainExecutor 提供。
fn mock_train_todo_parse_reply(prompt: &str) -> String {
    // 从 prompt 中提取用户原文（"用户原文：" 之后的部分）。
    let user_text = prompt
        .split_once("用户原文：")
        .map(|(_, rest)| rest.trim())
        .unwrap_or("");
    if user_text.contains("train-not-train")
        || user_text.contains("G34 版本 bug 明天修")
        || user_text.contains("K20-回归问题 今天跟进")
    {
        return json!({
            "kind": "todo",
            "title": "普通待办"
        })
        .to_string();
    }
    // 非 JSON 输出（测试 LLM 回空回退普通 Todo）
    if user_text.contains("train-invalid-json") {
        return "不是 JSON".to_owned();
    }
    // 自然语言输入优先：明天坐 G34 从杭州东去北京南
    if user_text.contains("坐 G34") || user_text.contains("坐G34") {
        return json!({
            "kind": "train",
            "train_code": "G34",
            "from_station": "杭州东",
            "to_station": "北京南",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    if user_text.contains("G34") && user_text.matches("杭州东").count() >= 2 {
        return json!({
            "kind": "train",
            "train_code": "G34",
            "from_station": "杭州东",
            "to_station": "杭州东",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    if user_text.contains("G34") && user_text.matches("南京南").count() >= 2 {
        return json!({
            "kind": "train",
            "train_code": "G34",
            "from_station": "南京南",
            "to_station": "南京南",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 结构化输入：/todo add G34 杭州东 北京南 明天 05车12A 8站台
    if user_text.contains("G34") && user_text.contains("杭州东") && user_text.contains("北京南")
    {
        return json!({
            "kind": "train",
            "train_code": "G34",
            "from_station": "杭州东",
            "to_station": "北京南",
            "travel_date": "2026-06-24",
            "seat": "05车12A",
            "platform": "8站台",
            "note": null
        })
        .to_string();
    }
    if user_text.contains("1461") && user_text.contains("北京") && user_text.contains("上海") {
        return json!({
            "kind": "train",
            "train_code": "1461",
            "from_station": "北京",
            "to_station": "上海",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 跨日行程：Z281 杭州 西安
    if user_text.contains("Z281") {
        return json!({
            "kind": "train",
            "train_code": "Z281",
            "from_station": "杭州",
            "to_station": "西安",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    if user_text.contains("K20") {
        return json!({
            "kind": "train",
            "train_code": "K20",
            "from_station": "中途站",
            "to_station": "终到站",
            "travel_date": "2026-06-25",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 缺少日期的火车行程
    if user_text.contains("G99") {
        return json!({
            "kind": "train",
            "train_code": "G99",
            "from_station": "杭州东",
            "to_station": "北京南",
            "travel_date": null,
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 站点不匹配的火车行程
    if user_text.contains("G50") {
        return json!({
            "kind": "train",
            "train_code": "G50",
            "from_station": "上海",
            "to_station": "北京南",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 站点顺序错误的火车行程
    if user_text.contains("G88") {
        return json!({
            "kind": "train",
            "train_code": "G88",
            "from_station": "北京南",
            "to_station": "杭州东",
            "travel_date": "2026-06-24",
            "seat": null,
            "platform": null,
            "note": null
        })
        .to_string();
    }
    // 普通 Todo 输入：回退普通待办 JSON
    json!({
        "title": "买牛奶",
        "detail": null,
        "due_date": null,
        "due_at": null,
        "time_precision": "none"
    })
    .to_string()
}

pub(super) fn test_service() -> RustRespondService {
    test_service_with_provider(MockProvider::new())
}

pub(super) fn test_service_with_provider(provider: MockProvider) -> RustRespondService {
    test_service_with_provider_and_base(provider).0
}

pub(super) fn test_service_with_base() -> (RustRespondService, PathBuf) {
    test_service_with_provider_and_base(MockProvider::new())
}

pub(super) fn test_service_with_provider_and_base(
    provider: MockProvider,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_and_base_and_title(provider, None)
}

pub(super) fn test_service_with_provider_and_base_and_title(
    provider: MockProvider,
    title_model: Option<String>,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_base_title_and_query(
        provider,
        title_model,
        Arc::new(MockQueryExecutor),
    )
}

pub(super) fn test_service_with_provider_base_title_and_query(
    provider: MockProvider,
    title_model: Option<String>,
    query_executor: Arc<dyn QueryExecutor>,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_base_title_query_and_models(
        provider,
        title_model,
        query_executor,
        Arc::new(MockWeatherExecutor::new()),
        None,
        None,
        None,
    )
}

pub(super) fn test_service_with_provider_base_title_query_and_models(
    provider: MockProvider,
    title_model: Option<String>,
    query_executor: Arc<dyn QueryExecutor>,
    weather_executor: Arc<dyn WeatherExecutor>,
    todo_model: Option<String>,
    memory_model: Option<String>,
    compact_model: Option<String>,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_base_title_query_weather_train_and_models(
        provider,
        title_model,
        query_executor,
        weather_executor,
        Arc::new(MockTrainExecutor::new()),
        TestModelOptions {
            todo_model,
            memory_model,
            compact_model,
            translation_model: None,
        },
    )
}

pub(super) fn test_service_with_provider_base_title_query_weather_train_and_models(
    provider: MockProvider,
    title_model: Option<String>,
    query_executor: Arc<dyn QueryExecutor>,
    weather_executor: Arc<dyn WeatherExecutor>,
    train_executor: Arc<dyn TrainExecutor>,
    models: TestModelOptions,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_base_title_query_weather_and_models(
        provider,
        title_model,
        query_executor,
        weather_executor,
        train_executor,
        TestModelOptions {
            todo_model: models.todo_model,
            memory_model: models.memory_model,
            compact_model: models.compact_model,
            translation_model: models.translation_model,
        },
    )
}

pub(super) fn test_service_with_translation_model(
    provider: MockProvider,
    translation_model: Option<String>,
) -> RustRespondService {
    test_service_with_provider_base_title_query_weather_and_models(
        provider,
        None,
        Arc::new(MockQueryExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(MockTrainExecutor::new()),
        TestModelOptions {
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model,
        },
    )
    .0
}

fn test_service_with_provider_base_title_query_weather_and_models(
    provider: MockProvider,
    title_model: Option<String>,
    query_executor: Arc<dyn QueryExecutor>,
    weather_executor: Arc<dyn WeatherExecutor>,
    train_executor: Arc<dyn TrainExecutor>,
    models: TestModelOptions,
) -> (RustRespondService, PathBuf) {
    let base = std::env::temp_dir().join(format!("qq-maid-respond-{}", Uuid::new_v4()));
    let prompt_dir = base.join("prompts");
    write_prompt_set(&prompt_dir);
    let mapping_file = base.join("member.json");
    fs::write(
        &mapping_file,
        r#"{"407":{"name":"测试成员","profile":"示例成员"}}"#,
    )
    .unwrap();
    let database = SqliteDatabase::open(base.join("app.db"), APP_MIGRATIONS).unwrap();
    let knowledge_dir = base.join("knowledge");
    let knowledge_index = KnowledgeIndex::new(KnowledgeStore::new(database.clone()), knowledge_dir);
    knowledge_index.sync().unwrap();
    let service = RustRespondService::new(
        Arc::new(provider),
        RespondExecutors {
            query_executor,
            weather_executor,
            train_executor,
        },
        RespondStores {
            memory_store: MemoryStore::new(database.clone()),
            session_store: SessionStore::new(database.clone()),
            todo_store: TodoStore::new(database.clone()),
            rss_store: RssStore::new(database.clone()),
        },
        RssFetcher::new(RssFetchConfig {
            allow_private_networks: true,
            ..RssFetchConfig::default()
        })
        .unwrap(),
        knowledge_index,
        PromptConfig::new(prompt_dir, mapping_file),
        RespondServiceOptions {
            title_model,
            todo_model: models.todo_model,
            memory_model: models.memory_model,
            compact_model: models.compact_model,
            translation_model: models.translation_model,
            send_mode: "final".to_owned(),
            rss_summary_max_chars: DEFAULT_RSS_SUMMARY_MAX_CHARS as usize,
            rss_seen_retention: 500,
        },
    );
    (service, base)
}

pub(super) fn test_service_with_title_provider(
    provider: MockProvider,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_and_base_and_title(provider, Some("title-model".to_owned()))
}

pub(super) fn message(text: &str) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: "group:g1".to_owned(),
        user_id: Some("u1".to_owned()),
        group_id: Some("g1".to_owned()),
        platform: "qq_official".to_owned(),
        event_type: "FakeEvent".to_owned(),
        ..empty_respond_request()
    }
}

pub(super) fn message_in_scope(
    text: &str,
    scope_key: &str,
    user_id: &str,
    group_id: &str,
) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: scope_key.to_owned(),
        user_id: Some(user_id.to_owned()),
        group_id: Some(group_id.to_owned()),
        platform: "qq_official".to_owned(),
        event_type: "FakeEvent".to_owned(),
        ..empty_respond_request()
    }
}

pub(super) fn test_meta() -> SessionMeta {
    SessionMeta::new(
        "group:g1",
        Some("u1".to_owned()),
        Some("g1".to_owned()),
        None,
        None,
        "qq_official",
    )
}

#[derive(Debug, Clone)]
pub(super) struct SeededCompletedTodos {
    pub(super) old_id: String,
    pub(super) yesterday_id: String,
}

fn completed_at_for(date: NaiveDate, hour: u32) -> String {
    format!("{}T{hour:02}:00:00+08:00", date.format("%Y-%m-%d"))
}

pub(super) fn seed_completed_time_todos(store: &TodoStore) -> SeededCompletedTodos {
    let today = request_time_context().local_date();
    let yesterday = today - Duration::days(1);
    let before_yesterday = today - Duration::days(2);
    let old_completed_at = completed_at_for(before_yesterday, 8);
    let yesterday_completed_at = completed_at_for(yesterday, 9);
    let today_completed_at = completed_at_for(today, 10);
    let cancelled_completed_at = completed_at_for(before_yesterday, 7);
    let old_created_at = completed_at_for(before_yesterday, 6);
    let yesterday_created_at = completed_at_for(before_yesterday, 7);
    let today_created_at = completed_at_for(before_yesterday, 8);
    let missing_created_at = completed_at_for(before_yesterday, 9);
    let cancelled_created_at = completed_at_for(before_yesterday, 10);
    let pending_created_at = completed_at_for(today, 11);

    let owner = TodoStore::owner(Some("u1"), "group:g1");
    store
        .set_items_for_test(
            &owner,
            &[
                TodoItem {
                    id: "1".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "前天完成".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                    status: TodoStatus::Completed,
                    created_at: old_created_at,
                    updated_at: old_completed_at.clone(),
                    completed_at: Some(old_completed_at),
                    cancelled_at: None,
                },
                TodoItem {
                    id: "2".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "昨天完成".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                    status: TodoStatus::Completed,
                    created_at: yesterday_created_at,
                    updated_at: yesterday_completed_at.clone(),
                    completed_at: Some(yesterday_completed_at),
                    cancelled_at: None,
                },
                TodoItem {
                    id: "3".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "今天完成".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                    status: TodoStatus::Completed,
                    created_at: today_created_at,
                    updated_at: today_completed_at.clone(),
                    completed_at: Some(today_completed_at),
                    cancelled_at: None,
                },
                TodoItem {
                    id: "4".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "没有完成时间".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                    status: TodoStatus::Completed,
                    created_at: missing_created_at.clone(),
                    updated_at: missing_created_at,
                    completed_at: None,
                    cancelled_at: None,
                },
                TodoItem {
                    id: "5".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "已取消完成".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: None,
                    due_at: None,
                    time_precision: TodoTimePrecision::None,
                    status: TodoStatus::Cancelled,
                    created_at: cancelled_created_at,
                    updated_at: cancelled_completed_at.clone(),
                    completed_at: Some(cancelled_completed_at.clone()),
                    cancelled_at: Some(cancelled_completed_at),
                },
                TodoItem {
                    id: "6".to_owned(),
                    user_id: Some("u1".to_owned()),
                    scope_key: "group:g1".to_owned(),
                    title: "未完成旧截止".to_owned(),
                    detail: None,
                    raw_text: None,
                    due_date: Some("2026-01-01".to_owned()),
                    due_at: None,
                    time_precision: TodoTimePrecision::Date,
                    status: TodoStatus::Pending,
                    created_at: pending_created_at.clone(),
                    updated_at: pending_created_at,
                    completed_at: None,
                    cancelled_at: None,
                },
            ],
        )
        .unwrap();

    SeededCompletedTodos {
        old_id: "1".to_owned(),
        yesterday_id: "2".to_owned(),
    }
}

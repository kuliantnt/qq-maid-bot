use super::*;

pub(crate) struct MockWebSearchExecutor;

#[derive(Clone)]
pub(crate) struct MockWeatherExecutor {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<WeatherRequest>>>,
}

pub(crate) struct FailingWeatherExecutor {
    pub(crate) err: LlmError,
}

pub(crate) struct SupplementWeatherExecutor {
    pub(crate) alerts: WeatherSupplement<Vec<WeatherAlert>>,
    pub(crate) air_quality: WeatherSupplement<AirQualitySummary>,
    pub(crate) life_indices: WeatherSupplement<Vec<WeatherLifeIndex>>,
}

pub(crate) struct FailingWebSearchExecutor {
    pub(crate) err: LlmError,
}

pub(crate) struct StreamOnlyWebSearchExecutor {
    pub(crate) deltas: Vec<String>,
    pub(crate) query_calls: Arc<AtomicUsize>,
    pub(crate) stream_calls: Arc<AtomicUsize>,
}

#[derive(Clone)]
pub(crate) struct MockTrainExecutor {
    requests: Arc<Mutex<Vec<TrainScheduleRequest>>>,
}

pub(crate) struct FailingTrainExecutor {
    pub(crate) err: LlmError,
}

#[derive(Clone)]
pub(crate) struct MockRadarExecutor {
    calls: Arc<AtomicUsize>,
    outcome: Arc<Mutex<Result<RadarSnapshot, LlmError>>>,
}

impl MockRadarExecutor {
    pub(crate) fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            outcome: Arc::new(Mutex::new(Ok(mock_radar_snapshot()))),
        }
    }
}

impl MockWeatherExecutor {
    pub(crate) fn new() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn with_counter(calls: Arc<AtomicUsize>) -> Self {
        Self {
            calls,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn requests(&self) -> Vec<WeatherRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl MockTrainExecutor {
    pub(crate) fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn requests(&self) -> Vec<TrainScheduleRequest> {
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
impl WebSearchExecutor for MockWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Ok(WebSearchOutcome {
            answer: format!("web answer: {}", req.query),
            sources: vec![WebSearchSource {
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
impl RadarExecutor for MockRadarExecutor {
    async fn radar(&self, target: RadarTarget) -> Result<RadarSnapshot, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let snapshot = self.outcome.lock().unwrap().clone()?;
        Ok(match target {
            RadarTarget::All => snapshot,
            RadarTarget::Codex => RadarSnapshot {
                codex: snapshot.codex,
                claude: None,
                failures: snapshot
                    .failures
                    .into_iter()
                    .filter(|failure| {
                        matches!(
                            failure.source,
                            crate::runtime::tools::RadarSourceKind::Codex
                        )
                    })
                    .collect(),
            },
            RadarTarget::Claude => RadarSnapshot {
                codex: None,
                claude: snapshot.claude,
                failures: snapshot
                    .failures
                    .into_iter()
                    .filter(|failure| {
                        matches!(
                            failure.source,
                            crate::runtime::tools::RadarSourceKind::Claude
                        )
                    })
                    .collect(),
            },
        })
    }

    fn provider_name(&self) -> &'static str {
        "mock-radar"
    }
}

pub(crate) fn mock_radar_snapshot() -> RadarSnapshot {
    RadarSnapshot {
        codex: Some(CodexRadarSummary {
            status: Some("community_confirmed".to_owned()),
            updated_at: Some("2026-06-30T18:39:12+08:00".to_owned()),
            action: Some("reset_completed".to_owned()),
            window_message: Some("社区反馈已完成重置，当前没有开启的速蹬窗口".to_owned()),
            prediction_level: Some("high".to_owned()),
            probability_24h: Some(0.36),
            model_score: Some(60.0),
            model_status: Some("red".to_owned()),
            model_passed: Some(4),
            model_tasks: Some(10),
            model_label: Some("GPT-5.5 xhigh".to_owned()),
            iq_models: vec![
                CodexModelMetric {
                    label: "GPT-5.5 xhigh".to_owned(),
                    score: Some(60.0),
                    status: Some("red".to_owned()),
                    passed: Some(4),
                    tasks: Some(10),
                },
                CodexModelMetric {
                    label: "GPT-5.4 xhigh".to_owned(),
                    score: Some(90.0),
                    status: Some("yellow".to_owned()),
                    passed: Some(6),
                    tasks: Some(10),
                },
            ],
            quota_5h_20x: Some(281.91),
            quota_7d_20x: Some(1691.46),
            source_url: "https://codexradar.com/".to_owned(),
            feedback_url: "https://codexradar.com/".to_owned(),
        }),
        claude: Some(ClaudeRadarSummary {
            status: Some("ok".to_owned()),
            updated_at: Some("2026-07-05T09:37:50+08:00".to_owned()),
            quota_updated_at: Some("2026-07-04T09:46:15+08:00".to_owned()),
            quota_5h: Some(332.29),
            quota_7d: Some(2270.63),
            usage_5h: Some("当前 5h 共享池 已用 41% · 13:00 重置".to_owned()),
            usage_7d: Some("当前 7d 额度 已用 60% · 7月4日 16:00 重置".to_owned()),
            top_iq_model: Some(ClaudeModelMetric {
                name: "Fable 5 xhigh".to_owned(),
                score: Some(120.0),
                passed: Some(8),
                valid: Some(10),
                invalid: Some(0),
                updated_at: Some("2026-07-03T15:25:03+08:00".to_owned()),
            }),
            top_rating_model: Some(ClaudeModelMetric {
                name: "Fable 5 xhigh".to_owned(),
                score: Some(9.1),
                passed: None,
                valid: Some(9),
                invalid: None,
                updated_at: None,
            }),
            source_url: "https://claudecoderadar.com/".to_owned(),
            feedback_url: "https://claudecoderadar.com/".to_owned(),
        }),
        failures: Vec::new(),
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
impl WebSearchExecutor for FailingWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Err(self.err.clone())
    }

    fn provider_name(&self) -> &'static str {
        "mock-query"
    }
}

#[async_trait]
impl WebSearchExecutor for StreamOnlyWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        self.query_calls.fetch_add(1, Ordering::SeqCst);
        Err(LlmError::provider("query must not be called", "test"))
    }

    async fn query_stream(
        &self,
        _req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        for delta in &self.deltas {
            delta_tx
                .send(delta.clone())
                .await
                .map_err(|_| LlmError::new("cancelled", "receiver dropped", "test"))?;
        }
        Ok(WebSearchOutcome {
            answer: self.deltas.join(""),
            sources: Vec::new(),
            provider: "stream-query".to_owned(),
            elapsed_ms: 11,
        })
    }

    fn provider_name(&self) -> &'static str {
        "stream-query"
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

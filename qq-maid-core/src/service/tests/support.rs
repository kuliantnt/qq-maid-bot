use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use qq_maid_common::identity_context::{
    IdentitySource, MentionConfidence, MentionIdentity, MessageActorContext,
};

use crate::{
    app::{CoreExecutors, CoreRuntimeState, CoreStores},
    config::{
        AppConfig, DEFAULT_BIGMODEL_BASE_URL, DEFAULT_DEEPSEEK_BASE_URL,
        DEFAULT_RSS_SUMMARY_MAX_CHARS, DailyReminderTime, OpenAiApiMode, ProviderMode,
    },
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, LlmStreamEvent, ToolCallingProtocol, ToolChatRequest,
        status::{UpstreamStatus, observe_provider},
        types::{ChatRequest, ModelRoute, TokenUsage},
    },
    runtime::{
        knowledge::KnowledgeIndex,
        prompt::PromptConfig,
        query::{QueryExecutor, QueryOutcome, QueryRequest},
        rss::{RssFetchConfig, RssFetcher, RssStore},
        session::SessionStore,
        tools::{RadarExecutor, RadarSnapshot, RadarTarget},
        train::{TrainExecutor, TrainSchedule, TrainScheduleRequest},
        weather::{WeatherExecutor, WeatherOutcome, WeatherRequest},
    },
    service::{
        CoreActor, CoreConversation, CoreError, CoreRequest, CoreRespondFailure, CoreRespondOutput,
        CoreResponse, CoreResponseEvent, CoreResponseStream, Platform,
    },
    storage::{APP_MIGRATIONS, database::SqliteDatabase, knowledge::KnowledgeStore},
    util::metrics::LlmMetrics,
};

pub(super) async fn collect_stream_failure(
    output: Result<CoreRespondOutput, CoreError>,
) -> CoreRespondFailure {
    let CoreRespondOutput::Stream(mut stream) = output.unwrap() else {
        panic!("expected stream output");
    };
    collect_failure_without_text_delta(&mut stream).await
}

pub(super) async fn collect_failure_without_text_delta(
    stream: &mut CoreResponseStream,
) -> CoreRespondFailure {
    while let Some(event) = stream.recv().await {
        match event {
            CoreResponseEvent::Status(_) => {}
            CoreResponseEvent::Failed(failure) => return failure,
            CoreResponseEvent::TextDelta(delta) => {
                panic!("unexpected text delta before failure: {delta}");
            }
            CoreResponseEvent::Completed(response) => {
                panic!("unexpected completed response before failure: {response:?}");
            }
        }
    }
    panic!("stream ended without failure");
}

pub(super) async fn collect_stream_completed(
    output: Result<CoreRespondOutput, CoreError>,
) -> CoreResponse {
    let mut stream = expect_stream(output.unwrap());
    while let Some(event) = stream.recv().await {
        if let CoreResponseEvent::Completed(response) = event {
            return *response;
        }
    }
    panic!("stream ended without completed response");
}

pub(super) async fn collect_completed_without_text_delta(
    stream: &mut CoreResponseStream,
) -> CoreResponse {
    while let Some(event) = stream.recv().await {
        match event {
            CoreResponseEvent::Status(_) => {}
            CoreResponseEvent::Completed(response) => return *response,
            CoreResponseEvent::TextDelta(delta) => {
                panic!("unexpected text delta before completed response: {delta}");
            }
            CoreResponseEvent::Failed(failure) => panic!("unexpected failure: {failure:?}"),
        }
    }
    panic!("stream ended without completed response");
}

pub(super) fn expect_stream(output: CoreRespondOutput) -> CoreResponseStream {
    let CoreRespondOutput::Stream(stream) = output else {
        panic!("expected stream output");
    };
    stream
}

#[derive(Clone)]
enum ProviderBehavior {
    Reply(String),
    Stream(Vec<Result<LlmStreamEvent, LlmError>>),
    Error(LlmError),
    Delayed { reply: String, delay: Duration },
}

#[derive(Clone)]
pub(super) struct TestProvider {
    behavior: ProviderBehavior,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    pub(super) calls: Arc<AtomicUsize>,
    pub(super) tool_calls: Arc<AtomicUsize>,
    tool_protocol: Option<ToolCallingProtocol>,
    stream_enabled: bool,
}

impl TestProvider {
    pub(super) fn replying(reply: &str) -> Self {
        Self::new(ProviderBehavior::Reply(reply.to_owned()))
    }

    pub(super) fn failing(error: LlmError) -> Self {
        Self::new(ProviderBehavior::Error(error))
    }

    pub(super) fn streaming(events: Vec<Result<LlmStreamEvent, LlmError>>) -> Self {
        Self::new(ProviderBehavior::Stream(events)).with_stream_enabled(true)
    }

    pub(super) fn delayed(reply: &str, delay: Duration) -> Self {
        Self::new(ProviderBehavior::Delayed {
            reply: reply.to_owned(),
            delay,
        })
    }

    fn new(behavior: ProviderBehavior) -> Self {
        Self {
            behavior,
            requests: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(AtomicUsize::new(0)),
            tool_calls: Arc::new(AtomicUsize::new(0)),
            tool_protocol: None,
            stream_enabled: false,
        }
    }

    pub(super) fn with_stream_enabled(mut self, enabled: bool) -> Self {
        self.stream_enabled = enabled;
        self
    }

    pub(super) fn with_tool_protocol(mut self, protocol: ToolCallingProtocol) -> Self {
        self.tool_protocol = Some(protocol);
        self
    }

    pub(super) fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl LlmProvider for TestProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req);
        match &self.behavior {
            ProviderBehavior::Reply(reply) => Ok(chat_outcome(reply)),
            ProviderBehavior::Stream(events) => {
                let reply = events
                    .iter()
                    .filter_map(|event| match event {
                        Ok(LlmStreamEvent::TextDelta(delta)) => Some(delta.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                Ok(chat_outcome(&reply))
            }
            ProviderBehavior::Error(error) => Err(error.clone()),
            ProviderBehavior::Delayed { reply, delay } => {
                tokio::time::sleep(*delay).await;
                Ok(chat_outcome(reply))
            }
        }
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req);
        match &self.behavior {
            ProviderBehavior::Reply(reply) => Ok(Box::pin(futures::stream::iter(vec![
                Ok(LlmStreamEvent::TextDelta(reply.clone())),
                Ok(LlmStreamEvent::Completed {
                    usage: None,
                    finish_reason: None,
                    fallback_used: false,
                }),
            ]))),
            ProviderBehavior::Stream(events) => {
                Ok(Box::pin(futures::stream::iter(events.to_vec())))
            }
            ProviderBehavior::Error(error) => Err(error.clone()),
            ProviderBehavior::Delayed { reply, delay } => {
                let reply = reply.clone();
                let delay = *delay;
                Ok(Box::pin(futures::stream::unfold(
                    (0_u8, reply, delay),
                    |(state, reply, delay)| async move {
                        if state == 0 {
                            tokio::time::sleep(delay).await;
                            return Some((
                                Ok(LlmStreamEvent::TextDelta(reply)),
                                (1, String::new(), delay),
                            ));
                        }
                        if state == 1 {
                            return Some((
                                Ok(LlmStreamEvent::Completed {
                                    usage: None,
                                    finish_reason: None,
                                    fallback_used: false,
                                }),
                                (2, String::new(), delay),
                            ));
                        }
                        None
                    },
                )))
            }
        }
    }

    fn tool_calling_protocol(&self, _model: Option<&str>) -> Option<ToolCallingProtocol> {
        self.tool_protocol
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(req.chat);
        match &self.behavior {
            ProviderBehavior::Reply(reply) => Ok(chat_outcome(reply)),
            ProviderBehavior::Stream(events) => {
                let reply = events
                    .iter()
                    .filter_map(|event| match event {
                        Ok(LlmStreamEvent::TextDelta(delta)) => Some(delta.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                Ok(chat_outcome(&reply))
            }
            ProviderBehavior::Error(error) => Err(error.clone()),
            ProviderBehavior::Delayed { reply, delay } => {
                tokio::time::sleep(*delay).await;
                Ok(chat_outcome(reply))
            }
        }
    }

    fn name(&self) -> &str {
        "test-provider"
    }

    fn model(&self) -> &str {
        "test-model"
    }

    fn stream_enabled(&self) -> bool {
        self.stream_enabled
    }
}

struct EmptyQueryExecutor;

#[async_trait::async_trait]
impl QueryExecutor for EmptyQueryExecutor {
    async fn query(&self, _req: QueryRequest) -> Result<QueryOutcome, LlmError> {
        Err(LlmError::provider("query unused", "query"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-query"
    }
}

struct EmptyWeatherExecutor;

#[async_trait::async_trait]
impl WeatherExecutor for EmptyWeatherExecutor {
    async fn weather(&self, _req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
        Err(LlmError::provider("weather unused", "weather"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-weather"
    }
}

struct EmptyTrainExecutor;

#[async_trait::async_trait]
impl TrainExecutor for EmptyTrainExecutor {
    async fn query_train_schedule(
        &self,
        _req: TrainScheduleRequest,
    ) -> Result<TrainSchedule, LlmError> {
        Err(LlmError::provider("train unused", "train"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-train"
    }
}

struct EmptyRadarExecutor;

#[async_trait::async_trait]
impl RadarExecutor for EmptyRadarExecutor {
    async fn radar(&self, _target: RadarTarget) -> Result<RadarSnapshot, LlmError> {
        Err(LlmError::provider("radar unused", "radar"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-radar"
    }
}

pub(super) fn private_request(text: &str) -> CoreRequest {
    CoreRequest {
        text: text.to_owned(),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        conversation: CoreConversation::Private {
            peer_id: "u1".to_owned(),
        },
    }
}

pub(super) fn private_scope() -> &'static str {
    "platform:qq_official:account:app-1:private:u1"
}

pub(super) fn group_request(text: &str) -> CoreRequest {
    CoreRequest {
        text: text.to_owned(),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: Some("app-1".to_owned()),
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        conversation: CoreConversation::Group {
            group_id: "g1".to_owned(),
        },
    }
}

/// 构造命中 `@当前机器人` mention 的群聊请求，用于验证群聊显式 WebSearch 路径。
pub(super) fn group_request_with_self_mention(text: &str) -> CoreRequest {
    let mut request = group_request(text);
    request.mentions = vec![MentionIdentity {
        raw_text: Some("@当前机器人".to_owned()),
        target: MessageActorContext::default(),
        is_self: true,
        confidence: MentionConfidence::Event,
    }];
    request
}

pub(super) fn wechat_service_request(text: &str) -> CoreRequest {
    CoreRequest {
        text: text.to_owned(),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::WechatService,
        account_id: Some("gh-service".to_owned()),
        actor: CoreActor {
            user_id: Some("openid-u1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        conversation: CoreConversation::ServiceAccount {
            account_id: Some("gh_test".to_owned()),
            peer_id: "openid-u1".to_owned(),
        },
    }
}

fn chat_outcome(reply: &str) -> ChatOutcome {
    ChatOutcome {
        reply: reply.to_owned(),
        metrics: LlmMetrics {
            provider: "test-provider".to_owned(),
            model: "test-model".to_owned(),
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
        executed_tools: Vec::new(),
        tool_results: Vec::new(),
    }
}

pub(super) fn test_state(provider: TestProvider, request_timeout_seconds: u64) -> CoreRuntimeState {
    test_state_with_tool_calling(provider, request_timeout_seconds, false)
}

pub(super) fn test_state_with_tool_calling(
    provider: TestProvider,
    request_timeout_seconds: u64,
    tool_calling_enabled: bool,
) -> CoreRuntimeState {
    test_state_with_group_tool_calling(
        provider,
        request_timeout_seconds,
        tool_calling_enabled,
        false,
    )
}

pub(super) fn test_state_with_group_tool_calling(
    provider: TestProvider,
    request_timeout_seconds: u64,
    tool_calling_enabled: bool,
    tool_calling_group_enabled: bool,
) -> CoreRuntimeState {
    let base_dir = std::env::temp_dir().join(format!(
        "qq-maid-core-service-test-{}",
        uuid::Uuid::new_v4()
    ));
    let prompt_dir = base_dir.join("prompts");
    fs::create_dir_all(&prompt_dir).unwrap();
    for file_name in crate::runtime::prompt::PROMPT_FILES {
        fs::write(prompt_dir.join(file_name), format!("{file_name} content")).unwrap();
    }
    let app_db_file = base_dir.join("app.db");
    let database = SqliteDatabase::open(&app_db_file, APP_MIGRATIONS).unwrap();
    let knowledge_dir = base_dir.join("knowledge");
    let knowledge_index =
        KnowledgeIndex::new(KnowledgeStore::new(database.clone()), &knowledge_dir);
    knowledge_index.sync().unwrap();
    let upstream_status = UpstreamStatus::default();

    CoreRuntimeState {
        config: AppConfig {
            provider: ProviderMode::OpenAi,
            model: "test-model".to_owned(),
            model_route: ModelRoute::parse_config("test-model", "LLM_MODEL").unwrap(),
            agent_config: crate::config::AgentRuntimeConfig::from_legacy(
                crate::config::LegacyAgentConfig {
                    main_model: "test-model".to_owned(),
                    max_output_tokens: 1200,
                    openai_search_model: "test-search".to_owned(),
                    tool_calling_enabled,
                    group_tool_calling_enabled: tool_calling_group_enabled,
                    tool_calling_max_rounds: 3,
                    group_llm_model: None,
                    private_llm_model: None,
                    group_openai_search_model: None,
                    private_openai_search_model: None,
                },
            )
            .unwrap(),
            title_model: None,
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
            openai_search_model: "test-search".to_owned(),
            openai_api_key: Some("test".to_owned()),
            openai_base_url: None,
            openai_api_mode: OpenAiApiMode::Auto,
            deepseek_api_key: None,
            deepseek_base_url: DEFAULT_DEEPSEEK_BASE_URL.to_owned(),
            deepseek_model: "deepseek-chat".to_owned(),
            bigmodel_api_key: None,
            bigmodel_base_url: DEFAULT_BIGMODEL_BASE_URL.to_owned(),
            bigmodel_model: "glm-5.2".to_owned(),
            stream: false,
            request_timeout_seconds,
            ttft_warn_seconds: 30,
            media_max_bytes: crate::config::DEFAULT_MEDIA_MAX_BYTES,
            max_output_tokens: 1200,
            max_concurrent_responses: 4,
            tool_calling_enabled,
            tool_calling_group_enabled,
            tool_calling_max_rounds: 3,
            context_budget: qq_maid_llm::context_budget::ContextBudgetConfig {
                context_window_chars: crate::config::DEFAULT_AGENT_CONTEXT_CHAR_LIMIT as usize,
                output_reserve_chars: crate::config::DEFAULT_AGENT_CONTEXT_OUTPUT_RESERVE_CHARS
                    as usize,
                protected_recent_turns: crate::config::DEFAULT_AGENT_CONTEXT_PROTECTED_RECENT_TURNS
                    as usize,
            },
            tool_result_max_chars: crate::config::DEFAULT_AGENT_TOOL_RESULT_CHAR_LIMIT as usize,
            status_display_name: crate::config::DEFAULT_STATUS_DISPLAY_NAME.to_owned(),
            server_host: "127.0.0.1".to_owned(),
            server_port: 8787,
            app_db_file: app_db_file.to_string_lossy().into_owned(),
            sqlite_pool_size: crate::storage::database::DEFAULT_SQLITE_POOL_SIZE,
            rss_enabled: false,
            rss_poll_interval_seconds: 300,
            rss_http_timeout_seconds: 15,
            rss_max_body_bytes: 2 * 1024 * 1024,
            rss_max_push_per_feed: 3,
            rss_summary_max_chars: DEFAULT_RSS_SUMMARY_MAX_CHARS,
            rss_seen_retention: 500,
            rss_push_max_failures: 3,
            rss_push_message_type: "markdown".to_owned(),
            todo_daily_reminder_enabled: false,
            todo_daily_reminder_time: DailyReminderTime { hour: 9, minute: 0 },
            rss_allow_private_urls: true,
            prompt_dir: prompt_dir.to_string_lossy().into_owned(),
            prompt_dir_uses_builtin_defaults: false,
            knowledge_dir: knowledge_dir.to_string_lossy().into_owned(),
            qweather_api_key: "test".to_owned(),
            qweather_api_host: "https://api.qweather.com".to_owned(),
            qweather_geo_host: "https://geoapi.qweather.com".to_owned(),
            web_console_enabled: false,
            web_console_allowed_origins: Vec::new(),
        },
        provider: observe_provider(Arc::new(provider), upstream_status.clone()),
        upstream_status,
        executors: CoreExecutors {
            query_executor: Arc::new(EmptyQueryExecutor),
            weather_executor: Arc::new(EmptyWeatherExecutor),
            train_executor: Arc::new(EmptyTrainExecutor),
            radar_executor: Arc::new(EmptyRadarExecutor),
        },
        stores: CoreStores {
            memory_store: crate::runtime::memory::MemoryStore::new(database.clone()),
            session_store: SessionStore::new(database.clone()),
            todo_store: crate::runtime::todo::TodoStore::new(database.clone()),
            notification_store: crate::storage::notification::NotificationOutboxStore::new(
                database.clone(),
            ),
            rss_store: RssStore::new(database.clone()),
            display_name_store: crate::runtime::display_name::DisplayNameStore::new(database),
        },
        rss_fetcher: RssFetcher::new(RssFetchConfig {
            allow_private_networks: true,
            ..RssFetchConfig::default()
        })
        .unwrap(),
        knowledge_index,
        prompt_config: PromptConfig::new(prompt_dir),
    }
}

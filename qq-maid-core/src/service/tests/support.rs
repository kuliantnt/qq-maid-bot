use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use qq_maid_common::identity_context::IdentitySource;
use qq_maid_llm::{
    agent_loop::{
        AgentStep, AgentStepSession, AgentToolCall, AgentToolResult, ToolLoopProgressEvent,
        run_agent_loop_with_handle,
    },
    provider::{
        ChatOutcome, LlmProvider, LlmStream, LlmStreamEvent, ToolCallingProtocol, ToolChatRequest,
        status::{UpstreamStatus, observe_provider},
        types::{ChatRequest, TokenUsage},
    },
    web_search::{WebSearchExecutor, WebSearchOutcome, WebSearchRequest},
};

use crate::{
    app::{CoreExecutors, CoreRuntimeState, CoreStores},
    config::{
        AppConfig, DEFAULT_BIGMODEL_BASE_URL, DEFAULT_DEEPSEEK_BASE_URL,
        DEFAULT_RSS_SUMMARY_MAX_CHARS, DailyReminderTime, OpenAiApiMode,
    },
    error::LlmError,
    runtime::{
        prompt::PromptConfig,
        session::SessionStore,
        tools::{
            RadarExecutor, RadarSnapshot, RadarTarget,
            knowledge::{KnowledgeIndex, KnowledgeStore},
            rss::{RssFetchConfig, RssFetcher, RssStore},
            train::{TrainExecutor, TrainSchedule, TrainScheduleRequest},
            weather::{WeatherExecutor, WeatherOutcome, WeatherRequest},
        },
    },
    service::{
        CoreActor, CoreConversation, CoreError, CoreRequest, CoreRespondFailure, CoreRespondOutput,
        CoreResponse, CoreResponseEvent, CoreResponseStream, Platform,
    },
    storage::{APP_MIGRATIONS, database::SqliteDatabase},
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
    PartialThenDelayed { delta: String, delay: Duration },
    AgentWeatherThenFinal,
}

struct WeatherAgentSession {
    round: usize,
}

#[async_trait::async_trait]
impl AgentStepSession for WeatherAgentSession {
    fn provider(&self) -> &str {
        "test-provider"
    }

    fn model(&self) -> &str {
        "test-model"
    }

    async fn advance(
        &mut self,
        _results: &[AgentToolResult],
        _allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        self.round += 1;
        if self.round == 1 {
            return Ok(AgentStep::ToolCalls {
                calls: vec![AgentToolCall {
                    name: "get_weather".to_owned(),
                    call_id: "weather-1".to_owned(),
                    arguments: r#"{"city":"杭州","forecast_days":1}"#.to_owned(),
                }],
                usage: None,
            });
        }
        Ok(AgentStep::FinalAnswer {
            reply: "must not start after timeout".to_owned(),
            output_parts: Vec::new(),
            usage: None,
        })
    }
}

#[derive(Clone)]
pub(super) struct TestProvider {
    behavior: ProviderBehavior,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    pub(super) calls: Arc<AtomicUsize>,
    pub(super) tool_calls: Arc<AtomicUsize>,
    tool_protocol: Option<ToolCallingProtocol>,
    stream_enabled: bool,
    emit_tool_progress: bool,
    progress_tool_name: String,
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

    pub(super) fn partial_then_delayed(delta: &str, delay: Duration) -> Self {
        Self::new(ProviderBehavior::PartialThenDelayed {
            delta: delta.to_owned(),
            delay,
        })
        .with_stream_enabled(true)
    }

    pub(super) fn agent_weather_then_final() -> Self {
        Self::new(ProviderBehavior::AgentWeatherThenFinal)
    }

    fn new(behavior: ProviderBehavior) -> Self {
        Self {
            behavior,
            requests: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(AtomicUsize::new(0)),
            tool_calls: Arc::new(AtomicUsize::new(0)),
            tool_protocol: None,
            stream_enabled: false,
            emit_tool_progress: true,
            progress_tool_name: "mock_tool".to_owned(),
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

    pub(super) fn without_tool_progress(mut self) -> Self {
        self.emit_tool_progress = false;
        self
    }

    pub(super) fn with_progress_tool_name(mut self, tool_name: &str) -> Self {
        self.progress_tool_name = tool_name.to_owned();
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
            ProviderBehavior::PartialThenDelayed { delta, delay } => {
                tokio::time::sleep(*delay).await;
                Ok(chat_outcome(delta))
            }
            ProviderBehavior::AgentWeatherThenFinal => {
                unreachable!("agent behavior uses chat_with_tools")
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
            ProviderBehavior::PartialThenDelayed { delta, delay } => {
                let delta = delta.clone();
                let delay = *delay;
                Ok(Box::pin(futures::stream::unfold(
                    (0_u8, delta, delay),
                    |(state, delta, delay)| async move {
                        if state == 0 {
                            return Some((
                                Ok(LlmStreamEvent::TextDelta(delta)),
                                (1, String::new(), delay),
                            ));
                        }
                        if state == 1 {
                            tokio::time::sleep(delay).await;
                        }
                        None
                    },
                )))
            }
            ProviderBehavior::AgentWeatherThenFinal => {
                unreachable!("agent behavior uses chat_with_tools")
            }
        }
    }

    fn tool_calling_protocol(&self, _model: Option<&str>) -> Option<ToolCallingProtocol> {
        self.tool_protocol
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        if matches!(self.behavior, ProviderBehavior::AgentWeatherThenFinal) {
            return run_agent_loop_with_handle(
                Box::new(WeatherAgentSession { round: 0 }),
                req.tools,
                req.tool_context,
                req.max_rounds,
                req.progress_sink,
                req.final_delta_sink,
                req.run_handle,
            )
            .await;
        }
        let progress_sink = req.progress_sink.clone();
        self.requests.lock().unwrap().push(req.chat);
        if self.emit_tool_progress
            && let Some(sink) = progress_sink
        {
            sink(ToolLoopProgressEvent::ToolCallStarted {
                tool_name: self.progress_tool_name.clone(),
            })
            .await?;
            sink(ToolLoopProgressEvent::ToolCallFinished {
                tool_name: self.progress_tool_name.clone(),
            })
            .await?;
        }
        match &self.behavior {
            ProviderBehavior::Reply(reply) => Ok(chat_outcome(reply)),
            ProviderBehavior::Stream(events) => {
                let mut reply = String::new();
                for event in events {
                    match event {
                        Ok(LlmStreamEvent::TextDelta(delta)) => {
                            reply.push_str(delta);
                            if let Some(sink) = req.final_delta_sink.as_ref() {
                                sink(delta.clone()).await?;
                            }
                        }
                        Ok(LlmStreamEvent::OutputPart(_)) => {}
                        Ok(LlmStreamEvent::Completed { .. }) => {}
                        Err(error) => return Err(error.clone()),
                    }
                }
                Ok(chat_outcome(&reply))
            }
            ProviderBehavior::Error(error) => Err(error.clone()),
            ProviderBehavior::Delayed { reply, delay } => {
                tokio::time::sleep(*delay).await;
                Ok(chat_outcome(reply))
            }
            ProviderBehavior::PartialThenDelayed { delta, delay } => {
                if let Some(sink) = req.final_delta_sink.as_ref() {
                    sink(delta.clone()).await?;
                }
                tokio::time::sleep(*delay).await;
                Ok(chat_outcome(delta))
            }
            ProviderBehavior::AgentWeatherThenFinal => {
                unreachable!("agent behavior returned before mock progress")
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

struct EmptyWebSearchExecutor;

#[async_trait::async_trait]
impl WebSearchExecutor for EmptyWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Err(LlmError::provider("query unused", "query"))
    }

    fn provider_name(&self) -> &'static str {
        "empty-query"
    }
}

#[derive(Clone, Default)]
pub(super) struct MockWebSearchExecutor {
    requests: Arc<Mutex<Vec<WebSearchRequest>>>,
}

impl MockWebSearchExecutor {
    pub(super) fn requests(&self) -> Vec<WebSearchRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl WebSearchExecutor for MockWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        self.requests.lock().unwrap().push(req.clone());
        Ok(WebSearchOutcome {
            answer: format!("web answer: {}", req.query),
            sources: Vec::new(),
            provider: "mock-query".to_owned(),
            elapsed_ms: 1,
        })
    }

    fn provider_name(&self) -> &'static str {
        "mock-query"
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
        message_id: Some("test-private-message".to_owned()),
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
        message_id: Some("test-group-message".to_owned()),
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

pub(super) fn wechat_service_request(text: &str) -> CoreRequest {
    CoreRequest {
        text: text.to_owned(),
        message_id: Some("test-wechat-message".to_owned()),
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
        output_parts: Vec::new(),
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
        agent: Default::default(),
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
    test_state_with_group_tool_calling_and_query_executor(
        provider,
        request_timeout_seconds,
        tool_calling_enabled,
        tool_calling_group_enabled,
        Arc::new(EmptyWebSearchExecutor),
    )
}

pub(super) fn test_state_with_query_executor(
    provider: TestProvider,
    request_timeout_seconds: u64,
    query_executor: Arc<dyn WebSearchExecutor>,
) -> CoreRuntimeState {
    test_state_with_group_tool_calling_and_query_executor(
        provider,
        request_timeout_seconds,
        false,
        false,
        query_executor,
    )
}

fn test_state_with_group_tool_calling_and_query_executor(
    provider: TestProvider,
    request_timeout_seconds: u64,
    tool_calling_enabled: bool,
    tool_calling_group_enabled: bool,
    query_executor: Arc<dyn WebSearchExecutor>,
) -> CoreRuntimeState {
    let (database, base_dir) =
        SqliteDatabase::open_temp_directory("qq-maid-core-service-test", APP_MIGRATIONS).unwrap();
    let app_db_file = database.path().to_path_buf();
    let prompt_dir = base_dir.join("prompts");
    fs::create_dir_all(&prompt_dir).unwrap();
    for file_name in crate::runtime::prompt::PROMPT_FILES {
        fs::write(prompt_dir.join(file_name), format!("{file_name} content")).unwrap();
    }
    let knowledge_dir = base_dir.join("knowledge");
    let knowledge_index =
        KnowledgeIndex::new(KnowledgeStore::new(database.clone()), &knowledge_dir);
    knowledge_index.sync().unwrap();
    let upstream_status = UpstreamStatus::default();

    CoreRuntimeState {
        config: AppConfig {
            agent_config: crate::config::AgentRuntimeConfig::for_test(
                "test-model",
                "test-search",
                tool_calling_enabled,
                tool_calling_group_enabled,
                3,
            ),
            ops_config: crate::runtime::tools::ops::OpsConfig::default(),
            command_prefix: Default::default(),
            openai_api_key: Some("test".to_owned()),
            openai_base_url: None,
            openai_api_mode: OpenAiApiMode::Auto,
            deepseek_api_key: None,
            deepseek_base_url: DEFAULT_DEEPSEEK_BASE_URL.to_owned(),
            bigmodel_api_key: None,
            bigmodel_base_url: DEFAULT_BIGMODEL_BASE_URL.to_owned(),
            gemini_api_key: None,
            gemini_base_url: crate::config::DEFAULT_GEMINI_BASE_URL.to_owned(),
            stream: false,
            request_timeout_seconds,
            agent_finalization_reserve_seconds:
                crate::config::DEFAULT_AGENT_FINALIZATION_RESERVE_SECONDS,
            web_search_first_activity_timeout_seconds:
                crate::config::DEFAULT_WEB_SEARCH_FIRST_ACTIVITY_TIMEOUT_SECONDS,
            web_search_idle_timeout_seconds: crate::config::DEFAULT_WEB_SEARCH_IDLE_TIMEOUT_SECONDS,
            web_search_absolute_timeout_seconds:
                crate::config::DEFAULT_WEB_SEARCH_ABSOLUTE_TIMEOUT_SECONDS,
            ttft_warn_seconds: 30,
            media_max_bytes: crate::config::DEFAULT_MEDIA_MAX_BYTES,
            max_concurrent_responses: 4,
            context_budget: qq_maid_llm::context_budget::ContextBudgetConfig {
                context_window_chars: crate::config::DEFAULT_AGENT_CONTEXT_CHAR_LIMIT as usize,
                output_reserve_chars: crate::config::DEFAULT_AGENT_CONTEXT_OUTPUT_RESERVE_CHARS
                    as usize,
                protected_recent_turns: crate::config::DEFAULT_AGENT_CONTEXT_PROTECTED_RECENT_TURNS
                    as usize,
            },
            tool_result_max_chars: crate::config::DEFAULT_AGENT_TOOL_RESULT_CHAR_LIMIT as usize,
            bot_display_name: crate::config::DEFAULT_BOT_DISPLAY_NAME.to_owned(),
            server_host: "127.0.0.1".to_owned(),
            server_port: 8787,
            app_db_file: app_db_file.to_string_lossy().into_owned(),
            sqlite_pool_size: crate::storage::database::DEFAULT_SQLITE_POOL_SIZE,
            memory_consolidation_enabled: false,
            memory_dream_enabled: false,
            memory_consolidation_check_interval_seconds:
                crate::config::DEFAULT_MEMORY_CONSOLIDATION_CHECK_INTERVAL_SECONDS,
            memory_consolidation_min_interval_seconds:
                crate::config::DEFAULT_MEMORY_CONSOLIDATION_MIN_INTERVAL_SECONDS,
            memory_consolidation_min_new_records:
                crate::config::DEFAULT_MEMORY_CONSOLIDATION_MIN_NEW_RECORDS,
            memory_consolidation_min_distinct_sources:
                crate::config::DEFAULT_MEMORY_CONSOLIDATION_MIN_DISTINCT_SOURCES,
            memory_consolidation_max_records:
                crate::config::DEFAULT_MEMORY_CONSOLIDATION_MAX_RECORDS,
            memory_consolidation_max_input_chars:
                crate::config::DEFAULT_MEMORY_CONSOLIDATION_MAX_INPUT_CHARS,
            memory_dream_min_interval_seconds:
                crate::config::DEFAULT_MEMORY_DREAM_MIN_INTERVAL_SECONDS,
            memory_dream_min_new_sessions: crate::config::DEFAULT_MEMORY_DREAM_MIN_NEW_SESSIONS,
            memory_dream_max_sessions: crate::config::DEFAULT_MEMORY_DREAM_MAX_SESSIONS,
            memory_dream_max_input_chars: crate::config::DEFAULT_MEMORY_DREAM_MAX_INPUT_CHARS,
            memory_dream_max_output_memories:
                crate::config::DEFAULT_MEMORY_DREAM_MAX_OUTPUT_MEMORIES,
            rss_enabled: false,
            rss_translation_enabled: false,
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
            web_console_trusted_proxy_ips: Vec::new(),
            web_console_secure_cookies: false,
        },
        provider: observe_provider(Arc::new(provider), upstream_status.clone()),
        upstream_status,
        executors: CoreExecutors {
            query_executor,
            weather_executor: Arc::new(EmptyWeatherExecutor),
            train_executor: Arc::new(EmptyTrainExecutor),
            radar_executor: Arc::new(EmptyRadarExecutor),
        },
        stores: CoreStores {
            memory_store: crate::runtime::tools::memory::MemoryStore::new(database.clone()),
            session_store: SessionStore::new(database.clone()),
            todo_store: crate::runtime::tools::todo::TodoStore::new(database.clone()),
            notification_store: crate::storage::notification::NotificationOutboxStore::new(
                database.clone(),
            ),
            ops_execution_store: crate::runtime::tools::ops::OpsExecutionStore::new(
                database.clone(),
            ),
            ops_task_registry: crate::runtime::tools::ops::OpsTaskRegistry::default(),
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

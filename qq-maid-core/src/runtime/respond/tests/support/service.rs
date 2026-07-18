use super::*;

#[derive(Default)]
pub(crate) struct TestModelOptions {
    pub(crate) memory_model: Option<String>,
    pub(crate) compact_model: Option<String>,
    pub(crate) translation_model: Option<String>,
}

#[derive(Default)]
pub(crate) struct TestToolCallingOptions {
    pub(crate) enabled: bool,
    pub(crate) group_enabled: bool,
    pub(crate) group_enabled_tools: Option<Vec<String>>,
    pub(crate) memory_dream: Option<crate::runtime::tools::memory::MemoryDreamConfig>,
}

pub(crate) fn test_service() -> RustRespondService {
    test_service_with_provider(MockProvider::new())
}

pub(crate) fn test_service_with_bot_display_name(name: &str) -> RustRespondService {
    let mut service = test_service();
    service.bot_display_name = name.to_owned();
    service
}

pub(crate) fn test_service_with_command_prefix(prefix: &str) -> RustRespondService {
    let mut service = test_service();
    service.command_prefix = qq_maid_common::command_prefix::CommandPrefix::parse(prefix).unwrap();
    service
}

pub(crate) fn test_service_with_provider(provider: MockProvider) -> RustRespondService {
    test_service_with_provider_and_base(provider).0
}

pub(crate) fn test_service_with_provider_and_tool_calling(
    provider: MockProvider,
    tool_calling_enabled: bool,
) -> RustRespondService {
    test_service_with_provider_and_group_tool_calling(provider, tool_calling_enabled, false)
}

pub(crate) fn test_service_with_provider_and_group_tool_calling(
    provider: MockProvider,
    tool_calling_enabled: bool,
    tool_calling_group_enabled: bool,
) -> RustRespondService {
    test_service_with_provider_and_group_tool_calling_tools(
        provider,
        tool_calling_enabled,
        tool_calling_group_enabled,
        None,
    )
}

pub(crate) fn test_service_with_provider_and_group_tool_calling_tools(
    provider: MockProvider,
    tool_calling_enabled: bool,
    tool_calling_group_enabled: bool,
    group_enabled_tools: Option<Vec<String>>,
) -> RustRespondService {
    test_service_with_provider_base_title_query_weather_train_models_and_options(
        provider,
        None,
        Arc::new(MockWebSearchExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(MockTrainExecutor::new()),
        TestModelOptions {
            memory_model: None,
            compact_model: None,
            translation_model: None,
        },
        TestToolCallingOptions {
            enabled: tool_calling_enabled,
            group_enabled: tool_calling_group_enabled,
            group_enabled_tools,
            memory_dream: None,
        },
    )
    .0
}

pub(crate) fn test_service_with_provider_and_memory_dream(
    provider: MockProvider,
    memory_dream: crate::runtime::tools::memory::MemoryDreamConfig,
) -> RustRespondService {
    test_service_with_provider_base_title_query_weather_train_models_and_options(
        provider,
        None,
        Arc::new(MockWebSearchExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(MockTrainExecutor::new()),
        TestModelOptions {
            memory_model: Some("mock-dream".to_owned()),
            compact_model: None,
            translation_model: None,
        },
        TestToolCallingOptions {
            memory_dream: Some(memory_dream),
            ..TestToolCallingOptions::default()
        },
    )
    .0
}

pub(crate) fn test_service_with_base() -> (RustRespondService, PathBuf) {
    test_service_with_provider_and_base(MockProvider::new())
}

pub(crate) fn test_service_with_provider_and_base(
    provider: MockProvider,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_and_base_and_title(provider, None)
}

pub(crate) fn test_service_with_provider_and_base_and_title(
    provider: MockProvider,
    title_model: Option<String>,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_base_title_and_query(
        provider,
        title_model,
        Arc::new(MockWebSearchExecutor),
    )
}

pub(crate) fn test_service_with_provider_base_title_and_query(
    provider: MockProvider,
    title_model: Option<String>,
    query_executor: Arc<dyn WebSearchExecutor>,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_base_title_query_and_models(
        provider,
        title_model,
        query_executor,
        Arc::new(MockWeatherExecutor::new()),
        None,
        None,
    )
}

pub(crate) fn test_service_with_provider_base_title_query_and_models(
    provider: MockProvider,
    title_model: Option<String>,
    query_executor: Arc<dyn WebSearchExecutor>,
    weather_executor: Arc<dyn WeatherExecutor>,
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
            memory_model,
            compact_model,
            translation_model: None,
        },
    )
}

pub(crate) fn test_service_with_provider_base_title_query_weather_train_and_models(
    provider: MockProvider,
    title_model: Option<String>,
    query_executor: Arc<dyn WebSearchExecutor>,
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
            memory_model: models.memory_model,
            compact_model: models.compact_model,
            translation_model: models.translation_model,
        },
    )
}

pub(crate) fn test_service_with_aux_model(
    provider: MockProvider,
    translation_model: Option<String>,
) -> RustRespondService {
    test_service_with_provider_base_title_query_weather_and_models(
        provider,
        None,
        Arc::new(MockWebSearchExecutor),
        Arc::new(MockWeatherExecutor::new()),
        Arc::new(MockTrainExecutor::new()),
        TestModelOptions {
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
    query_executor: Arc<dyn WebSearchExecutor>,
    weather_executor: Arc<dyn WeatherExecutor>,
    train_executor: Arc<dyn TrainExecutor>,
    models: TestModelOptions,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_base_title_query_weather_train_models_and_options(
        provider,
        title_model,
        query_executor,
        weather_executor,
        train_executor,
        models,
        TestToolCallingOptions::default(),
    )
}

pub(crate) fn test_service_with_provider_base_title_query_weather_train_models_and_options(
    provider: MockProvider,
    title_model: Option<String>,
    query_executor: Arc<dyn WebSearchExecutor>,
    weather_executor: Arc<dyn WeatherExecutor>,
    train_executor: Arc<dyn TrainExecutor>,
    models: TestModelOptions,
    tool_calling: TestToolCallingOptions,
) -> (RustRespondService, PathBuf) {
    let (database, base) =
        SqliteDatabase::open_temp_directory("qq-maid-respond", APP_MIGRATIONS).unwrap();
    let prompt_dir = base.join("prompts");
    write_prompt_set(&prompt_dir);
    let knowledge_dir = base.join("knowledge");
    let knowledge_index = KnowledgeIndex::new(KnowledgeStore::new(database.clone()), knowledge_dir);
    knowledge_index.sync().unwrap();
    let auxiliary_model = models
        .memory_model
        .as_deref()
        .or(models.compact_model.as_deref())
        .or(models.translation_model.as_deref())
        .or(title_model.as_deref());
    let service = RustRespondService::new(
        Arc::new(provider),
        RespondExecutors {
            query_executor,
            weather_executor,
            train_executor,
            radar_executor: Arc::new(MockRadarExecutor::new()),
        },
        RespondStores {
            memory_store: MemoryStore::new(database.clone()),
            session_store: SessionStore::new(database.clone()),
            task_store: TodoStore::new(database.clone()),
            notification_store: crate::storage::notification::NotificationOutboxStore::new(
                database.clone(),
            ),
            ops_execution_store: crate::runtime::tools::ops::OpsExecutionStore::new(
                database.clone(),
            ),
            ops_task_registry: crate::runtime::tools::ops::OpsTaskRegistry::default(),
            rss_store: RssStore::new(database.clone()),
            display_name_store: crate::runtime::display_name::DisplayNameStore::new(
                database.clone(),
            ),
        },
        RssFetcher::new(RssFetchConfig {
            allow_private_networks: true,
            ..RssFetchConfig::default()
        })
        .unwrap(),
        knowledge_index,
        PromptConfig::new(prompt_dir),
        RespondServiceOptions {
            memory_dream: tool_calling.memory_dream.unwrap_or(
                crate::runtime::tools::memory::MemoryDreamConfig {
                    enabled: false,
                    min_interval_seconds: 0,
                    min_new_sessions: 1,
                    max_sessions: 20,
                    max_input_chars: 32_000,
                    max_output_memories: 8,
                },
            ),
            rss_summary_max_chars: DEFAULT_RSS_SUMMARY_MAX_CHARS as usize,
            rss_seen_retention: 500,
            context_budget: qq_maid_llm::context_budget::ContextBudgetConfig {
                context_window_chars: crate::config::DEFAULT_AGENT_CONTEXT_CHAR_LIMIT as usize,
                output_reserve_chars: crate::config::DEFAULT_AGENT_CONTEXT_OUTPUT_RESERVE_CHARS
                    as usize,
                protected_recent_turns: crate::config::DEFAULT_AGENT_CONTEXT_PROTECTED_RECENT_TURNS
                    as usize,
            },
            tool_result_max_chars: crate::config::DEFAULT_AGENT_TOOL_RESULT_CHAR_LIMIT as usize,
            web_search_timeouts: crate::runtime::tools::WebSearchTimeouts::default(),
            bot_display_name: crate::config::DEFAULT_BOT_DISPLAY_NAME.to_owned(),
            agent_config: {
                let mut config =
                    test_agent_config(tool_calling.enabled, tool_calling.group_enabled);
                if let Some(auxiliary_model) = auxiliary_model {
                    config = config.with_scene_models_for_test(
                        "mock-model",
                        Some(auxiliary_model),
                        "mock-model",
                        Some(auxiliary_model),
                    );
                }
                if let Some(tools) = tool_calling.group_enabled_tools.as_ref() {
                    let refs = tools.iter().map(String::as_str).collect::<Vec<_>>();
                    config.with_group_enabled_tools_for_test(&refs)
                } else {
                    config
                }
            },
            ops_config: crate::runtime::tools::ops::OpsConfig::default(),
            command_prefix: Default::default(),
        },
    );
    (service, base)
}

pub(crate) fn test_agent_config(
    tool_calling_enabled: bool,
    group_tool_calling_enabled: bool,
) -> crate::config::AgentRuntimeConfig {
    crate::config::AgentRuntimeConfig::for_test(
        "mock-model",
        "mock-search-model",
        tool_calling_enabled,
        group_tool_calling_enabled,
        3,
    )
}

pub(crate) fn test_service_with_title_provider(
    provider: MockProvider,
) -> (RustRespondService, PathBuf) {
    test_service_with_provider_and_base_and_title(provider, Some("title-model".to_owned()))
}

pub(crate) fn test_service_with_title_provider_and_agent_config(
    provider: MockProvider,
    title_model: Option<String>,
    agent_config: crate::config::AgentRuntimeConfig,
) -> (RustRespondService, PathBuf) {
    let (mut service, base) = test_service_with_provider_and_base_and_title(provider, title_model);
    service.translation_service =
        crate::runtime::translation::TranslationService::new(service.provider.clone(), None)
            .with_agent_config(agent_config.clone());
    service.agent_config = agent_config;
    (service, base)
}

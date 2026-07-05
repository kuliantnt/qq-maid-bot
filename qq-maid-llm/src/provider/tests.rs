use super::*;
use crate::{
    config::{
        HttpAuthConfig, LlmConfig, OpenAiApiMode, OpenAiCompatibleProviderConfig, ProviderMode,
    },
    metrics::LlmMetrics,
    provider::types::{ChatMessage, ChatRequest, ModelId},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
struct MockProvider {
    name: &'static str,
    model: &'static str,
    stream: bool,
    results: Arc<Mutex<Vec<Result<ChatOutcome, LlmError>>>>,
    stream_results: Arc<Mutex<Vec<Result<LlmStream, LlmError>>>>,
    calls: Arc<Mutex<usize>>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    tool_protocol: Option<ToolCallingProtocol>,
    tool_results: Arc<Mutex<Vec<Result<ChatOutcome, LlmError>>>>,
    tool_calls: Arc<Mutex<usize>>,
    tool_requests: Arc<Mutex<Vec<ToolChatRequest>>>,
}

impl MockProvider {
    fn new(name: &'static str, results: Vec<Result<ChatOutcome, LlmError>>) -> Self {
        Self {
            name,
            model: "mock-model",
            stream: false,
            results: Arc::new(Mutex::new(results)),
            stream_results: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(Mutex::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
            tool_protocol: None,
            tool_results: Arc::new(Mutex::new(Vec::new())),
            tool_calls: Arc::new(Mutex::new(0)),
            tool_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_streams(name: &'static str, results: Vec<Result<LlmStream, LlmError>>) -> Self {
        Self {
            name,
            model: "mock-model",
            stream: true,
            results: Arc::new(Mutex::new(Vec::new())),
            stream_results: Arc::new(Mutex::new(results)),
            calls: Arc::new(Mutex::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
            tool_protocol: None,
            tool_results: Arc::new(Mutex::new(Vec::new())),
            tool_calls: Arc::new(Mutex::new(0)),
            tool_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_tool_protocol(mut self, protocol: ToolCallingProtocol) -> Self {
        self.tool_protocol = Some(protocol);
        self
    }

    fn with_tool_results(self, results: Vec<Result<ChatOutcome, LlmError>>) -> Self {
        *self.tool_results.lock().unwrap() = results;
        self
    }

    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn tool_calls(&self) -> usize {
        *self.tool_calls.lock().unwrap()
    }

    fn tool_requests(&self) -> Vec<ToolChatRequest> {
        self.tool_requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        *self.calls.lock().unwrap() += 1;
        self.requests.lock().unwrap().push(req);
        self.results.lock().unwrap().remove(0)
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        *self.calls.lock().unwrap() += 1;
        self.requests.lock().unwrap().push(req);
        self.stream_results.lock().unwrap().remove(0)
    }

    fn tool_calling_protocol(&self, _model: Option<&str>) -> Option<ToolCallingProtocol> {
        self.tool_protocol
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        *self.tool_calls.lock().unwrap() += 1;
        self.tool_requests.lock().unwrap().push(req);
        self.tool_results.lock().unwrap().remove(0)
    }

    fn name(&self) -> &str {
        self.name
    }

    fn model(&self) -> &str {
        self.model
    }

    fn stream_enabled(&self) -> bool {
        self.stream
    }
}

fn request() -> ChatRequest {
    ChatRequest {
        session_id: "group:g1".to_owned(),
        model: None,
        messages: vec![ChatMessage::user("hi")],
        context_budget: None,
        max_output_tokens: None,
        reasoning_effort: None,
        metadata: HashMap::new(),
    }
}

fn tool_request() -> ToolChatRequest {
    ToolChatRequest {
        chat: request(),
        tools: ToolRegistry::new(),
        tool_context: crate::tool::ToolContext {
            task_id: "task-1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            tool_call_id: None,
        },
        max_rounds: 3,
    }
}

fn outcome(reply: &str) -> ChatOutcome {
    ChatOutcome {
        reply: reply.to_owned(),
        metrics: LlmMetrics {
            provider: "mock".to_owned(),
            model: "mock-model".to_owned(),
            stream: false,
            ttfe_ms: None,
            ttft_ms: None,
            total_latency_ms: 1,
        },
        usage: None,
        fallback_used: false,
        executed_tools: Vec::new(),
        tool_results: Vec::new(),
    }
}

fn stream_events(events: Vec<Result<LlmStreamEvent, LlmError>>) -> LlmStream {
    Box::pin(stream::iter(events))
}

fn app_config(provider: ProviderMode, model: &str) -> LlmConfig {
    let model_route = ModelRoute::parse_config(model, "LLM_MODEL").unwrap();
    LlmConfig {
        provider,
        model_route: model_route.clone(),
        configured_model_routes: vec![("LLM_MODEL".to_owned(), model_route)],
        openai_search_model: "gpt-5.5".to_owned(),
        openai_api_key: Some("test-openai-key".to_owned()),
        openai_base_url: None,
        openai_api_mode: OpenAiApiMode::Auto,
        deepseek_api_key: None,
        deepseek_base_url: "https://api.deepseek.com".to_owned(),
        deepseek_model: "deepseek:deepseek-chat".to_owned(),
        bigmodel_api_key: None,
        bigmodel_base_url: "https://open.bigmodel.cn/api/paas/v4".to_owned(),
        bigmodel_model: "bigmodel:glm-5.2".to_owned(),
        openai_compatible_providers: Vec::new(),
        stream: true,
        request_timeout_seconds: 90,
        media_max_bytes: 10 * 1024 * 1024,
        max_output_tokens: 1200,
    }
}

fn set_configured_route(config: &mut LlmConfig, name: &'static str, value: &str) {
    let route = ModelRoute::parse_config(value, name).unwrap();
    if let Some((_, existing)) = config
        .configured_model_routes
        .iter_mut()
        .find(|(existing_name, _)| existing_name == name)
    {
        *existing = route;
    } else {
        config
            .configured_model_routes
            .push((name.to_owned(), route));
    }
}

fn mimo_provider_config(api_key: Option<&str>) -> OpenAiCompatibleProviderConfig {
    OpenAiCompatibleProviderConfig {
        id: ModelProvider::Custom("mimo".to_owned()),
        base_url: "https://api.xiaomimimo.com/v1".to_owned(),
        api_key_env: "MIMO_API_KEY".to_owned(),
        api_key: api_key.map(str::to_owned),
        auth: HttpAuthConfig::default(),
        request_timeout_seconds: None,
    }
}

fn auto_required_provider_kinds(config: &LlmConfig) -> Result<Vec<ModelProvider>, LlmError> {
    let route = auto_default_route(config)?;
    let provider_routes = auto_provider_routes(config, &route)?;
    Ok(available_provider_kinds_for_routes(
        config,
        &provider_routes,
        &ModelProvider::OpenAi,
    ))
}

#[test]
fn model_id_parse_accepts_custom_mimo_provider_prefix() {
    let model = ModelId::parse_config("mimo:mimo-v2.5-pro", "LLM_MODEL").unwrap();

    assert_eq!(
        model.provider,
        Some(ModelProvider::Custom("mimo".to_owned()))
    );
    assert_eq!(model.name, "mimo-v2.5-pro");
    assert_eq!(model.to_request_model(), "mimo:mimo-v2.5-pro");
}

fn route_provider(
    route: &str,
    openai_results: Vec<Result<ChatOutcome, LlmError>>,
    deepseek_results: Vec<Result<ChatOutcome, LlmError>>,
) -> (ModelRouteProvider, Arc<MockProvider>, Arc<MockProvider>) {
    let openai = Arc::new(MockProvider::new("openai", openai_results));
    let deepseek = Arc::new(MockProvider::new("deepseek", deepseek_results));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config(route, "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();
    (provider, openai, deepseek)
}

#[test]
fn auto_default_route_appends_deepseek_fallback_for_single_openai_model() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());

    let route = auto_default_route(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(
        route.display(),
        "openai:gpt-5.4-mini,deepseek:deepseek-chat"
    );
    assert_eq!(
        provider.model(),
        "openai:gpt-5.4-mini,deepseek:deepseek-chat"
    );
}

#[test]
fn auto_default_route_keeps_explicit_candidate_order() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());

    let route = auto_default_route(&config).unwrap();

    assert_eq!(route.display(), "openai:gpt-5.4-mini,openai:gpt-5.4");
}

#[test]
fn auto_provider_set_includes_deepseek_from_translation_model() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    set_configured_route(&mut config, "TRANSLATION_MODEL", "deepseek:deepseek-chat");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(
        providers,
        vec![ModelProvider::OpenAi, ModelProvider::DeepSeek]
    );
    assert_eq!(provider.model(), "openai:gpt-5.4-mini,openai:gpt-5.4");
}

#[test]
fn auto_provider_set_includes_bigmodel_from_translation_model() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
    config.bigmodel_api_key = Some("test-bigmodel-key".to_owned());
    set_configured_route(&mut config, "TRANSLATION_MODEL", "bigmodel:glm-5.2");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(
        providers,
        vec![ModelProvider::OpenAi, ModelProvider::BigModel]
    );
    assert_eq!(provider.model(), "openai:gpt-5.4-mini");
}

#[test]
fn auto_provider_set_includes_specialty_deepseek_with_explicit_openai_main_chain() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    set_configured_route(
        &mut config,
        "TRANSLATION_MODEL",
        "deepseek:deepseek-chat,openai:gpt-5.4-mini",
    );

    let default_route = auto_default_route(&config).unwrap();
    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(
        default_route.display(),
        "openai:gpt-5.4-mini,openai:gpt-5.4"
    );
    assert_eq!(
        providers,
        vec![ModelProvider::OpenAi, ModelProvider::DeepSeek]
    );
    assert_eq!(provider.model(), "openai:gpt-5.4-mini,openai:gpt-5.4");
}

#[test]
fn auto_provider_set_skips_specialty_deepseek_without_api_key() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
    set_configured_route(&mut config, "TRANSLATION_MODEL", "deepseek:deepseek-chat");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::OpenAi]);
    assert_eq!(provider.name(), "auto");
}

#[test]
fn auto_provider_set_skips_bigmodel_without_api_key() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
    set_configured_route(&mut config, "TRANSLATION_MODEL", "bigmodel:glm-5.2");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::OpenAi]);
    assert_eq!(provider.name(), "auto");
}

#[test]
fn auto_provider_set_keeps_openai_only_without_deepseek_key() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
    set_configured_route(&mut config, "TITLE_MODEL", "openai:gpt-5.4-mini");
    set_configured_route(&mut config, "TRANSLATION_MODEL", "openai:gpt-5.4-mini");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::OpenAi]);
    assert_eq!(provider.name(), "auto");
    assert_eq!(provider.model(), "openai:gpt-5.4-mini");
}

#[test]
fn auto_provider_set_deduplicates_repeated_specialty_providers() {
    let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    set_configured_route(&mut config, "TITLE_MODEL", "deepseek:deepseek-chat");
    set_configured_route(
        &mut config,
        "TODO_MODEL",
        "deepseek:deepseek-chat,openai:gpt-5.4-mini",
    );
    set_configured_route(&mut config, "MEMORY_MODEL", "deepseek:deepseek-chat");
    set_configured_route(&mut config, "TRANSLATION_MODEL", "deepseek:deepseek-chat");

    let providers = auto_required_provider_kinds(&config).unwrap();

    assert_eq!(
        providers,
        vec![ModelProvider::OpenAi, ModelProvider::DeepSeek]
    );
}

#[test]
fn auto_deepseek_only_does_not_require_openai_provider() {
    let mut config = app_config(ProviderMode::Auto, "deepseek:deepseek-chat");
    config.openai_api_key = None;
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::DeepSeek]);
    assert_eq!(provider.name(), "auto");
    assert_eq!(provider.model(), "deepseek:deepseek-chat");
}

#[test]
fn auto_deepseek_only_agent_routes_do_not_initialize_openai() {
    let mut config = app_config(ProviderMode::Auto, "deepseek:deepseek-chat");
    config.openai_api_key = None;
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    set_configured_route(
        &mut config,
        "agent.model_routes.private_main",
        "deepseek:deepseek-chat",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.group_main",
        "deepseek:deepseek-chat",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.aux",
        "deepseek:deepseek-chat",
    );

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::DeepSeek]);
    assert_eq!(provider.name(), "auto");
    assert_eq!(provider.model(), "deepseek:deepseek-chat");
}

#[test]
fn auto_bigmodel_only_agent_routes_do_not_initialize_openai() {
    let mut config = app_config(ProviderMode::Auto, "bigmodel:glm-5.2");
    config.openai_api_key = None;
    config.bigmodel_api_key = Some("test-bigmodel-key".to_owned());
    set_configured_route(
        &mut config,
        "agent.model_routes.private_main",
        "bigmodel:glm-5.2",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.group_main",
        "bigmodel:glm-5.2",
    );
    set_configured_route(&mut config, "agent.model_routes.aux", "bigmodel:glm-5.2");

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::BigModel]);
    assert_eq!(provider.name(), "auto");
    assert_eq!(provider.model(), "bigmodel:glm-5.2");
}

#[test]
fn auto_provider_set_includes_configured_mimo_provider() {
    let mut config = app_config(
        ProviderMode::Auto,
        "mimo:mimo-v2.5-pro,deepseek:deepseek-chat",
    );
    config.openai_api_key = None;
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    config
        .openai_compatible_providers
        .push(mimo_provider_config(Some("test-mimo-key")));

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(
        providers,
        vec![
            ModelProvider::DeepSeek,
            ModelProvider::Custom("mimo".to_owned())
        ]
    );
    assert_eq!(provider.name(), "auto");
    assert_eq!(
        provider.model(),
        "mimo:mimo-v2.5-pro,deepseek:deepseek-chat"
    );
}

#[test]
fn auto_provider_set_skips_mimo_without_api_key() {
    let mut config = app_config(
        ProviderMode::Auto,
        "mimo:mimo-v2.5-pro,deepseek:deepseek-chat",
    );
    config.openai_api_key = None;
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    config
        .openai_compatible_providers
        .push(mimo_provider_config(None));

    let providers = auto_required_provider_kinds(&config).unwrap();
    let provider = build_provider(&config).unwrap();

    assert_eq!(providers, vec![ModelProvider::DeepSeek]);
    assert_eq!(provider.name(), "auto");
}

#[test]
fn auto_provider_rejects_undeclared_custom_provider() {
    let mut config = app_config(
        ProviderMode::Auto,
        "mimmo:mimo-v2.5-pro,deepseek:deepseek-chat",
    );
    config.openai_api_key = None;
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject undeclared custom provider"),
        Err(err) => err,
    };

    assert_eq!(err.stage, "config");
    assert!(
        err.message.contains("providers.mimmo is not configured"),
        "{}",
        err.message
    );
}

#[test]
fn auto_requires_at_least_one_referenced_provider_api_key() {
    let mut config = app_config(ProviderMode::Auto, "deepseek:deepseek-chat");
    config.openai_api_key = None;
    config.deepseek_api_key = None;
    config.bigmodel_api_key = Some("unused-bigmodel-key".to_owned());

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject auto routes with no available provider"),
        Err(err) => err,
    };

    assert_eq!(err.code, "config");
    assert!(err.message.contains("no LLM provider is available"));
    assert!(!err.message.contains("BIGMODEL_API_KEY"));
}

#[test]
fn fixed_provider_modes_validate_specialty_routes_at_startup() {
    let mut config = app_config(ProviderMode::OpenAi, "openai:gpt-5.4-mini");
    set_configured_route(&mut config, "TRANSLATION_MODEL", "deepseek:deepseek-chat");

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject cross-provider specialty route"),
        Err(err) => err,
    };

    assert_eq!(err.code, "config");
    assert!(err.message.contains("TRANSLATION_MODEL"));
    assert!(err.message.contains("requires provider `deepseek`"));
}

#[test]
fn fixed_deepseek_provider_accepts_deepseek_only_agent_routes() {
    let mut config = app_config(ProviderMode::DeepSeek, "deepseek:deepseek-chat");
    config.deepseek_api_key = Some("test-deepseek-key".to_owned());
    set_configured_route(
        &mut config,
        "agent.model_routes.private_main",
        "deepseek:deepseek-chat",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.group_main",
        "deepseek:deepseek-chat",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.aux",
        "deepseek:deepseek-chat",
    );

    let provider = build_provider(&config).unwrap();

    assert_eq!(provider.name(), "deepseek");
    assert_eq!(provider.model(), "deepseek:deepseek-chat");
}

#[test]
fn fixed_bigmodel_provider_validates_specialty_routes_at_startup() {
    let mut config = app_config(ProviderMode::BigModel, "bigmodel:glm-5.2");
    config.bigmodel_api_key = Some("test-bigmodel-key".to_owned());
    set_configured_route(&mut config, "TRANSLATION_MODEL", "openai:gpt-5.4-mini");

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject cross-provider specialty route"),
        Err(err) => err,
    };

    assert_eq!(err.code, "config");
    assert!(err.message.contains("TRANSLATION_MODEL"));
    assert!(err.message.contains("requires provider `openai`"));
}

#[test]
fn fixed_bigmodel_provider_accepts_bigmodel_only_agent_routes() {
    let mut config = app_config(ProviderMode::BigModel, "bigmodel:glm-5.2");
    config.bigmodel_api_key = Some("test-bigmodel-key".to_owned());
    set_configured_route(
        &mut config,
        "agent.model_routes.private_main",
        "bigmodel:glm-5.2",
    );
    set_configured_route(
        &mut config,
        "agent.model_routes.group_main",
        "bigmodel:glm-5.2",
    );
    set_configured_route(&mut config, "agent.model_routes.aux", "bigmodel:glm-5.2");

    let provider = build_provider(&config).unwrap();

    assert_eq!(provider.name(), "bigmodel");
    assert_eq!(provider.model(), "bigmodel:glm-5.2");
}

#[test]
fn auto_rejects_only_custom_provider_without_configured_key() {
    let mut config = app_config(ProviderMode::Auto, "anthropic:claude");
    config.openai_api_key = None;
    config
        .openai_compatible_providers
        .push(OpenAiCompatibleProviderConfig {
            id: ModelProvider::Custom("anthropic".to_owned()),
            base_url: "https://api.anthropic.example/v1".to_owned(),
            api_key_env: "ANTHROPIC_API_KEY".to_owned(),
            api_key: None,
            auth: HttpAuthConfig::default(),
            request_timeout_seconds: None,
        });

    let err = match build_provider(&config) {
        Ok(_) => panic!("build_provider should reject routes with no available provider"),
        Err(err) => err,
    };

    assert_eq!(err.code, "config");
    assert!(err.message.contains("no LLM provider is available"));
}

#[test]
fn provider_errors_are_fallback_eligible() {
    assert!(should_try_next_model(&LlmError::provider(
        "upstream failed",
        "provider"
    )));
    assert!(should_try_next_model(&LlmError::provider(
        "provider missing key",
        "provider_unavailable"
    )));
    assert!(should_try_next_model(&LlmError::timeout("request")));
    assert!(!should_try_next_model(&LlmError::config("missing key")));
    assert!(!should_try_next_model(&LlmError::new(
        "bad_request",
        "bad local request",
        "request"
    )));
}

#[tokio::test]
async fn model_route_provider_uses_first_successful_candidate() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a,deepseek:deepseek-chat",
        vec![Ok(outcome("primary"))],
        vec![Ok(outcome("fallback"))],
    );

    let result = provider.chat(request()).await.unwrap();

    assert_eq!(result.reply, "primary");
    assert!(!result.fallback_used);
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 0);
    assert_eq!(openai.requests()[0].model.as_deref(), Some("openai:gpt-a"));
}

#[tokio::test]
async fn model_route_provider_skips_unavailable_candidate_provider() {
    let (provider, openai, deepseek) = route_provider(
        "bigmodel:glm-5.2,deepseek:deepseek-chat",
        vec![Ok(outcome("should not be used"))],
        vec![Ok(outcome("fallback"))],
    );

    let result = provider.chat(request()).await.unwrap();

    assert_eq!(result.reply, "fallback");
    assert!(result.fallback_used);
    assert_eq!(openai.calls(), 0);
    assert_eq!(deepseek.calls(), 1);
    assert_eq!(
        deepseek.requests()[0].model.as_deref(),
        Some("deepseek:deepseek-chat")
    );
}

#[tokio::test]
async fn model_route_tool_calling_uses_declared_protocol() {
    let openai = Arc::new(
        MockProvider::new("openai", Vec::new())
            .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
            .with_tool_results(vec![Ok(outcome("tool reply"))]),
    );
    let deepseek = Arc::new(MockProvider::new(
        "deepseek",
        vec![Ok(outcome("should not be used"))],
    ));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let result = provider.chat_with_tools(tool_request()).await.unwrap();

    assert_eq!(result.reply, "tool reply");
    assert_eq!(openai.calls(), 0);
    assert_eq!(openai.tool_calls(), 1);
    assert_eq!(
        openai.tool_requests()[0].chat.model.as_deref(),
        Some("openai:gpt-a")
    );
    assert_eq!(deepseek.calls(), 0);
    assert_eq!(deepseek.tool_calls(), 0);
}

#[tokio::test]
async fn model_route_tool_calling_skips_unavailable_candidate_provider() {
    let openai = Arc::new(MockProvider::new(
        "openai",
        vec![Ok(outcome("should not be used"))],
    ));
    let deepseek = Arc::new(
        MockProvider::new("deepseek", Vec::new())
            .with_tool_protocol(ToolCallingProtocol::ChatCompletionsToolCalls)
            .with_tool_results(vec![Ok(outcome("tool fallback"))]),
    );
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("bigmodel:glm-5.2,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let result = provider.chat_with_tools(tool_request()).await.unwrap();

    assert_eq!(result.reply, "tool fallback");
    assert_eq!(openai.calls(), 0);
    assert_eq!(deepseek.tool_calls(), 1);
    assert_eq!(
        deepseek.tool_requests()[0].chat.model.as_deref(),
        Some("deepseek:deepseek-chat")
    );
}

#[tokio::test]
async fn model_route_tool_calling_falls_back_after_eligible_candidate_error() {
    let openai = Arc::new(
        MockProvider::new("openai", Vec::new())
            .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
            .with_tool_results(vec![Err(LlmError::new(
                "http_error",
                "upstream unavailable",
                "http",
            ))]),
    );
    let deepseek = Arc::new(
        MockProvider::new("deepseek", Vec::new())
            .with_tool_protocol(ToolCallingProtocol::ChatCompletionsToolCalls)
            .with_tool_results(vec![Ok(outcome("tool fallback"))]),
    );
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let result = provider.chat_with_tools(tool_request()).await.unwrap();

    assert_eq!(result.reply, "tool fallback");
    assert!(result.fallback_used);
    assert_eq!(openai.tool_calls(), 1);
    assert_eq!(deepseek.tool_calls(), 1);
    assert_eq!(
        deepseek.tool_requests()[0].chat.model.as_deref(),
        Some("deepseek:deepseek-chat")
    );
}

#[tokio::test]
async fn model_route_tool_calling_falls_back_to_first_candidate_chat_when_unsupported() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a,deepseek:deepseek-chat",
        vec![Ok(outcome("plain reply"))],
        vec![Ok(outcome("fallback should not be used"))],
    );

    let result = provider.chat_with_tools(tool_request()).await.unwrap();

    assert_eq!(result.reply, "plain reply");
    assert_eq!(openai.calls(), 1);
    assert_eq!(openai.tool_calls(), 0);
    assert_eq!(deepseek.calls(), 0);
    assert_eq!(deepseek.tool_calls(), 0);
    assert_eq!(openai.requests()[0].model.as_deref(), Some("openai:gpt-a"));
}

#[tokio::test]
async fn model_route_provider_falls_back_on_eligible_error() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a,deepseek:deepseek-chat",
        vec![Err(LlmError::timeout("provider"))],
        vec![Ok(outcome("fallback"))],
    );

    let result = provider.chat(request()).await.unwrap();

    assert_eq!(result.reply, "fallback");
    assert!(result.fallback_used);
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 1);
    assert_eq!(
        deepseek.requests()[0].model.as_deref(),
        Some("deepseek:deepseek-chat")
    );
}

#[tokio::test]
async fn model_route_provider_falls_back_after_mimo_rate_limit() {
    let mimo_id = ModelProvider::Custom("mimo".to_owned());
    let mimo = Arc::new(MockProvider::new(
        "mimo",
        vec![Err(LlmError::new(
            "rate_limited",
            "too many requests",
            "http",
        ))],
    ));
    let deepseek = Arc::new(MockProvider::new(
        "deepseek",
        vec![Ok(outcome("deepseek fallback"))],
    ));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("mimo:mimo-v2.5-pro,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (mimo_id, mimo.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let result = provider.chat(request()).await.unwrap();

    assert_eq!(result.reply, "deepseek fallback");
    assert!(result.fallback_used);
    assert_eq!(mimo.calls(), 1);
    assert_eq!(deepseek.calls(), 1);
    assert_eq!(
        mimo.requests()[0].model.as_deref(),
        Some("mimo:mimo-v2.5-pro")
    );
}

#[tokio::test]
async fn model_route_stream_falls_back_before_delta_only() {
    let openai = Arc::new(MockProvider::with_streams(
        "openai",
        vec![Ok(stream_events(vec![Ok(LlmStreamEvent::Completed {
            usage: None,
            finish_reason: None,
            fallback_used: false,
        })]))],
    ));
    let deepseek = Arc::new(MockProvider::with_streams(
        "deepseek",
        vec![Ok(stream_events(vec![
            Ok(LlmStreamEvent::TextDelta("fallback".to_owned())),
            Ok(LlmStreamEvent::Completed {
                usage: None,
                finish_reason: None,
                fallback_used: false,
            }),
        ]))],
    ));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let outcome = collect_llm_stream(
        provider.stream_chat(request()).await.unwrap(),
        provider.name(),
        provider.model(),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "fallback");
    assert!(outcome.fallback_used);
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 1);
}

#[tokio::test]
async fn model_route_stream_error_after_delta_does_not_fallback() {
    let openai = Arc::new(MockProvider::with_streams(
        "openai",
        vec![Ok(stream_events(vec![
            Ok(LlmStreamEvent::TextDelta("partial".to_owned())),
            Err(LlmError::provider("broken", "stream")),
        ]))],
    ));
    let deepseek = Arc::new(MockProvider::with_streams(
        "deepseek",
        vec![Ok(stream_events(vec![
            Ok(LlmStreamEvent::TextDelta("fallback".to_owned())),
            Ok(LlmStreamEvent::Completed {
                usage: None,
                finish_reason: None,
                fallback_used: false,
            }),
        ]))],
    ));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, openai.clone()),
            (ModelProvider::DeepSeek, deepseek.clone()),
        ],
    )
    .unwrap();

    let err = collect_llm_stream(
        provider.stream_chat(request()).await.unwrap(),
        provider.name(),
        provider.model(),
    )
    .await
    .unwrap_err();

    assert_eq!(err.stage, "stream_after_delta");
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 0);
}

#[tokio::test]
async fn model_route_provider_keeps_permanent_error() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a,deepseek:deepseek-chat",
        vec![Err(LlmError::config("missing key"))],
        vec![Ok(outcome("fallback"))],
    );

    let err = provider.chat(request()).await.unwrap_err();

    assert_eq!(err.code, "config");
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 0);
}

#[tokio::test]
async fn model_route_provider_aggregates_all_candidate_failures() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a,deepseek:deepseek-chat",
        vec![Err(LlmError::timeout("provider"))],
        vec![Err(LlmError::provider("empty response", "provider"))],
    );

    let err = provider.chat(request()).await.unwrap_err();

    assert_eq!(err.code, "provider_error");
    assert_eq!(err.stage, "provider_route");
    assert!(err.message.contains("#0 openai:gpt-a -> timeout@provider"));
    assert!(
        err.message
            .contains("#1 deepseek:deepseek-chat -> provider_error@provider")
    );
    assert_eq!(openai.calls(), 1);
    assert_eq!(deepseek.calls(), 1);
}

#[tokio::test]
async fn model_route_provider_uses_request_route_override() {
    let (provider, openai, deepseek) = route_provider(
        "openai:gpt-a",
        vec![Ok(outcome("primary"))],
        vec![Ok(outcome("deepseek"))],
    );
    let mut req = request();
    req.model = Some("deepseek:deepseek-chat".to_owned());

    let result = provider.chat(req).await.unwrap();

    assert_eq!(result.reply, "deepseek");
    assert_eq!(openai.calls(), 0);
    assert_eq!(deepseek.calls(), 1);
}

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
    supports_vision: bool,
    tool_results: Arc<Mutex<Vec<Result<ChatOutcome, LlmError>>>>,
    tool_deltas: Arc<Mutex<Vec<String>>>,
    tool_calls: Arc<Mutex<usize>>,
    tool_requests: Arc<Mutex<Vec<ToolChatRequest>>>,
}

#[derive(Clone, Copy)]
enum HandleBehavior {
    Failed,
    Timeout,
    Cancelled,
    Success,
    FailedAfterToolStart,
}

struct HandleAwareProvider {
    name: &'static str,
    behavior: HandleBehavior,
    calls: Arc<Mutex<usize>>,
}

struct PlainDefaultProvider;

#[async_trait]
impl LlmProvider for PlainDefaultProvider {
    async fn chat(&self, _req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        Ok(outcome("plain default reply"))
    }

    fn name(&self) -> &str {
        "plain"
    }

    fn model(&self) -> &str {
        "plain-model"
    }

    fn stream_enabled(&self) -> bool {
        false
    }
}

struct BlockingPlainProvider {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl LlmProvider for BlockingPlainProvider {
    async fn chat(&self, _req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        self.started.notify_one();
        self.release.notified().await;
        Ok(outcome("late plain reply"))
    }

    fn name(&self) -> &str {
        "blocking-plain"
    }

    fn model(&self) -> &str {
        "plain-model"
    }

    fn stream_enabled(&self) -> bool {
        false
    }
}

struct SessionCreationFailureProvider;

#[async_trait]
impl LlmProvider for SessionCreationFailureProvider {
    async fn chat(&self, _req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        unreachable!("session creation failure must not fall back to chat")
    }

    fn tool_calling_protocol(&self, _model: Option<&str>) -> Option<ToolCallingProtocol> {
        Some(ToolCallingProtocol::OpenAiResponses)
    }

    async fn begin_agent_session(
        &self,
        _req: AgentSessionRequest<'_>,
    ) -> Result<Option<Box<dyn AgentStepSession + Send>>, LlmError> {
        Err(LlmError::new(
            "provider_error",
            "failed to create agent session",
            "agent_session",
        ))
    }

    fn name(&self) -> &str {
        "session-failure"
    }

    fn model(&self) -> &str {
        "failure-model"
    }

    fn stream_enabled(&self) -> bool {
        false
    }
}

impl HandleAwareProvider {
    fn new(name: &'static str, behavior: HandleBehavior) -> Self {
        Self {
            name,
            behavior,
            calls: Arc::new(Mutex::new(0)),
        }
    }

    fn calls(&self) -> usize {
        *self.calls.lock().unwrap()
    }
}

#[async_trait]
impl LlmProvider for HandleAwareProvider {
    async fn chat(&self, _req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        unreachable!("handle-aware provider is only used by tool routing tests")
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        *self.calls.lock().unwrap() += 1;
        let handle = req.run_handle.expect("missing shared run handle");
        handle.update(|diagnostics| diagnostics.model_rounds += 1);
        match self.behavior {
            HandleBehavior::Failed => {
                handle.update(|diagnostics| {
                    diagnostics.emitted_tools.push("lookup_tool".to_owned());
                });
                handle.set_stop_reason(AgentStopReason::Failed);
                Err(LlmError::new("provider_error", "failed", "provider")
                    .with_agent(handle.snapshot()))
            }
            HandleBehavior::Timeout => {
                handle.cancel(AgentStopReason::Timeout);
                Err(LlmError::timeout("provider").with_agent(handle.snapshot()))
            }
            HandleBehavior::Cancelled => {
                handle.cancel(AgentStopReason::Cancelled);
                Err(LlmError::new("cancelled", "cancelled", "agent_loop")
                    .with_agent(handle.snapshot()))
            }
            HandleBehavior::Success => {
                handle.set_stop_reason(AgentStopReason::DirectAnswer);
                let mut result = outcome("fallback success");
                result.agent = handle.snapshot();
                Ok(result)
            }
            HandleBehavior::FailedAfterToolStart => {
                handle.update(|diagnostics| {
                    diagnostics.executed_tools.push("write_tool".to_owned());
                    diagnostics
                        .tools_with_unknown_result
                        .push("write_tool".to_owned());
                });
                handle.set_stop_reason(AgentStopReason::Failed);
                Err(LlmError::new("provider_error", "failed", "provider")
                    .with_agent(handle.snapshot()))
            }
        }
    }

    fn tool_calling_protocol(&self, _model: Option<&str>) -> Option<ToolCallingProtocol> {
        Some(ToolCallingProtocol::OpenAiResponses)
    }

    fn name(&self) -> &str {
        self.name
    }

    fn model(&self) -> &str {
        "mock-model"
    }

    fn stream_enabled(&self) -> bool {
        false
    }
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
            supports_vision: false,
            tool_results: Arc::new(Mutex::new(Vec::new())),
            tool_deltas: Arc::new(Mutex::new(Vec::new())),
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
            supports_vision: false,
            tool_results: Arc::new(Mutex::new(Vec::new())),
            tool_deltas: Arc::new(Mutex::new(Vec::new())),
            tool_calls: Arc::new(Mutex::new(0)),
            tool_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_tool_protocol(mut self, protocol: ToolCallingProtocol) -> Self {
        self.tool_protocol = Some(protocol);
        self
    }

    fn with_vision(mut self) -> Self {
        self.supports_vision = true;
        self
    }

    fn with_tool_results(self, results: Vec<Result<ChatOutcome, LlmError>>) -> Self {
        *self.tool_results.lock().unwrap() = results;
        self
    }

    fn with_tool_deltas(self, deltas: Vec<&str>) -> Self {
        *self.tool_deltas.lock().unwrap() = deltas.into_iter().map(str::to_owned).collect();
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

    fn supports_vision(&self, _model: Option<&str>) -> bool {
        self.supports_vision
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        *self.tool_calls.lock().unwrap() += 1;
        self.tool_requests.lock().unwrap().push(req.clone());
        let deltas = std::mem::take(&mut *self.tool_deltas.lock().unwrap());
        for delta in deltas {
            if let Some(sink) = &req.final_delta_sink {
                sink(delta).await?;
            }
        }
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
            actor: qq_maid_common::identity_context::ExecutionActorContext {
                user_id: Some("u1".to_owned()),
                group_member_role: None,
            },
            conversation: qq_maid_common::identity_context::ExecutionConversationContext {
                platform: "test".to_owned(),
                account_id: None,
                kind: qq_maid_common::identity_context::ConversationKind::Private,
                target_id: Some("u1".to_owned()),
                scope_id: "private:u1".to_owned(),
                interaction_scope_id: "private:u1".to_owned(),
            },
            tool_call_id: None,
            execution_deadline: None,
        },
        max_rounds: 3,
        progress_sink: None,
        final_delta_sink: None,
        run_handle: None,
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
        agent: Default::default(),
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
        gemini_api_key: None,
        gemini_base_url: "https://generativelanguage.googleapis.com/v1beta/openai".to_owned(),
        gemini_model: "gemini:gemini-2.5-flash".to_owned(),
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

fn handle_route_provider(
    first_behavior: HandleBehavior,
    second_behavior: HandleBehavior,
) -> (
    ModelRouteProvider,
    Arc<HandleAwareProvider>,
    Arc<HandleAwareProvider>,
) {
    let first = Arc::new(HandleAwareProvider::new("openai", first_behavior));
    let second = Arc::new(HandleAwareProvider::new("deepseek", second_behavior));
    let provider = ModelRouteProvider::new(
        "auto",
        ModelProvider::OpenAi,
        ModelRoute::parse_config("openai:gpt-a,deepseek:deepseek-chat", "LLM_MODEL").unwrap(),
        vec![
            (ModelProvider::OpenAi, first.clone()),
            (ModelProvider::DeepSeek, second.clone()),
        ],
    )
    .unwrap();
    (provider, first, second)
}

mod config;
mod routing;

//! LLM 提供商抽象层。
//!
//! 定义了统一的 [`LlmProvider`] trait，屏蔽不同 LLM API（OpenAI、DeepSeek、BigModel）的差异。
//! 同时提供通用模型候选链路由逻辑，以及 [`ChatOutcome`] 等通用类型。

pub mod bigmodel;
pub mod deepseek;
pub mod limiter;
pub mod openai;
pub mod status;
pub mod types;

use std::{pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};

use crate::{
    config::{LlmConfig, ProviderMode},
    error::LlmError,
    metrics::{LlmMetrics, MetricsRecorder},
    provider::types::{ChatRequest, ModelId, ModelProvider, ModelRoute, TokenUsage},
    tool::ToolRegistry,
};

/// LLM 调用的最终输出结果。
#[derive(Debug, Clone)]
pub struct ChatOutcome {
    /// 模型返回的文本回复。
    pub reply: String,
    /// 本次请求的指标记录（延迟、首 token 时间等）。
    pub metrics: LlmMetrics,
    /// 令牌用量统计（输入/输出/总计），部分提供商可能不返回。
    pub usage: Option<TokenUsage>,
    /// 是否因前序模型候选失败而使用了后续候选。
    pub fallback_used: bool,
}

/// 原生 Tool Calling 请求。
#[derive(Clone)]
pub struct ToolChatRequest {
    /// 基础聊天请求。
    pub chat: ChatRequest,
    /// 服务端白名单工具。
    pub tools: ToolRegistry,
    /// 最多允许执行的工具调用轮数。
    pub max_rounds: usize,
}

/// LLM 标准聊天流事件。
///
/// `Completed` 是每条流唯一的成功终止状态，usage 与 finish reason 都随终止事件返回；
/// collector 必须继续消费到 EOF，不能因为某个 provider 提前给出 finish 标记就停止读流。
#[derive(Debug, Clone)]
pub enum LlmStreamEvent {
    /// 模型正文增量。当前 Core/Gateway 只把它作为进程内保活和未来增量发送扩展依据。
    TextDelta(String),
    /// 成功终止事件。完整正文由 collector 聚合；usage 不单独作为终止信号。
    Completed {
        usage: Option<TokenUsage>,
        finish_reason: Option<String>,
        fallback_used: bool,
    },
}

/// provider 暴露给 Core 的标准聊天流。
pub type LlmStream = Pin<Box<dyn Stream<Item = Result<LlmStreamEvent, LlmError>> + Send>>;

/// LLM 提供商统一接口。
///
/// 所有后端（OpenAI、DeepSeek、BigModel 等）必须实现此 trait。
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 发送聊天请求并返回结果。
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError>;
    /// 发送聊天请求并返回标准流。
    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        self.chat(req).await.map(outcome_to_stream)
    }
    /// 使用模型原生 Tool Calling 执行聊天。默认不支持，具体 provider 按能力覆盖。
    async fn chat_with_tools(&self, _req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        Err(LlmError::new(
            "unsupported_tool_calling",
            "provider does not support native tool calling",
            "tool_loop",
        ))
    }
    /// 提供商名称，例如 "openai"、"deepseek"、"bigmodel"。
    fn name(&self) -> &'static str;
    /// 当前使用的模型名称。
    fn model(&self) -> &str;
    /// 是否启用了流式传输。
    fn stream_enabled(&self) -> bool;
}

/// 线程安全的 LLM 提供商智能指针别名。
pub type DynLlmProvider = Arc<dyn LlmProvider>;

/// 收集标准 LLM 流为完整结果，供内部结构化任务继续使用完整 `chat()` 语义。
pub async fn collect_llm_stream(
    mut stream: LlmStream,
    provider: &str,
    model: &str,
) -> Result<ChatOutcome, LlmError> {
    let mut recorder = MetricsRecorder::start();
    let mut reply = String::new();
    let mut usage = None;
    let mut completed = false;
    let mut fallback_used = false;
    while let Some(event) = stream.next().await {
        match event? {
            LlmStreamEvent::TextDelta(delta) => {
                recorder.mark_event();
                if !delta.is_empty() {
                    recorder.mark_token();
                }
                reply.push_str(&delta);
            }
            LlmStreamEvent::Completed {
                usage: event_usage,
                fallback_used: event_fallback_used,
                ..
            } => {
                if completed {
                    return Err(LlmError::provider(
                        "LLM stream produced multiple completion events",
                        "stream",
                    ));
                }
                completed = true;
                usage = event_usage;
                fallback_used |= event_fallback_used;
            }
        }
    }
    if !completed {
        return Err(LlmError::provider(
            "LLM stream ended without completion event",
            "stream",
        ));
    }
    if reply.trim().is_empty() {
        return Err(LlmError::provider(
            "LLM stream returned empty text output",
            "provider",
        ));
    }
    Ok(ChatOutcome {
        reply,
        metrics: recorder.finish(provider, model, true),
        usage,
        fallback_used,
    })
}

pub(crate) fn outcome_to_stream(outcome: ChatOutcome) -> LlmStream {
    let usage = outcome.usage.clone();
    let reply = outcome.reply;
    Box::pin(stream::iter(vec![
        Ok(LlmStreamEvent::TextDelta(reply)),
        Ok(LlmStreamEvent::Completed {
            usage,
            finish_reason: None,
            fallback_used: outcome.fallback_used,
        }),
    ]))
}

/// 根据配置构建 LLM 提供商实例。
///
/// - `OpenAi`：仅使用 OpenAI 提供商。
/// - `DeepSeek`：仅使用 DeepSeek 提供商。
/// - `BigModel`：仅使用智谱 BigModel 提供商。
/// - `Auto`：根据模型候选链路由；单 OpenAI 主模型仍兼容原 OpenAI -> DeepSeek fallback。
pub fn build_provider(config: &LlmConfig) -> Result<DynLlmProvider, LlmError> {
    match config.provider {
        ProviderMode::OpenAi => {
            for (name, route) in &config.configured_model_routes {
                ensure_route_supported(route, ModelProvider::OpenAi, ModelProvider::OpenAi, name)?;
            }
            let provider: DynLlmProvider = Arc::new(openai::OpenAiProvider::new(config)?);
            Ok(Arc::new(ModelRouteProvider::new(
                "openai",
                ModelProvider::OpenAi,
                config.model_route.clone(),
                vec![(ModelProvider::OpenAi, provider)],
            )?))
        }
        ProviderMode::DeepSeek => {
            for (name, route) in &config.configured_model_routes {
                ensure_route_supported(
                    route,
                    ModelProvider::DeepSeek,
                    ModelProvider::DeepSeek,
                    name,
                )?;
            }
            let provider: DynLlmProvider = Arc::new(deepseek::DeepSeekProvider::new(config)?);
            Ok(Arc::new(ModelRouteProvider::new(
                "deepseek",
                ModelProvider::DeepSeek,
                config.model_route.clone(),
                vec![(ModelProvider::DeepSeek, provider)],
            )?))
        }
        ProviderMode::BigModel => {
            for (name, route) in &config.configured_model_routes {
                ensure_route_supported(
                    route,
                    ModelProvider::BigModel,
                    ModelProvider::BigModel,
                    name,
                )?;
            }
            let provider: DynLlmProvider = Arc::new(bigmodel::BigModelProvider::new(config)?);
            Ok(Arc::new(ModelRouteProvider::new(
                "bigmodel",
                ModelProvider::BigModel,
                config.model_route.clone(),
                vec![(ModelProvider::BigModel, provider)],
            )?))
        }
        ProviderMode::Auto => {
            let route = auto_default_route(config)?;
            let provider_routes = auto_provider_routes(config, &route)?;
            let required_providers =
                provider_kinds_for_routes(&provider_routes, ModelProvider::OpenAi);
            let mut providers: Vec<(ModelProvider, DynLlmProvider)> = Vec::new();

            ensure_required_api_keys_for_routes(config, &provider_routes)?;

            for provider_kind in required_providers {
                match provider_kind {
                    ModelProvider::OpenAi => providers.push((
                        ModelProvider::OpenAi,
                        Arc::new(openai::OpenAiProvider::new(config)?),
                    )),
                    ModelProvider::DeepSeek => providers.push((
                        ModelProvider::DeepSeek,
                        Arc::new(deepseek::DeepSeekProvider::new(config)?),
                    )),
                    ModelProvider::BigModel => providers.push((
                        ModelProvider::BigModel,
                        Arc::new(bigmodel::BigModelProvider::new(config)?),
                    )),
                }
            }

            Ok(Arc::new(ModelRouteProvider::new(
                "auto",
                ModelProvider::OpenAi,
                route,
                providers,
            )?))
        }
    }
}

/// 通用模型候选链提供商。
///
/// 先执行 OpenAI/DeepSeek/BigModel 各自内部的 Responses、Chat Completions、空流补非流等
/// 兼容策略；只有某个候选整体失败且错误允许跨模型降级时，才尝试下一个候选。
struct ModelRouteProvider {
    name: &'static str,
    default_provider: ModelProvider,
    default_route: ModelRoute,
    providers: Vec<(ModelProvider, DynLlmProvider)>,
    model_display: String,
}

impl ModelRouteProvider {
    fn new(
        name: &'static str,
        default_provider: ModelProvider,
        default_route: ModelRoute,
        providers: Vec<(ModelProvider, DynLlmProvider)>,
    ) -> Result<Self, LlmError> {
        if providers.is_empty() {
            return Err(LlmError::config(
                "no LLM provider is available for model route",
            ));
        }
        let model_display = default_route.display();
        Ok(Self {
            name,
            default_provider,
            default_route,
            providers,
            model_display,
        })
    }

    fn provider_for(&self, provider: ModelProvider) -> Option<&DynLlmProvider> {
        self.providers
            .iter()
            .find(|(candidate, _)| *candidate == provider)
            .map(|(_, provider)| provider)
    }
}

#[async_trait]
impl LlmProvider for ModelRouteProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let route = match req.model.as_deref() {
            Some(value) => ModelRoute::parse(value, "request")?,
            None => self.default_route.clone(),
        };
        let task = model_task_name(&req);
        let mut failures = Vec::new();

        for (index, candidate) in route.candidates().iter().enumerate() {
            let provider_kind = candidate.provider.unwrap_or(self.default_provider);
            let provider = self.provider_for(provider_kind).ok_or_else(|| {
                LlmError::config(format!(
                    "provider `{}` is not available for model candidate `{}`",
                    provider_kind.as_str(),
                    candidate.to_request_model()
                ))
            })?;
            let mut candidate_req = req.clone();
            candidate_req.model = Some(candidate.to_request_model());

            match provider.chat(candidate_req).await {
                Ok(mut outcome) => {
                    tracing::info!(
                        task,
                        candidate_index = index,
                        provider = provider_kind.as_str(),
                        model = %candidate.name,
                        result = "success",
                        "model candidate succeeded"
                    );
                    // provider 内部兼容 fallback 与跨模型候选降级语义不同；这里只在
                    // 真正使用后续模型候选时标记，保持原有候选链行为不变。
                    outcome.fallback_used |= index > 0;
                    return Ok(outcome);
                }
                Err(err) => {
                    let fallback = index + 1 < route.len() && should_try_next_model(&err);
                    tracing::warn!(
                        task,
                        candidate_index = index,
                        provider = provider_kind.as_str(),
                        model = %candidate.name,
                        result = "failed",
                        error_code = err.code.as_str(),
                        error_stage = err.stage.as_str(),
                        error_kind = model_error_kind(&err),
                        fallback,
                        "model candidate failed"
                    );
                    if !fallback {
                        if route.len() == 1 || !should_try_next_model(&err) {
                            return Err(err);
                        }
                        failures.push(ModelAttemptFailure::new(
                            index,
                            provider_kind,
                            candidate,
                            err,
                        ));
                        return Err(aggregate_route_error(task, failures));
                    }
                    failures.push(ModelAttemptFailure::new(
                        index,
                        provider_kind,
                        candidate,
                        err,
                    ));
                }
            }
        }

        Err(aggregate_route_error(task, failures))
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        let candidates = match req.chat.model.as_deref() {
            Some(value) => ModelRoute::parse(value, "request")?.candidates().to_vec(),
            None => self.default_route.candidates().to_vec(),
        };
        let Some(candidate) = candidates.first() else {
            return Err(LlmError::new(
                "bad_request",
                "model candidate list must not be empty",
                "request",
            ));
        };
        let provider_kind = candidate.provider.unwrap_or(self.default_provider);
        let Some(provider) = self.provider_for(provider_kind).cloned() else {
            return Err(LlmError::config(format!(
                "no provider configured for {}",
                provider_kind.as_str()
            )));
        };
        let mut chat = req.chat;
        chat.model = Some(candidate.to_request_model());
        // Tool Loop 期间固定首个候选和 provider，不进入模型候选 fallback。
        provider
            .chat_with_tools(ToolChatRequest {
                chat,
                tools: req.tools,
                max_rounds: req.max_rounds,
            })
            .await
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        let route = match req.model.as_deref() {
            Some(value) => ModelRoute::parse(value, "request")?,
            None => self.default_route.clone(),
        };
        let task = model_task_name(&req).to_owned();
        let candidates = route.candidates().to_vec();
        let providers = self.providers.clone();
        let default_provider = self.default_provider;

        Ok(Box::pin(stream::unfold(
            RouteStreamState {
                req,
                task,
                candidates,
                providers,
                default_provider,
                candidate_index: 0,
                current_stream: None,
                current_attempt: None,
                failures: Vec::new(),
                emitted_non_empty_delta: false,
                done: false,
            },
            |mut state| async move {
                let event = next_route_stream_event(&mut state).await;
                event.map(|event| (event, state))
            },
        )))
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn model(&self) -> &str {
        &self.model_display
    }

    fn stream_enabled(&self) -> bool {
        self.providers
            .first()
            .map(|(_, provider)| provider.stream_enabled())
            .unwrap_or(false)
    }
}

struct RouteStreamState {
    req: ChatRequest,
    task: String,
    candidates: Vec<ModelId>,
    providers: Vec<(ModelProvider, DynLlmProvider)>,
    default_provider: ModelProvider,
    candidate_index: usize,
    current_stream: Option<LlmStream>,
    current_attempt: Option<(usize, ModelProvider, ModelId)>,
    failures: Vec<ModelAttemptFailure>,
    emitted_non_empty_delta: bool,
    done: bool,
}

async fn next_route_stream_event(
    state: &mut RouteStreamState,
) -> Option<Result<LlmStreamEvent, LlmError>> {
    loop {
        if state.done {
            return None;
        }
        if state.current_stream.is_none() {
            match start_next_route_candidate(state).await {
                Ok(true) => {}
                Ok(false) => {
                    state.done = true;
                    return Some(Err(aggregate_route_error(
                        &state.task,
                        std::mem::take(&mut state.failures),
                    )));
                }
                Err(err) => {
                    state.done = true;
                    return Some(Err(err));
                }
            }
        }

        let Some(stream) = state.current_stream.as_mut() else {
            continue;
        };
        match stream.next().await {
            Some(Ok(LlmStreamEvent::TextDelta(delta))) => {
                if !delta.is_empty() {
                    state.emitted_non_empty_delta = true;
                }
                return Some(Ok(LlmStreamEvent::TextDelta(delta)));
            }
            Some(Ok(LlmStreamEvent::Completed {
                usage,
                finish_reason,
                fallback_used,
            })) => {
                if !state.emitted_non_empty_delta {
                    let err =
                        LlmError::provider("LLM stream returned empty text output", "provider");
                    record_current_route_failure(state, err);
                    state.current_stream = None;
                    state.current_attempt = None;
                    continue;
                }
                let fallback_used = fallback_used
                    || state
                        .current_attempt
                        .as_ref()
                        .is_some_and(|(index, _, _)| *index > 0);
                state.done = true;
                return Some(Ok(LlmStreamEvent::Completed {
                    usage,
                    finish_reason,
                    fallback_used,
                }));
            }
            Some(Err(err)) => {
                if state.emitted_non_empty_delta {
                    state.done = true;
                    return Some(Err(LlmError::new(
                        err.code,
                        err.message,
                        "stream_after_delta",
                    )));
                }
                record_current_route_failure(state, err);
                state.current_stream = None;
                state.current_attempt = None;
            }
            None => {
                if state.emitted_non_empty_delta {
                    state.done = true;
                    return Some(Err(LlmError::provider(
                        "LLM stream ended before completion after emitting text",
                        "stream_after_delta",
                    )));
                }
                let err = LlmError::provider("LLM stream ended without completion event", "stream");
                record_current_route_failure(state, err);
                state.current_stream = None;
                state.current_attempt = None;
            }
        }
    }
}

async fn start_next_route_candidate(state: &mut RouteStreamState) -> Result<bool, LlmError> {
    while state.candidate_index < state.candidates.len() {
        let index = state.candidate_index;
        state.candidate_index += 1;
        let candidate = state.candidates[index].clone();
        let provider_kind = candidate.provider.unwrap_or(state.default_provider);
        let provider = state
            .providers
            .iter()
            .find(|(kind, _)| *kind == provider_kind)
            .map(|(_, provider)| provider.clone())
            .ok_or_else(|| {
                LlmError::config(format!(
                    "provider `{}` is not available for model candidate `{}`",
                    provider_kind.as_str(),
                    candidate.to_request_model()
                ))
            })?;
        let mut candidate_req = state.req.clone();
        candidate_req.model = Some(candidate.to_request_model());
        match provider.stream_chat(candidate_req).await {
            Ok(stream) => {
                tracing::info!(
                    task = state.task.as_str(),
                    candidate_index = index,
                    provider = provider_kind.as_str(),
                    model = %candidate.name,
                    result = "stream_started",
                    "model candidate stream started"
                );
                state.current_stream = Some(stream);
                state.current_attempt = Some((index, provider_kind, candidate));
                return Ok(true);
            }
            Err(err) => {
                let fallback =
                    state.candidate_index < state.candidates.len() && should_try_next_model(&err);
                tracing::warn!(
                    task = state.task.as_str(),
                    candidate_index = index,
                    provider = provider_kind.as_str(),
                    model = %candidate.name,
                    result = "failed",
                    error_code = err.code.as_str(),
                    error_stage = err.stage.as_str(),
                    error_kind = model_error_kind(&err),
                    fallback,
                    "model candidate stream init failed"
                );
                if !fallback {
                    return Err(err);
                }
                state.failures.push(ModelAttemptFailure::new(
                    index,
                    provider_kind,
                    &candidate,
                    err,
                ));
            }
        }
    }
    Ok(false)
}

fn record_current_route_failure(state: &mut RouteStreamState, err: LlmError) {
    let Some((index, provider_kind, candidate)) = state.current_attempt.take() else {
        state.failures.push(ModelAttemptFailure {
            index: state.candidate_index,
            provider: state.default_provider,
            model: "<unknown>".to_owned(),
            error: err,
        });
        return;
    };
    let fallback = state.candidate_index < state.candidates.len() && should_try_next_model(&err);
    tracing::warn!(
        task = state.task.as_str(),
        candidate_index = index,
        provider = provider_kind.as_str(),
        model = %candidate.name,
        result = "failed",
        error_code = err.code.as_str(),
        error_stage = err.stage.as_str(),
        error_kind = model_error_kind(&err),
        fallback,
        "model candidate stream failed before text delta"
    );
    state.failures.push(ModelAttemptFailure::new(
        index,
        provider_kind,
        &candidate,
        err,
    ));
    if !fallback {
        state.candidate_index = state.candidates.len();
    }
}

#[derive(Debug)]
struct ModelAttemptFailure {
    index: usize,
    provider: ModelProvider,
    model: String,
    error: LlmError,
}

impl ModelAttemptFailure {
    fn new(index: usize, provider: ModelProvider, candidate: &ModelId, error: LlmError) -> Self {
        Self {
            index,
            provider,
            model: candidate.name.clone(),
            error,
        }
    }
}

fn auto_default_route(config: &LlmConfig) -> Result<ModelRoute, LlmError> {
    let mut candidates = config.model_route.candidates().to_vec();
    // 兼容旧的 `LLM_PROVIDER=auto` 行为：单个 OpenAI/裸主模型在可恢复失败时，
    // 仍可降级到 `DEEPSEEK_MODEL`。用户显式写多个候选时则严格按配置顺序执行。
    if candidates.len() == 1
        && config.deepseek_api_key.is_some()
        && candidates[0].provider != Some(ModelProvider::DeepSeek)
    {
        let deepseek_model = deepseek::deepseek_config_model(&config.deepseek_model)?;
        candidates.push(ModelId {
            provider: Some(ModelProvider::DeepSeek),
            name: deepseek_model,
        });
    }
    ModelRoute::from_candidates(candidates)
}

fn auto_provider_routes(
    config: &LlmConfig,
    default_route: &ModelRoute,
) -> Result<Vec<(String, ModelRoute)>, LlmError> {
    let mut routes = config.configured_model_routes.clone();
    if let Some((_, route)) = routes.iter_mut().find(|(name, _)| *name == "LLM_MODEL") {
        // provider 初始化必须使用 auto 模式的实际默认链，才能保留单 OpenAI
        // 主模型自动追加 DeepSeek fallback 的兼容行为。
        *route = default_route.clone();
    }
    Ok(routes)
}

fn provider_kinds_for_routes(
    routes: &[(String, ModelRoute)],
    default_provider: ModelProvider,
) -> Vec<ModelProvider> {
    [
        ModelProvider::OpenAi,
        ModelProvider::DeepSeek,
        ModelProvider::BigModel,
    ]
    .into_iter()
    .filter(|provider| {
        routes
            .iter()
            .any(|(_, route)| route_uses_provider(route, *provider, default_provider))
    })
    .collect()
}

fn ensure_deepseek_api_key_for_routes(
    config: &LlmConfig,
    routes: &[(String, ModelRoute)],
) -> Result<(), LlmError> {
    let uses_deepseek = routes
        .iter()
        .filter_map(|(name, route)| {
            route_uses_provider(route, ModelProvider::DeepSeek, ModelProvider::OpenAi)
                .then_some(name.as_str())
        })
        .collect::<Vec<_>>()
        .join(", ");
    if uses_deepseek.is_empty() {
        return Ok(());
    }

    let api_key = config.deepseek_api_key.as_ref().ok_or_else(|| {
        LlmError::config(format!(
            "DEEPSEEK_API_KEY is required because configured model routes include DeepSeek: {uses_deepseek}"
        ))
    })?;
    if api_key.trim().is_empty() {
        return Err(LlmError::config(format!(
            "DEEPSEEK_API_KEY is required because configured model routes include DeepSeek: {uses_deepseek}"
        )));
    }
    Ok(())
}

fn ensure_bigmodel_api_key_for_routes(
    config: &LlmConfig,
    routes: &[(String, ModelRoute)],
) -> Result<(), LlmError> {
    let uses_bigmodel = routes
        .iter()
        .filter_map(|(name, route)| {
            route_uses_provider(route, ModelProvider::BigModel, ModelProvider::OpenAi)
                .then_some(name.as_str())
        })
        .collect::<Vec<_>>()
        .join(", ");
    if uses_bigmodel.is_empty() {
        return Ok(());
    }

    let api_key = config.bigmodel_api_key.as_ref().ok_or_else(|| {
        LlmError::config(format!(
            "BIGMODEL_API_KEY is required because configured model routes include BigModel: {uses_bigmodel}"
        ))
    })?;
    if api_key.trim().is_empty() {
        return Err(LlmError::config(format!(
            "BIGMODEL_API_KEY is required because configured model routes include BigModel: {uses_bigmodel}"
        )));
    }
    Ok(())
}

fn ensure_required_api_keys_for_routes(
    config: &LlmConfig,
    routes: &[(String, ModelRoute)],
) -> Result<(), LlmError> {
    ensure_deepseek_api_key_for_routes(config, routes)?;
    ensure_bigmodel_api_key_for_routes(config, routes)
}

fn ensure_route_supported(
    route: &ModelRoute,
    supported: ModelProvider,
    default_provider: ModelProvider,
    name: &str,
) -> Result<(), LlmError> {
    for candidate in route.candidates() {
        let provider = candidate.provider.unwrap_or(default_provider);
        if provider != supported {
            return Err(LlmError::config(format!(
                "{name} candidate `{}` requires provider `{}`, but LLM_PROVIDER is `{}`",
                candidate.to_request_model(),
                provider.as_str(),
                supported.as_str()
            )));
        }
    }
    Ok(())
}

fn route_uses_provider(
    route: &ModelRoute,
    provider: ModelProvider,
    default_provider: ModelProvider,
) -> bool {
    route
        .candidates()
        .iter()
        .any(|candidate| candidate.provider.unwrap_or(default_provider) == provider)
}

/// 判断当前错误是否允许跨候选模型降级。
///
/// 这里只接收上游传输、限流、超时、空响应和 provider 协议类失败；配置错误、
/// 本地请求构造错误和业务参数错误会直接返回，避免把本地问题放大成多次计费请求。
fn should_try_next_model(err: &LlmError) -> bool {
    matches!(
        err.code.as_str(),
        "timeout" | "provider_error" | "http_error" | "rate_limited" | "upstream_unavailable"
    )
}

fn model_error_kind(err: &LlmError) -> &'static str {
    match err.code.as_str() {
        "timeout" => "timeout",
        "http_error" => "http_error",
        "provider_error" if matches!(err.stage.as_str(), "stream" | "sse") => "stream_error",
        "provider_error" if err.stage == "json" => "invalid_response",
        "provider_error" => "provider_error",
        "rate_limited" => "rate_limited",
        "upstream_unavailable" => "upstream_unavailable",
        "bad_request" => "permanent",
        "config" => "config",
        _ => "permanent",
    }
}

fn model_task_name(req: &ChatRequest) -> &str {
    req.metadata
        .get("purpose")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("chat")
}

fn aggregate_route_error(task: &str, failures: Vec<ModelAttemptFailure>) -> LlmError {
    let details = failures
        .into_iter()
        .map(|failure| {
            format!(
                "#{} {}:{} -> {}@{}",
                failure.index,
                failure.provider.as_str(),
                failure.model,
                failure.error.code,
                failure.error.stage
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    LlmError::provider(
        format!("all model candidates failed for task `{task}`: {details}"),
        "provider_route",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{LlmConfig, OpenAiApiMode, ProviderMode},
        metrics::LlmMetrics,
        provider::types::{ChatMessage, ChatRequest},
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
            }
        }

        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
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

        fn name(&self) -> &'static str {
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
            metadata: HashMap::new(),
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
            stream: true,
            request_timeout_seconds: 90,
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

    fn auto_required_provider_kinds(config: &LlmConfig) -> Result<Vec<ModelProvider>, LlmError> {
        let route = auto_default_route(config)?;
        let provider_routes = auto_provider_routes(config, &route)?;
        ensure_required_api_keys_for_routes(config, &provider_routes)?;
        Ok(provider_kinds_for_routes(
            &provider_routes,
            ModelProvider::OpenAi,
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
    fn auto_provider_set_rejects_specialty_deepseek_without_api_key() {
        let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini,openai:gpt-5.4");
        set_configured_route(&mut config, "TRANSLATION_MODEL", "deepseek:deepseek-chat");

        let err = match build_provider(&config) {
            Ok(_) => panic!("build_provider should reject missing DeepSeek API key"),
            Err(err) => err,
        };

        assert_eq!(err.code, "config");
        assert!(err.message.contains("DEEPSEEK_API_KEY"));
        assert!(err.message.contains("TRANSLATION_MODEL"));
    }

    #[test]
    fn auto_provider_set_rejects_bigmodel_without_api_key() {
        let mut config = app_config(ProviderMode::Auto, "openai:gpt-5.4-mini");
        set_configured_route(&mut config, "TRANSLATION_MODEL", "bigmodel:glm-5.2");

        let err = match build_provider(&config) {
            Ok(_) => panic!("build_provider should reject missing BigModel API key"),
            Err(err) => err,
        };

        assert_eq!(err.code, "config");
        assert!(err.message.contains("BIGMODEL_API_KEY"));
        assert!(err.message.contains("TRANSLATION_MODEL"));
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
    fn configured_specialty_route_rejects_unsupported_provider_at_startup() {
        let err = ModelRoute::parse_config("anthropic:claude", "TRANSLATION_MODEL").unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("TRANSLATION_MODEL"));
        assert!(err.message.contains("unsupported model provider prefix"));
    }

    #[test]
    fn provider_errors_are_fallback_eligible() {
        assert!(should_try_next_model(&LlmError::provider(
            "upstream failed",
            "provider"
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
}

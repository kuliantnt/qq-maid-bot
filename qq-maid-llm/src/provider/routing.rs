//! 通用模型候选链 provider。
//!
//! [`ModelRouteProvider`] 包装一组按 provider 索引的具体 provider 实例，按 `ModelRoute`
//! 候选顺序执行；单个候选整体失败且错误允许跨模型降级时，才尝试下一个候选。
//!
//! 执行时先跑各 provider 内部的兼容策略（OpenAI Responses -> Chat Completions、
//! DeepSeek / BigModel 各自的空流补非流等），只有候选整体失败才落到本模块的候选链降级。

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use futures::stream;

use crate::{
    agent_loop::{AgentTextDeltaFuture, AgentTextDeltaSink},
    error::LlmError,
    provider::{
        DynLlmProvider,
        types::{ChatRequest, ModelProvider, ModelRoute},
    },
};

use super::route_error::{
    ModelAttemptFailure, aggregate_route_error, model_error_kind, model_task_name,
    should_try_next_model, unavailable_provider_error,
};
use super::stream_state::{RouteStreamState, next_route_stream_event};
use super::{ChatOutcome, LlmProvider, LlmStream, ToolCallingProtocol, ToolChatRequest};

/// 通用模型候选链提供商。
///
/// 先执行 OpenAI/DeepSeek/BigModel 各自内部的 Responses、Chat Completions、空流补非流等
/// 兼容策略；只有某个候选整体失败且错误允许跨模型降级时，才尝试下一个候选。
pub(crate) struct ModelRouteProvider {
    name: &'static str,
    default_provider: ModelProvider,
    default_route: ModelRoute,
    providers: Vec<(ModelProvider, DynLlmProvider)>,
    model_display: String,
}

impl ModelRouteProvider {
    pub(crate) fn new(
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

    /// 按候选 provider 查找已加载的 provider 实例。
    fn provider_for(&self, provider: &ModelProvider) -> Option<&DynLlmProvider> {
        self.providers
            .iter()
            .find(|(candidate, _)| candidate == provider)
            .map(|(_, provider)| provider)
    }
}

fn track_visible_final_delta_sink(
    sink: Option<AgentTextDeltaSink>,
    visible_delta_sent: Arc<AtomicBool>,
) -> Option<AgentTextDeltaSink> {
    sink.map(|sink| {
        Arc::new(move |delta: String| {
            let sink = sink.clone();
            let visible_delta_sent = visible_delta_sent.clone();
            Box::pin(async move {
                if !delta.is_empty() {
                    // 候选链共用用户可见流；一旦外发最终回答 delta，就不能再切到后续候选。
                    visible_delta_sent.store(true, Ordering::SeqCst);
                }
                sink(delta).await
            }) as AgentTextDeltaFuture
        }) as AgentTextDeltaSink
    })
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
            let provider_kind = candidate
                .provider
                .as_ref()
                .unwrap_or(&self.default_provider);
            let Some(provider) = self.provider_for(provider_kind) else {
                let err = unavailable_provider_error(provider_kind, candidate);
                let fallback = index + 1 < route.len() && should_try_next_model(&err);
                tracing::warn!(
                    task,
                    candidate_index = index,
                    provider = provider_kind.as_str(),
                    model = %candidate.name,
                    result = "skipped",
                    error_code = err.code.as_str(),
                    error_stage = err.stage.as_str(),
                    error_kind = model_error_kind(&err),
                    fallback,
                    "model candidate provider is not available"
                );
                if !fallback {
                    if route.len() == 1 {
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
                continue;
            };
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
        if candidates.is_empty() {
            return Err(LlmError::new(
                "bad_request",
                "model candidate list must not be empty",
                "request",
            ));
        }
        let task = model_task_name(&req.chat).to_owned();
        let mut failures = Vec::new();
        let visible_final_delta_sent = Arc::new(AtomicBool::new(false));
        for (index, candidate) in candidates.iter().enumerate() {
            if let Some(handle) = &req.run_handle {
                if handle.is_cancelled() {
                    return Err(LlmError::new(
                        "cancelled",
                        "agent run cancelled before model candidate",
                        "agent_loop",
                    )
                    .with_agent(handle.snapshot()));
                }
                // 每个候选只清理 attempt 终止态；请求级计数和工具轨迹继续累计。
                handle.begin_attempt();
            }
            let provider_kind = candidate
                .provider
                .as_ref()
                .unwrap_or(&self.default_provider);
            let Some(provider) = self.provider_for(provider_kind).cloned() else {
                let err = unavailable_provider_error(provider_kind, candidate);
                let fallback = index + 1 < candidates.len() && should_try_next_model(&err);
                tracing::warn!(
                    task,
                    candidate_index = index,
                    provider = provider_kind.as_str(),
                    model = %candidate.name,
                    result = "skipped",
                    error_code = err.code.as_str(),
                    error_stage = err.stage.as_str(),
                    error_kind = model_error_kind(&err),
                    fallback,
                    "tool model candidate provider is not available"
                );
                if !fallback {
                    failures.push(ModelAttemptFailure::new(
                        index,
                        provider_kind,
                        candidate,
                        err,
                    ));
                    let mut err = aggregate_route_error(&task, failures);
                    if let Some(handle) = &req.run_handle {
                        err.agent = Some(Box::new(handle.snapshot()));
                    }
                    return Err(err);
                }
                failures.push(ModelAttemptFailure::new(
                    index,
                    provider_kind,
                    candidate,
                    err,
                ));
                continue;
            };

            let model = candidate.to_request_model();
            let mut chat = req.chat.clone();
            chat.model = Some(model.clone());
            let result = if provider.tool_calling_protocol(Some(&model)).is_some() {
                tracing::debug!(
                    task,
                    provider = provider_kind.as_str(),
                    model = %candidate.name,
                    "tool model candidate selected"
                );
                provider
                    .chat_with_tools(ToolChatRequest {
                        chat,
                        tools: req.tools.clone(),
                        tool_context: req.tool_context.clone(),
                        max_rounds: req.max_rounds,
                        progress_sink: req.progress_sink.clone(),
                        final_delta_sink: track_visible_final_delta_sink(
                            req.final_delta_sink.clone(),
                            visible_final_delta_sent.clone(),
                        ),
                        run_handle: req.run_handle.clone(),
                    })
                    .await
            } else {
                // 未适配 Tool Calling 的 provider 安全回退到同候选普通聊天；若该候选
                // 仍发生可恢复上游失败，再继续尝试后续候选。
                provider.chat(chat).await
            };

            match result {
                Ok(mut outcome) => {
                    outcome.fallback_used |= index > 0;
                    return Ok(outcome);
                }
                Err(err) => {
                    let visible_delta_sent = visible_final_delta_sent.load(Ordering::SeqCst);
                    let tool_side_effect_started = req.run_handle.as_ref().is_some_and(|handle| {
                        let diagnostics = handle.snapshot();
                        !diagnostics.executed_tools.is_empty()
                            || !diagnostics.tools_with_unknown_result.is_empty()
                    });
                    let fallback = index + 1 < candidates.len()
                        && should_try_next_model(&err)
                        && !visible_delta_sent
                        && !tool_side_effect_started;
                    tracing::warn!(
                        task,
                        candidate_index = index,
                        provider = provider_kind.as_str(),
                        model = %candidate.name,
                        result = "failed",
                        error_code = err.code.as_str(),
                        error_stage = err.stage.as_str(),
                        error_kind = model_error_kind(&err),
                        visible_delta_sent,
                        tool_side_effect_started,
                        fallback,
                        "tool model candidate failed"
                    );
                    if visible_delta_sent {
                        return Err(err);
                    }
                    failures.push(ModelAttemptFailure::new(
                        index,
                        provider_kind,
                        candidate,
                        err,
                    ));
                    if !fallback {
                        let mut err = aggregate_route_error(&task, failures);
                        if let Some(handle) = &req.run_handle {
                            err.agent = Some(Box::new(handle.snapshot()));
                        }
                        return Err(err);
                    }
                }
            }
        }

        Err(aggregate_route_error(&task, failures))
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        let candidates = match model {
            Some(value) => ModelRoute::parse(value, "request")
                .ok()?
                .candidates()
                .to_vec(),
            None => self.default_route.candidates().to_vec(),
        };
        for candidate in candidates {
            let provider_kind = candidate
                .provider
                .as_ref()
                .unwrap_or(&self.default_provider);
            let Some(provider) = self.provider_for(provider_kind) else {
                continue;
            };
            let request_model = candidate.to_request_model();
            return provider.tool_calling_protocol(Some(&request_model));
        }
        None
    }

    fn supports_vision(&self, model: Option<&str>) -> bool {
        let candidates = match model {
            Some(value) => match ModelRoute::parse(value, "request") {
                Ok(route) => route.candidates().to_vec(),
                Err(_) => return false,
            },
            None => self.default_route.candidates().to_vec(),
        };
        for candidate in candidates {
            let provider_kind = candidate
                .provider
                .as_ref()
                .unwrap_or(&self.default_provider);
            let Some(provider) = self.provider_for(provider_kind) else {
                continue;
            };
            let request_model = candidate.to_request_model();
            return provider.supports_vision(Some(&request_model));
        }
        false
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        let route = match req.model.as_deref() {
            Some(value) => ModelRoute::parse(value, "request")?,
            None => self.default_route.clone(),
        };
        let task = model_task_name(&req).to_owned();
        let candidates = route.candidates().to_vec();
        let providers = self.providers.clone();
        let default_provider = self.default_provider.clone();

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

    fn name(&self) -> &str {
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

//! OpenAI 提供商实现。
//!
//! 主链路直接调用 Responses API；在 `OPENAI_API_MODE=auto` 且 Responses
//! 出现可恢复上游错误时，降级到同 endpoint 的 Chat Completions。

mod chat;
mod chat_tool_loop;
mod configured;
mod extract;
mod fallback;
mod payload;
mod responses;
mod stream;
mod tool_loop;
mod transport;

use std::time::Duration;

use async_trait::async_trait;
use futures::{StreamExt, stream as futures_stream};

use crate::{
    agent_loop::{AgentSessionRequest, AgentStepSession},
    config::{HttpAuthConfig, LlmConfig, OpenAiApiMode},
    error::LlmError,
    provider::{
        ChatOutcome, LlmProvider, LlmStream, LlmStreamEvent, ToolCallingProtocol,
        outcome_to_stream,
        types::{ChatMessage, ChatRequest, ModelProvider, ModelRoute},
    },
};

const IMAGE_GENERATION_METADATA_KEY: &str = "image_generation";

/// Provider 在强制最终回答阶段仍返回工具调用时的统一协议错误。
pub(crate) fn tool_calls_disabled_error() -> LlmError {
    LlmError::new(
        "tool_loop_limit",
        "tool loop returned tool calls when tool calls are disabled",
        "tool_loop",
    )
}

fn image_generation_enabled(req: &ChatRequest) -> bool {
    req.metadata
        .get(IMAGE_GENERATION_METADATA_KEY)
        .is_some_and(|value| value == "true")
}

pub(crate) use chat::{
    ChatCompletionsClient, chat_completions_stream, chat_completions_with_stream_fallback,
};
pub(crate) use chat_tool_loop::{
    begin_chat_completions_session, provider_chat_completions_tool_calling_protocol,
};
pub(crate) use configured::ConfiguredResponsesProvider;
pub(crate) use stream::is_openai_responses_done_sentinel;
pub(crate) use transport::{
    ResponsesTransportContext, openai_responses_url, send_openai_responses_request,
};

struct OpenAiChatFallbackRequest<'a> {
    api_mode: OpenAiApiMode,
    stream: bool,
    responses_client: &'a reqwest::Client,
    chat_client: &'a ChatCompletionsClient,
    api_key: &'a str,
    base_url: Option<&'a str>,
    responses_auth: Option<&'a HttpAuthConfig>,
    provider: &'a str,
    model: &'a str,
    media_max_bytes: u64,
    max_output_tokens: u64,
    reasoning_effort: Option<crate::provider::types::ReasoningEffort>,
    messages: &'a [ChatMessage],
    image_generation_enabled: bool,
}

/// OpenAI 提供商实现。
pub struct OpenAiProvider {
    /// 直连 Responses API 的 HTTP 客户端。
    responses_client: reqwest::Client,
    /// OpenAI 兼容 Chat Completions fallback 客户端。
    chat_client: ChatCompletionsClient,
    /// OpenAI API 密钥。
    api_key: String,
    /// 自定义 API 基础地址。
    base_url: Option<String>,
    /// 默认模型名称。
    model: String,
    api_mode: OpenAiApiMode,
    /// 是否启用流式传输。
    stream: bool,
    /// 单张本地图片允许转成 data URL 的最大字节数。
    media_max_bytes: u64,
    /// 最大输出令牌数。
    max_output_tokens: u64,
}

impl OpenAiProvider {
    /// 从 LLM 配置创建 OpenAI 提供商实例。
    pub fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let api_key = config
            .openai_api_key
            .clone()
            .ok_or_else(|| LlmError::config("OPENAI_API_KEY is required"))?;
        let http_client = qq_maid_common::http_client::try_builder()
            .map_err(|err| LlmError::config(format!("failed to configure OpenAI TLS: {err}")))?
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build OpenAI HTTP client: {err}"))
            })?;
        let chat_client = ChatCompletionsClient::new(
            api_key.clone(),
            config.openai_base_url.as_deref(),
            http_client.clone(),
        );

        Ok(Self {
            responses_client: http_client,
            chat_client,
            api_key,
            base_url: config.openai_base_url.clone(),
            model: openai_config_model(&config.model_route)?,
            api_mode: config.openai_api_mode,
            stream: config.stream,
            media_max_bytes: config.media_max_bytes,
            max_output_tokens: config.max_output_tokens,
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    /// 执行聊天补全，根据配置选择 Responses 或 Chat Completions。`model` 支持 `"openai:"` 前缀。
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        let effective_model = effective_openai_model(req.model.as_deref(), &self.model)?;
        let image_generation_enabled = image_generation_enabled(&req);
        openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: self.api_mode,
            stream: self.stream,
            responses_client: &self.responses_client,
            chat_client: &self.chat_client,
            api_key: &self.api_key,
            base_url: self.base_url.as_deref(),
            responses_auth: None,
            provider: self.name(),
            model: &effective_model,
            media_max_bytes: self.media_max_bytes,
            max_output_tokens: req.max_output_tokens.unwrap_or(self.max_output_tokens),
            reasoning_effort: req.reasoning_effort,
            messages: &req.messages,
            image_generation_enabled,
        })
        .await
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        let effective_model = effective_openai_model(req.model.as_deref(), &self.model)?;
        let image_generation_enabled = image_generation_enabled(&req);
        if !self.stream {
            let outcome = openai_chat_with_chat_fallback(OpenAiChatFallbackRequest {
                api_mode: self.api_mode,
                stream: false,
                responses_client: &self.responses_client,
                chat_client: &self.chat_client,
                api_key: &self.api_key,
                base_url: self.base_url.as_deref(),
                responses_auth: None,
                provider: self.name(),
                model: &effective_model,
                media_max_bytes: self.media_max_bytes,
                max_output_tokens: req.max_output_tokens.unwrap_or(self.max_output_tokens),
                reasoning_effort: req.reasoning_effort,
                messages: &req.messages,
                image_generation_enabled,
            })
            .await?;
            return Ok(outcome_to_stream(outcome));
        }
        openai_stream_with_chat_fallback(OpenAiChatFallbackRequest {
            api_mode: self.api_mode,
            stream: self.stream,
            responses_client: &self.responses_client,
            chat_client: &self.chat_client,
            api_key: &self.api_key,
            base_url: self.base_url.as_deref(),
            responses_auth: None,
            provider: self.name(),
            model: &effective_model,
            media_max_bytes: self.media_max_bytes,
            max_output_tokens: req.max_output_tokens.unwrap_or(self.max_output_tokens),
            reasoning_effort: req.reasoning_effort,
            messages: &req.messages,
            image_generation_enabled,
        })
        .await
    }

    async fn begin_agent_session(
        &self,
        req: AgentSessionRequest<'_>,
    ) -> Result<Option<Box<dyn AgentStepSession + Send>>, LlmError> {
        // 仅 Responses auto 模式适配 Tool Calling；ChatOnly 等返回 None，
        // 由 LlmProvider::chat_with_tools 默认实现安全回退到普通 chat。
        if self.tool_calling_protocol(req.chat.model.as_deref())
            != Some(ToolCallingProtocol::OpenAiResponses)
        {
            return Ok(None);
        }
        let effective_model = effective_openai_model(req.chat.model.as_deref(), &self.model)?;
        Ok(Some(Box::new(
            tool_loop::ResponsesAgentSession::new_with_image_generation(
                self.responses_client.clone(),
                self.api_key.clone(),
                self.base_url.clone(),
                self.name(),
                effective_model,
                self.media_max_bytes,
                req.chat.max_output_tokens.unwrap_or(self.max_output_tokens),
                req.chat.reasoning_effort,
                &req.chat.messages,
                req.tools,
                req.chat.context_budget,
                image_generation_enabled(req.chat),
            )?,
        )))
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        if self.api_mode == OpenAiApiMode::Auto
            && effective_openai_model(model, &self.model).is_ok()
        {
            Some(ToolCallingProtocol::OpenAiResponses)
        } else {
            None
        }
    }

    fn supports_vision(&self, model: Option<&str>) -> bool {
        effective_openai_model(model, &self.model).is_ok()
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stream_enabled(&self) -> bool {
        self.stream
    }
}

async fn openai_stream_with_chat_fallback(
    req: OpenAiChatFallbackRequest<'_>,
) -> Result<LlmStream, LlmError> {
    if !req.stream {
        let outcome = openai_chat_with_chat_fallback(req).await?;
        return Ok(outcome_to_stream(outcome));
    }
    match req.api_mode {
        OpenAiApiMode::Auto => openai_auto_stream_with_chat_fallback(req).await,
        OpenAiApiMode::ChatOnly => {
            chat::chat_completions_stream(
                req.chat_client,
                req.provider,
                req.model,
                req.media_max_bytes,
                req.max_output_tokens,
                req.messages,
                true,
            )
            .await
        }
    }
}

async fn openai_chat_with_chat_fallback(
    req: OpenAiChatFallbackRequest<'_>,
) -> Result<ChatOutcome, LlmError> {
    match req.api_mode {
        OpenAiApiMode::Auto => openai_auto_chat_with_chat_fallback(req).await,
        OpenAiApiMode::ChatOnly => {
            chat_completions_with_stream_fallback(
                req.stream,
                req.chat_client,
                req.provider,
                req.model,
                req.media_max_bytes,
                req.max_output_tokens,
                req.messages,
            )
            .await
        }
    }
}

async fn openai_auto_stream_with_chat_fallback(
    req: OpenAiChatFallbackRequest<'_>,
) -> Result<LlmStream, LlmError> {
    let responses_req = responses::OpenAiResponsesChatRequest {
        stream: true,
        client: req.responses_client,
        api_key: req.api_key,
        base_url: req.base_url,
        auth: req.responses_auth,
        provider: req.provider,
        model: req.model,
        media_max_bytes: req.media_max_bytes,
        max_output_tokens: req.max_output_tokens,
        reasoning_effort: req.reasoning_effort,
        messages: req.messages,
        allow_completed_response_fallback: true,
        image_generation_enabled: req.image_generation_enabled,
    };
    match responses::openai_responses_chat_stream(&responses_req).await {
        Ok(stream) => Ok(openai_responses_runtime_fallback_stream(stream, req)),
        Err(err) if fallback::should_fallback_to_chat_after_responses_error(&err) => {
            tracing::warn!(
                provider = req.provider,
                model = %req.model,
                error_code = err.code.as_str(),
                error_stage = err.stage.as_str(),
                "OpenAI Responses 流初始化失败，将降级到 Chat Completions 流"
            );
            chat::chat_completions_stream(
                req.chat_client,
                req.provider,
                req.model,
                req.media_max_bytes,
                req.max_output_tokens,
                req.messages,
                true,
            )
            .await
        }
        Err(err) => Err(err),
    }
}

fn openai_responses_runtime_fallback_stream(
    responses_stream: LlmStream,
    req: OpenAiChatFallbackRequest<'_>,
) -> LlmStream {
    Box::pin(futures_stream::unfold(
        OpenAiRuntimeFallbackStreamState {
            responses_stream: Some(responses_stream),
            chat_stream: None,
            chat_client: req.chat_client.clone(),
            provider: req.provider.to_owned(),
            model: req.model.to_owned(),
            media_max_bytes: req.media_max_bytes,
            max_output_tokens: req.max_output_tokens,
            messages: req.messages.to_vec(),
            emitted_non_empty_delta: false,
            fallback_used: false,
            done: false,
        },
        |mut state| async move {
            let event = next_openai_runtime_fallback_event(&mut state).await;
            event.map(|event| (event, state))
        },
    ))
}

struct OpenAiRuntimeFallbackStreamState {
    responses_stream: Option<LlmStream>,
    chat_stream: Option<LlmStream>,
    chat_client: ChatCompletionsClient,
    provider: String,
    model: String,
    media_max_bytes: u64,
    max_output_tokens: u64,
    messages: Vec<ChatMessage>,
    emitted_non_empty_delta: bool,
    fallback_used: bool,
    done: bool,
}

async fn next_openai_runtime_fallback_event(
    state: &mut OpenAiRuntimeFallbackStreamState,
) -> Option<Result<LlmStreamEvent, LlmError>> {
    loop {
        if state.done {
            return None;
        }
        if let Some(stream) = state.responses_stream.as_mut() {
            match stream.next().await {
                Some(Ok(LlmStreamEvent::TextDelta(delta))) => {
                    if !delta.is_empty() {
                        state.emitted_non_empty_delta = true;
                    }
                    return Some(Ok(LlmStreamEvent::TextDelta(delta)));
                }
                Some(Ok(LlmStreamEvent::OutputPart(part))) => {
                    state.emitted_non_empty_delta = true;
                    return Some(Ok(LlmStreamEvent::OutputPart(part)));
                }
                Some(Ok(LlmStreamEvent::Completed {
                    usage,
                    finish_reason,
                    fallback_used,
                })) => {
                    state.done = true;
                    return Some(Ok(LlmStreamEvent::Completed {
                        usage,
                        finish_reason,
                        fallback_used: fallback_used || state.fallback_used,
                    }));
                }
                Some(Err(err)) => {
                    if state.emitted_non_empty_delta
                        || !fallback::should_fallback_to_chat_after_responses_error(&err)
                    {
                        state.done = true;
                        return Some(Err(err));
                    }
                    tracing::warn!(
                        provider = state.provider.as_str(),
                        model = %state.model,
                        error_code = err.code.as_str(),
                        error_stage = err.stage.as_str(),
                        "OpenAI Responses 流在首个增量前失败，将降级到 Chat Completions 流"
                    );
                    state.responses_stream = None;
                    match chat::chat_completions_stream(
                        &state.chat_client,
                        &state.provider,
                        &state.model,
                        state.media_max_bytes,
                        state.max_output_tokens,
                        &state.messages,
                        true,
                    )
                    .await
                    {
                        Ok(stream) => {
                            state.chat_stream = Some(stream);
                            state.fallback_used = true;
                        }
                        Err(err) => {
                            state.done = true;
                            return Some(Err(err));
                        }
                    }
                    continue;
                }
                None => {
                    state.done = true;
                    return Some(Err(LlmError::provider(
                        "OpenAI Responses stream ended without completion event",
                        "stream",
                    )));
                }
            }
        }

        let Some(stream) = state.chat_stream.as_mut() else {
            state.done = true;
            return None;
        };
        match stream.next().await {
            Some(Ok(LlmStreamEvent::TextDelta(delta))) => {
                if !delta.is_empty() {
                    state.emitted_non_empty_delta = true;
                }
                return Some(Ok(LlmStreamEvent::TextDelta(delta)));
            }
            Some(Ok(LlmStreamEvent::OutputPart(part))) => {
                return Some(Ok(LlmStreamEvent::OutputPart(part)));
            }
            Some(Ok(LlmStreamEvent::Completed {
                usage,
                finish_reason,
                fallback_used,
            })) => {
                state.done = true;
                return Some(Ok(LlmStreamEvent::Completed {
                    usage,
                    finish_reason,
                    fallback_used: fallback_used || state.fallback_used,
                }));
            }
            Some(Err(err)) => {
                state.done = true;
                return Some(Err(err));
            }
            None => {
                state.done = true;
                return Some(Err(LlmError::provider(
                    "Chat Completions fallback stream ended without completion event",
                    "stream",
                )));
            }
        }
    }
}

async fn openai_auto_chat_with_chat_fallback(
    req: OpenAiChatFallbackRequest<'_>,
) -> Result<ChatOutcome, LlmError> {
    match responses::openai_responses_chat_with_stream_fallback(
        responses::OpenAiResponsesChatRequest {
            stream: req.stream,
            client: req.responses_client,
            api_key: req.api_key,
            base_url: req.base_url,
            auth: req.responses_auth,
            provider: req.provider,
            model: req.model,
            media_max_bytes: req.media_max_bytes,
            max_output_tokens: req.max_output_tokens,
            reasoning_effort: req.reasoning_effort,
            messages: req.messages,
            allow_completed_response_fallback: true,
            image_generation_enabled: req.image_generation_enabled,
        },
    )
    .await
    {
        Ok(outcome) => Ok(outcome),
        Err(err) if fallback::should_fallback_to_chat_after_responses_error(&err) => {
            tracing::warn!(
                provider = req.provider,
                model = %req.model,
                error_code = err.code.as_str(),
                error_stage = err.stage.as_str(),
                "OpenAI Responses 对话失败，将降级到 Chat Completions"
            );
            chat_completions_with_stream_fallback(
                req.stream,
                req.chat_client,
                req.provider,
                req.model,
                req.media_max_bytes,
                req.max_output_tokens,
                req.messages,
            )
            .await
        }
        Err(err) => Err(err),
    }
}

/// 验证并解析 OpenAI 的配置模型名。
pub(crate) fn openai_config_model(route: &ModelRoute) -> Result<String, LlmError> {
    route
        .candidates()
        .iter()
        .find_map(|model| match model.provider.as_ref() {
            Some(ModelProvider::OpenAi) | None => Some(model.name.clone()),
            Some(ModelProvider::DeepSeek)
            | Some(ModelProvider::BigModel)
            | Some(ModelProvider::Gemini)
            | Some(ModelProvider::Custom(_)) => None,
        })
        .ok_or_else(|| {
            LlmError::config(
                "LLM_MODEL for OpenAI provider must include openai: prefix or no prefix",
            )
        })
}

/// 决定本次请求实际使用的模型名称。
fn effective_openai_model(
    override_model: Option<&str>,
    default_model: &str,
) -> Result<String, LlmError> {
    let Some(value) = override_model else {
        return Ok(default_model.to_owned());
    };
    let model = crate::provider::types::ModelId::parse(value, "request")?;
    match model.provider {
        Some(ModelProvider::OpenAi) | None => Ok(model.name),
        Some(ModelProvider::DeepSeek)
        | Some(ModelProvider::BigModel)
        | Some(ModelProvider::Gemini)
        | Some(ModelProvider::Custom(_)) => Err(LlmError::new(
            "bad_request",
            "non-openai-prefixed model cannot be used by OpenAI provider",
            "request",
        )),
    }
}

#[cfg(test)]
mod tests;

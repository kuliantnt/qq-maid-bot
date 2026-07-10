//! LLM 提供商抽象层。
//!
//! 定义了统一的 [`LlmProvider`] trait，屏蔽不同 LLM API（OpenAI、DeepSeek、BigModel、Gemini）的差异。
//! 同时提供通用模型候选链路由逻辑，以及 [`ChatOutcome`] 等通用类型。
//!
//! 本模块作为 provider-facing 的公开入口；模型候选链的执行、流式状态、route 配置预检和
//! 失败聚合等基础设施分别拆分到 `routing`、`stream_state`、`route_config`、`route_error` 子模块，
//! 这里仅做组装与 re-export。

pub mod bigmodel;
pub mod deepseek;
pub mod gemini;
pub mod limiter;
pub mod openai;
pub mod openai_compatible;
mod route_config;
mod route_error;
mod routing;
pub mod status;
mod stream_state;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
pub(crate) mod tool_loop;
pub mod types;

use std::{pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};

use crate::{
    agent_loop::{
        AgentSessionRequest, AgentStepSession, AgentTextDeltaSink, ToolLoopProgressSink,
        run_agent_loop_with_handle,
    },
    config::{LlmConfig, ProviderMode},
    error::LlmError,
    metrics::{LlmMetrics, MetricsRecorder},
    provider::types::{ChatRequest, ModelProvider, TokenUsage},
    tool::{ToolContext, ToolRegistry},
};

pub use crate::agent_loop::{
    AgentRunDiagnostics, AgentRunHandle, AgentStopReason, ToolExecutionResult,
};

// 候选链构建与 provider 预检 helper 来源于拆分后的子模块，这里 `use` 进来同时供
// `build_provider` 与测试模块（`tests` 通过 `use super::*` 引用）复用。
use route_config::{
    auto_default_route, auto_provider_routes, available_provider_kinds_for_routes,
    ensure_custom_providers_declared, ensure_route_supported,
};
use routing::ModelRouteProvider;

// `should_try_next_model` 仅在测试中从 mod 入口处直接断言，运行期由 `routing` /
// `stream_state` 各自引入，因此用 `cfg(test)` 标注，避免出现 unused import 警告。
#[cfg(test)]
use route_error::should_try_next_model;
// `ModelRoute` 仅在测试中通过 `use super::*` 引用解析模型配置；运行期 `build_provider` 只通过
// `config.model_route` 字段间接使用该类型，不需要在 mod 入口处直接命名。
#[cfg(test)]
use crate::provider::types::ModelRoute;

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
    /// Agent Runtime 的结构化执行轨迹；普通聊天使用默认空轨迹。
    pub agent: AgentRunDiagnostics,
}

/// 原生 Tool Calling 请求。
#[derive(Clone)]
pub struct ToolChatRequest {
    /// 基础聊天请求。
    pub chat: ChatRequest,
    /// 服务端白名单工具。
    pub tools: ToolRegistry,
    /// 服务端生成的 Tool 执行上下文。
    pub tool_context: ToolContext,
    /// 最多允许执行工具调用轮数。
    pub max_rounds: usize,
    /// Tool Loop 执行进度回调；未传入时保持完全静默的旧行为。
    ///
    /// 回调 `Err` 被视为取消/接收端关闭，会中断 Tool Loop；它不是普通日志 hook。
    pub progress_sink: Option<ToolLoopProgressSink>,
    /// Tool Loop 最终回答的真实 provider 文本增量接收器。
    ///
    /// 只用于已经确认属于最终回答的正文；工具调用轮的模型草稿不得通过该 sink 外发。
    pub final_delta_sink: Option<AgentTextDeltaSink>,
    /// 与 Core 共享的 Agent 轨迹和取消边界。
    pub run_handle: Option<AgentRunHandle>,
}

/// Provider 已适配的 Tool Calling 协议类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallingProtocol {
    /// OpenAI Responses `function_call` / `function_call_output` 协议。
    OpenAiResponses,
    /// OpenAI 兼容 Chat Completions `tools` / `tool_calls` 协议。
    ChatCompletionsToolCalls,
}

/// LLM 标准聊天流事件。
///
/// `Completed` 是每条流唯一的成功终止状态，usage 与 finish reason 都随终止事件返回；
/// Provider 在协议级完成信号已经足够定稿时应立即产出该事件，不得依赖 HTTP EOF。
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
    /// 当前 provider 对指定模型可用的 Tool Calling 协议；未适配时返回 `None`。
    fn tool_calling_protocol(&self, _model: Option<&str>) -> Option<ToolCallingProtocol> {
        None
    }
    /// 当前 provider 是否能接收图片输入。未适配多模态的 provider 必须保守返回 false。
    fn supports_vision(&self, _model: Option<&str>) -> bool {
        false
    }
    /// 开始一个 Provider 无关的 Agent Loop 单步会话。
    ///
    /// 未适配 Tool Calling 的 provider 应返回 `Ok(None)`，默认 `chat_with_tools`
    /// 会据此安全回退到普通 `chat`，保留旧路径。适配方只需把各自协议的一次模型
    /// 请求转换为统一 [`AgentStep`](crate::agent_loop::AgentStep)，不应在此决定
    /// 最大轮数或 Loop 退出条件——那是 `run_agent_loop` 的统一职责。
    async fn begin_agent_session(
        &self,
        _req: AgentSessionRequest<'_>,
    ) -> Result<Option<Box<dyn AgentStepSession + Send>>, LlmError> {
        Ok(None)
    }
    /// 使用模型原生 Tool Calling 执行聊天。
    ///
    /// 默认实现：若 `begin_agent_session` 返回会话，则走统一 [`run_agent_loop`]；
    /// 否则回退普通 `chat`，避免未适配 provider 回归。
    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        let ToolChatRequest {
            chat,
            tools,
            tool_context,
            max_rounds,
            progress_sink,
            final_delta_sink,
            run_handle,
        } = req;
        match self
            .begin_agent_session(AgentSessionRequest {
                chat: &chat,
                tools: &tools,
            })
            .await?
        {
            Some(session) => {
                run_agent_loop_with_handle(
                    session,
                    tools,
                    tool_context,
                    max_rounds,
                    progress_sink,
                    final_delta_sink,
                    run_handle,
                )
                .await
            }
            None => self.chat(chat).await,
        }
    }
    /// 提供商名称，例如 "openai"、"deepseek"、"bigmodel"。
    fn name(&self) -> &str;
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
        agent: Default::default(),
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
/// - `Gemini`：仅使用 Google Gemini 提供商。
/// - `Auto`：根据模型候选链路由；单 OpenAI 主模型仍兼容原 OpenAI -> DeepSeek fallback。
pub fn build_provider(config: &LlmConfig) -> Result<DynLlmProvider, LlmError> {
    let configured_custom_providers = config
        .openai_compatible_providers
        .iter()
        .map(|provider| provider.id.clone())
        .collect::<Vec<_>>();
    ensure_custom_providers_declared(
        &config.configured_model_routes,
        &configured_custom_providers,
    )?;

    match config.provider {
        ProviderMode::OpenAi => {
            for (name, route) in &config.configured_model_routes {
                ensure_route_supported(
                    route,
                    &ModelProvider::OpenAi,
                    &ModelProvider::OpenAi,
                    name,
                )?;
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
                    &ModelProvider::DeepSeek,
                    &ModelProvider::DeepSeek,
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
                    &ModelProvider::BigModel,
                    &ModelProvider::BigModel,
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
        ProviderMode::Gemini => {
            for (name, route) in &config.configured_model_routes {
                ensure_route_supported(
                    route,
                    &ModelProvider::Gemini,
                    &ModelProvider::Gemini,
                    name,
                )?;
            }
            let provider: DynLlmProvider = Arc::new(gemini::GeminiProvider::new(config)?);
            Ok(Arc::new(ModelRouteProvider::new(
                "gemini",
                ModelProvider::Gemini,
                config.model_route.clone(),
                vec![(ModelProvider::Gemini, provider)],
            )?))
        }
        ProviderMode::Auto => {
            let route = auto_default_route(config)?;
            let provider_routes = auto_provider_routes(config, &route)?;
            let required_providers = available_provider_kinds_for_routes(
                config,
                &provider_routes,
                &ModelProvider::OpenAi,
            );
            let mut providers: Vec<(ModelProvider, DynLlmProvider)> = Vec::new();

            if required_providers.is_empty() {
                return Err(LlmError::config(
                    "no LLM provider is available for auto model routes; configure an API key for at least one provider referenced by LLM_MODEL or agent model_routes",
                ));
            }

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
                    ModelProvider::Gemini => providers.push((
                        ModelProvider::Gemini,
                        Arc::new(gemini::GeminiProvider::new(config)?),
                    )),
                    ModelProvider::Custom(_) => {
                        let provider_config = config
                            .openai_compatible_providers
                            .iter()
                            .find(|entry| entry.id == provider_kind)
                            .ok_or_else(|| {
                                LlmError::config(format!(
                                    "provider `{}` is referenced by model routes but not configured",
                                    provider_kind.as_str()
                                ))
                            })?;
                        let default_model =
                            first_model_for_provider(&provider_routes, &provider_kind)
                                .unwrap_or_else(|| provider_kind.as_str().to_owned());
                        providers.push((
                            provider_kind.clone(),
                            Arc::new(openai_compatible::OpenAiCompatibleProvider::new(
                                provider_config,
                                default_model,
                                config.stream,
                                config.request_timeout_seconds,
                                config.media_max_bytes,
                                config.max_output_tokens,
                            )?),
                        ));
                    }
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

fn first_model_for_provider(
    routes: &[(String, crate::provider::types::ModelRoute)],
    provider: &ModelProvider,
) -> Option<String> {
    routes.iter().find_map(|(_, route)| {
        route.candidates().iter().find_map(|candidate| {
            (candidate.provider.as_ref() == Some(provider)).then(|| candidate.name.clone())
        })
    })
}

//! LLM 提供商抽象层。
//!
//! 定义了统一的 [`LlmProvider`] trait，屏蔽不同 LLM API（OpenAI、DeepSeek）的差异。
//! 同时提供通用模型候选链路由逻辑，以及 [`ChatOutcome`] 等通用类型。

pub mod deepseek;
pub mod openai;
pub mod types;

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    config::{AppConfig, ProviderMode},
    error::LlmError,
    provider::types::{ChatRequest, ModelId, ModelProvider, ModelRoute, TokenUsage},
    util::metrics::LlmMetrics,
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
}

/// LLM 提供商统一接口。
///
/// 所有后端（OpenAI、DeepSeek 等）必须实现此 trait。
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 发送聊天请求并返回结果。
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError>;
    /// 提供商名称，例如 "openai"、"deepseek"。
    fn name(&self) -> &'static str;
    /// 当前使用的模型名称。
    fn model(&self) -> &str;
    /// 是否启用了流式传输。
    fn stream_enabled(&self) -> bool;
}

/// 线程安全的 LLM 提供商智能指针别名。
pub type DynLlmProvider = Arc<dyn LlmProvider>;

/// 根据配置构建 LLM 提供商实例。
///
/// - `OpenAi`：仅使用 OpenAI 提供商。
/// - `DeepSeek`：仅使用 DeepSeek 提供商。
/// - `Auto`：根据模型候选链路由；单 OpenAI 主模型仍兼容原 OpenAI -> DeepSeek fallback。
pub fn build_provider(config: &AppConfig) -> Result<DynLlmProvider, LlmError> {
    match config.provider {
        ProviderMode::OpenAi => {
            ensure_route_supported(
                &config.model_route,
                ModelProvider::OpenAi,
                ModelProvider::OpenAi,
                "LLM_MODEL",
            )?;
            let provider: DynLlmProvider = Arc::new(openai::OpenAiRigProvider::new(config)?);
            Ok(Arc::new(ModelRouteProvider::new(
                "openai",
                ModelProvider::OpenAi,
                config.model_route.clone(),
                vec![(ModelProvider::OpenAi, provider)],
            )?))
        }
        ProviderMode::DeepSeek => {
            ensure_route_supported(
                &config.model_route,
                ModelProvider::DeepSeek,
                ModelProvider::DeepSeek,
                "LLM_MODEL",
            )?;
            let provider: DynLlmProvider = Arc::new(deepseek::DeepSeekRigProvider::new(config)?);
            Ok(Arc::new(ModelRouteProvider::new(
                "deepseek",
                ModelProvider::DeepSeek,
                config.model_route.clone(),
                vec![(ModelProvider::DeepSeek, provider)],
            )?))
        }
        ProviderMode::Auto => {
            let route = auto_default_route(config)?;
            let mut providers: Vec<(ModelProvider, DynLlmProvider)> = Vec::new();

            if route_uses_provider(&route, ModelProvider::OpenAi, ModelProvider::OpenAi) {
                providers.push((
                    ModelProvider::OpenAi,
                    Arc::new(openai::OpenAiRigProvider::new(config)?),
                ));
            }
            if route_uses_provider(&route, ModelProvider::DeepSeek, ModelProvider::OpenAi) {
                let provider = config.deepseek_api_key.as_ref().ok_or_else(|| {
                    LlmError::config(
                        "DEEPSEEK_API_KEY is required because model route includes DeepSeek",
                    )
                })?;
                if provider.trim().is_empty() {
                    return Err(LlmError::config(
                        "DEEPSEEK_API_KEY is required because model route includes DeepSeek",
                    ));
                }
                providers.push((
                    ModelProvider::DeepSeek,
                    Arc::new(deepseek::DeepSeekRigProvider::new(config)?),
                ));
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
/// 先执行 OpenAI/DeepSeek 各自内部的 Responses、Chat Completions、空流补非流等
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
                Ok(outcome) => {
                    tracing::info!(
                        task,
                        candidate_index = index,
                        provider = provider_kind.as_str(),
                        model = %candidate.name,
                        result = "success",
                        "model candidate succeeded"
                    );
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

fn auto_default_route(config: &AppConfig) -> Result<ModelRoute, LlmError> {
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
        provider::types::{ChatMessage, ChatRequest},
        util::metrics::LlmMetrics,
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
        }
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
        assert_eq!(openai.calls(), 1);
        assert_eq!(deepseek.calls(), 1);
        assert_eq!(
            deepseek.requests()[0].model.as_deref(),
            Some("deepseek:deepseek-chat")
        );
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

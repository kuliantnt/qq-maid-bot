//! LLM 提供商抽象层。
//!
//! 定义了统一的 [`LlmProvider`] trait，屏蔽不同 LLM API（OpenAI、DeepSeek）的差异。
//! 同时提供 [`FallbackProvider`] 主备切换逻辑，以及 [`ChatOutcome`] 等通用类型。

pub mod deepseek;
pub mod openai;
pub mod types;

use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    config::{AppConfig, ProviderMode},
    error::LlmError,
    provider::types::{ChatRequest, ModelId, ModelProvider, TokenUsage},
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
/// - `Auto`：以 OpenAI 为主，若配置了 DeepSeek API Key 则创建 FallbackProvider。
pub fn build_provider(config: &AppConfig) -> Result<DynLlmProvider, LlmError> {
    match config.provider {
        ProviderMode::OpenAi => Ok(Arc::new(openai::OpenAiRigProvider::new(config)?)),
        ProviderMode::DeepSeek => Ok(Arc::new(deepseek::DeepSeekRigProvider::new(config)?)),
        ProviderMode::Auto => {
            let primary: DynLlmProvider = Arc::new(openai::OpenAiRigProvider::new(config)?);
            if config.deepseek_api_key.is_none() {
                return Ok(primary);
            }

            let fallback: DynLlmProvider = Arc::new(deepseek::DeepSeekRigProvider::new(config)?);
            Ok(Arc::new(FallbackProvider::new(primary, fallback)))
        }
    }
}

/// 主备切换提供商。
///
/// 先尝试 `primary`（OpenAI），若遇到可恢复错误（超时、提供商错误等）
/// 则自动切换到 `fallback`（DeepSeek）。若请求显式指定了模型归属，
/// 则直接路由到对应提供商，不走主备逻辑。
struct FallbackProvider {
    primary: DynLlmProvider,
    fallback: DynLlmProvider,
}

impl FallbackProvider {
    fn new(primary: DynLlmProvider, fallback: DynLlmProvider) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl LlmProvider for FallbackProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        match requested_model_provider(req.model.as_deref())? {
            Some(ModelProvider::OpenAi) => return self.primary.chat(req).await,
            Some(ModelProvider::DeepSeek) => return self.fallback.chat(req).await,
            None => {}
        }

        match self.primary.chat(req.clone()).await {
            Ok(outcome) => Ok(outcome),
            Err(err) if is_recoverable(&err) => {
                tracing::warn!(
                    error_code = err.code,
                    error_stage = err.stage,
                    fallback_provider = self.fallback.name(),
                    "primary LLM provider failed; trying fallback provider"
                );
                self.fallback.chat(req).await
            }
            Err(err) => Err(err),
        }
    }

    fn name(&self) -> &'static str {
        "auto"
    }

    fn model(&self) -> &str {
        self.primary.model()
    }

    fn stream_enabled(&self) -> bool {
        self.primary.stream_enabled()
    }
}

/// 从可选的模型字符串中解析出指定的提供商。
///
/// 例如 `"deepseek:deepseek-chat"` 返回 `Some(ModelProvider::DeepSeek)`。
fn requested_model_provider(model: Option<&str>) -> Result<Option<ModelProvider>, LlmError> {
    model
        .map(|value| ModelId::parse(value, "request").map(|model| model.provider))
        .transpose()
        .map(Option::flatten)
}

/// 判断错误是否可恢复（值得触发 fallback 重试）。
///
/// 可恢复错误类型：超时、提供商内部错误、HTTP 通信错误。
fn is_recoverable(err: &LlmError) -> bool {
    matches!(
        err.code.as_str(),
        "timeout" | "provider_error" | "http_error"
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

    #[test]
    fn provider_errors_are_recoverable() {
        assert!(is_recoverable(&LlmError::provider(
            "upstream failed",
            "provider"
        )));
        assert!(is_recoverable(&LlmError::timeout("request")));
        assert!(!is_recoverable(&LlmError::config("missing key")));
    }

    /// 合并 4 个 FallbackProvider 异步测试为表驱动测试。
    #[tokio::test]
    async fn fallback_provider_routes_and_falls_back() {
        struct Case {
            name: &'static str,
            primary_results: Vec<Result<ChatOutcome, LlmError>>,
            fallback_results: Vec<Result<ChatOutcome, LlmError>>,
            request_model: Option<&'static str>,
            /// Some(reply) 表示预期 Ok，None 表示预期 Err
            expected_reply: Option<&'static str>,
            expected_err_code: Option<&'static str>,
            expected_primary_calls: usize,
            expected_fallback_calls: usize,
        }

        let cases = [
            Case {
                name: "fallback_provider_uses_primary_on_success",
                primary_results: vec![Ok(outcome("primary"))],
                fallback_results: vec![Ok(outcome("fallback"))],
                request_model: None,
                expected_reply: Some("primary"),
                expected_err_code: None,
                expected_primary_calls: 1,
                expected_fallback_calls: 0,
            },
            Case {
                name: "fallback_provider_falls_back_on_recoverable_error",
                primary_results: vec![Err(LlmError::timeout("provider"))],
                fallback_results: vec![Ok(outcome("fallback"))],
                request_model: None,
                expected_reply: Some("fallback"),
                expected_err_code: None,
                expected_primary_calls: 1,
                expected_fallback_calls: 1,
            },
            Case {
                name: "fallback_provider_routes_provider_prefixed_models_directly",
                primary_results: vec![Ok(outcome("primary"))],
                fallback_results: vec![Ok(outcome("fallback"))],
                request_model: Some("deepseek:deepseek-chat"),
                expected_reply: Some("fallback"),
                expected_err_code: None,
                expected_primary_calls: 0,
                expected_fallback_calls: 1,
            },
            Case {
                name: "fallback_provider_keeps_nonrecoverable_error",
                primary_results: vec![Err(LlmError::config("missing key"))],
                fallback_results: vec![Ok(outcome("fallback"))],
                request_model: None,
                expected_reply: None,
                expected_err_code: Some("config"),
                expected_primary_calls: 1,
                expected_fallback_calls: 0,
            },
        ];

        for case in &cases {
            let primary = Arc::new(MockProvider::new("openai", case.primary_results.clone()));
            let fallback = Arc::new(MockProvider::new("deepseek", case.fallback_results.clone()));
            let provider = FallbackProvider::new(primary.clone(), fallback.clone());

            let mut req = request();
            if let Some(m) = case.request_model {
                req.model = Some(m.to_owned());
            }

            let result = provider.chat(req).await;

            match case.expected_reply {
                Some(expected_reply) => {
                    let outcome = result.unwrap_or_else(|e| {
                        panic!("case '{}' failed: expected Ok, got Err {:?}", case.name, e)
                    });
                    assert_eq!(
                        outcome.reply, expected_reply,
                        "case '{}' failed: reply mismatch",
                        case.name
                    );
                }
                None => {
                    let err = match result {
                        Err(e) => e,
                        Ok(_) => panic!("case '{}' failed: expected Err, got Ok", case.name),
                    };
                    assert_eq!(
                        err.code,
                        case.expected_err_code.unwrap(),
                        "case '{}' failed: error code mismatch",
                        case.name
                    );
                }
            }

            assert_eq!(
                primary.calls(),
                case.expected_primary_calls,
                "case '{}' failed: primary calls mismatch",
                case.name
            );
            assert_eq!(
                fallback.calls(),
                case.expected_fallback_calls,
                "case '{}' failed: fallback calls mismatch",
                case.name
            );

            // 原 fallback_provider_routes_provider_prefixed_models_directly 的断言：
            // 验证带 provider 前缀的 model 被正确转发到目标 provider。
            if case.request_model.is_some() && case.expected_fallback_calls > 0 {
                assert_eq!(
                    fallback.requests()[0].model.as_deref(),
                    case.request_model,
                    "case '{}' failed: fallback request model not forwarded",
                    case.name
                );
            }
        }
    }
}

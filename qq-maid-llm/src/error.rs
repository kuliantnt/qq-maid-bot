//! 应用错误类型。定义 `LlmError` 主错误结构体及其便捷构造方法，
//! 以及序列化友好的 `ErrorInfo` 表示。

use std::{error::Error as StdError, io};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agent_loop::AgentRunDiagnostics;

#[derive(Debug, Clone, PartialEq, Eq)]
struct UpstreamErrorContext {
    provider: String,
    model: String,
}

/// 可序列化的错误信息，用于 HTTP 响应或 API 返回。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorInfo {
    /// 错误分类码
    pub code: String,
    /// 人类可读的错误描述
    pub message: String,
    /// 错误发生的阶段
    pub stage: String,
}

/// LLM 调用失败的稳定分类。
///
/// Provider 可以保留自己的错误码与阶段，但路由、日志和用户提示应优先消费这一层，
/// 避免把客户端超时、连接失败和上游 5xx 都压成同一个“服务不可用”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmErrorKind {
    Timeout,
    Authentication,
    RateLimit,
    UpstreamUnavailable,
    Network,
    InvalidRequest,
    Other,
}

impl LlmErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Authentication => "authentication",
            Self::RateLimit => "rate_limit",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::Network => "network",
            Self::InvalidRequest => "invalid_request",
            Self::Other => "other",
        }
    }
}

/// 应用主错误类型，携带代码、消息和阶段信息。
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{code}@{stage}: {message}")]
pub struct LlmError {
    /// 错误分类码（如 config、timeout、provider_error）
    pub code: String,
    /// 人类可读的错误描述
    pub message: String,
    /// 错误发生的阶段（如 config、http、realtime）
    pub stage: String,
    /// 上游 HTTP 状态码；仅在确实收到上游响应时填写，不从错误文本反向猜测。
    pub upstream_status: Option<u16>,
    /// 搜索上游的低敏路由身份；装箱避免增大每个通用错误返回值。
    upstream_context: Option<Box<UpstreamErrorContext>>,
    /// Responses `response.incomplete` 的低敏协议原因；不进入用户错误正文。
    incomplete_reason: Option<String>,
    /// Agent Runtime 失败时已经发生的可信执行轨迹。
    pub agent: Option<Box<AgentRunDiagnostics>>,
}

impl LlmError {
    /// 创建通用错误。
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        stage: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            stage: stage.into(),
            upstream_status: None,
            upstream_context: None,
            incomplete_reason: None,
            agent: None,
        }
    }

    /// 创建配置类错误。
    pub fn config(message: impl Into<String>) -> Self {
        Self::new("config", message, "config")
    }

    /// 创建超时类错误。
    pub fn timeout(stage: impl Into<String>) -> Self {
        Self::new("timeout", "LLM request timed out", stage)
    }

    /// 从带 source chain 的底层错误创建 LLM 错误。
    ///
    /// 结构化的 reqwest / io 超时优先于 fallback；无法识别时才采用调用点给出的
    /// fallback。消息保留错误链供脱敏日志排障，用户侧不得直接展示该消息。
    pub fn from_error_source(
        error: &(dyn StdError + 'static),
        fallback: LlmErrorKind,
        stage: impl Into<String>,
        context: impl AsRef<str>,
    ) -> Self {
        let detected = classify_error_source(error);
        let kind = if detected == LlmErrorKind::Other {
            fallback
        } else {
            detected
        };
        Self::from_kind(
            kind,
            format!("{}: {}", context.as_ref(), error_chain_message(error)),
            stage,
        )
    }

    /// 映射读取非流式响应正文时的 reqwest 错误：超时/断链属于传输阶段，纯 JSON
    /// 解码失败仍属于上游响应协议错误。
    pub fn from_response_source(
        error: &(dyn StdError + 'static),
        context: impl AsRef<str>,
    ) -> Self {
        let detected = classify_error_source(error);
        let stage = if matches!(detected, LlmErrorKind::Timeout | LlmErrorKind::Network) {
            "response_read"
        } else {
            "json"
        };
        Self::from_error_source(error, LlmErrorKind::Other, stage, context)
    }

    /// 按稳定分类构造错误，同时保留项目既有错误码以兼容调用方。
    pub fn from_kind(
        kind: LlmErrorKind,
        message: impl Into<String>,
        stage: impl Into<String>,
    ) -> Self {
        let code = match kind {
            LlmErrorKind::Timeout => "timeout",
            LlmErrorKind::Authentication => "authentication_failed",
            LlmErrorKind::RateLimit => "rate_limited",
            LlmErrorKind::UpstreamUnavailable => "upstream_unavailable",
            LlmErrorKind::Network => "network_error",
            LlmErrorKind::InvalidRequest => "bad_request",
            LlmErrorKind::Other => "provider_error",
        };
        Self::new(code, message, stage)
    }

    /// 根据真实上游 HTTP 状态建立错误分类；调用方仍可在此之前处理安全拦截等
    /// 协议特例，但不得从响应正文反向猜测状态。
    pub fn from_upstream_status(
        status: u16,
        message: impl Into<String>,
        stage: impl Into<String>,
    ) -> Self {
        let kind = match status {
            401 | 403 => LlmErrorKind::Authentication,
            429 => LlmErrorKind::RateLimit,
            500..=599 => LlmErrorKind::UpstreamUnavailable,
            400..=499 => LlmErrorKind::InvalidRequest,
            _ => LlmErrorKind::Other,
        };
        Self::from_kind(kind, message, stage).with_upstream_status(status)
    }

    /// 创建供应商接口类错误。
    pub fn provider(message: impl Into<String>, stage: impl Into<String>) -> Self {
        Self::new("provider_error", message, stage)
    }

    /// 创建 HTTP 类错误。
    pub fn http(message: impl Into<String>) -> Self {
        Self::new("http_error", message, "http")
    }

    /// 将错误转为可序列化的 ErrorInfo。
    pub fn as_info(&self) -> ErrorInfo {
        ErrorInfo {
            code: self.code.clone(),
            message: self.message.clone(),
            stage: self.stage.clone(),
        }
    }

    pub fn with_agent(mut self, diagnostics: AgentRunDiagnostics) -> Self {
        self.agent = Some(Box::new(diagnostics));
        self
    }

    /// 附加真实上游 HTTP 状态，供重试判定和低敏结构化日志使用。
    pub fn with_upstream_status(mut self, status: u16) -> Self {
        self.upstream_status = Some(status);
        self
    }

    /// 附加低敏搜索路由身份，避免日志把发起 Tool Call 的模型误当成搜索上游。
    pub fn with_upstream_context(
        mut self,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        if self.upstream_context.is_none() {
            self.upstream_context = Some(Box::new(UpstreamErrorContext {
                provider: provider.into(),
                model: model.into(),
            }));
        }
        self
    }

    pub fn upstream_provider(&self) -> Option<&str> {
        self.upstream_context
            .as_deref()
            .map(|context| context.provider.as_str())
    }

    pub fn upstream_model(&self) -> Option<&str> {
        self.upstream_context
            .as_deref()
            .map(|context| context.model.as_str())
    }

    /// 附加 Responses API 返回的稳定 incomplete reason，供候选诊断使用。
    pub fn with_incomplete_reason(mut self, reason: impl Into<String>) -> Self {
        self.incomplete_reason = Some(reason.into());
        self
    }

    pub fn incomplete_reason(&self) -> Option<&str> {
        self.incomplete_reason.as_deref()
    }

    /// 返回跨 Provider 统一的错误分类。
    pub fn kind(&self) -> LlmErrorKind {
        match self.upstream_status {
            Some(401 | 403) => return LlmErrorKind::Authentication,
            Some(429) => return LlmErrorKind::RateLimit,
            Some(500..=599) => return LlmErrorKind::UpstreamUnavailable,
            Some(400..=499) => return LlmErrorKind::InvalidRequest,
            _ => {}
        }
        match self.code.as_str() {
            "timeout" => LlmErrorKind::Timeout,
            "authentication_failed" | "tavily_auth_error" | "permission_denied" => {
                LlmErrorKind::Authentication
            }
            "rate_limited" | "quota_exhausted" => LlmErrorKind::RateLimit,
            "upstream_unavailable" => LlmErrorKind::UpstreamUnavailable,
            "http_error" | "network_error" => LlmErrorKind::Network,
            "bad_tool_arguments" | "bad_request" | "invalid_request" | "upstream_bad_request" => {
                LlmErrorKind::InvalidRequest
            }
            _ => LlmErrorKind::Other,
        }
    }

    /// 将历史错误码归一为稳定的联网/工具故障分类。
    pub fn error_kind(&self) -> &'static str {
        match self.upstream_status {
            Some(400) => return "upstream_bad_request",
            Some(401) => return "authentication_failed",
            Some(403) => return "permission_denied",
            Some(429) => return "rate_limited",
            Some(500..=599) => return "upstream_unavailable",
            _ => {}
        }
        match self.code.as_str() {
            "bad_tool_arguments" | "bad_request" => "invalid_arguments",
            "config" | "web_search_not_configured" | "web_search_disabled" => {
                "missing_configuration"
            }
            "authentication_failed" | "tavily_auth_error" => "authentication_failed",
            "permission_denied" | "quota_exhausted" => "permission_denied",
            "rate_limited" => "rate_limited",
            "upstream_bad_request" => "upstream_bad_request",
            "upstream_unavailable" => "upstream_unavailable",
            "timeout" => "timeout",
            "http_error" | "network_error" => "network_error",
            "provider_error" if matches!(self.stage.as_str(), "json" | "sse" | "stream") => {
                "invalid_response"
            }
            "invalid_response" | "sse_incomplete_frame" => "invalid_response",
            _ => "internal_error",
        }
    }

    /// 只有瞬时故障允许有限重试；400/401/403、配置和参数错误永不重试。
    pub fn retriable(&self) -> bool {
        match self.upstream_status {
            Some(429 | 502 | 503 | 504) => return true,
            Some(_) => return false,
            None => {}
        }
        matches!(
            self.error_kind(),
            "rate_limited" | "timeout" | "network_error" | "upstream_unavailable"
        )
    }
}

/// 沿错误 source chain 识别结构化超时与传输错误，不依赖错误文本。
pub fn classify_error_source(error: &(dyn StdError + 'static)) -> LlmErrorKind {
    let mut current = Some(error);
    while let Some(source) = current {
        if let Some(error) = source.downcast_ref::<reqwest::Error>() {
            if error.is_timeout() {
                return LlmErrorKind::Timeout;
            }
            if let Some(status) = error.status() {
                return match status.as_u16() {
                    401 | 403 => LlmErrorKind::Authentication,
                    429 => LlmErrorKind::RateLimit,
                    500..=599 => LlmErrorKind::UpstreamUnavailable,
                    400..=499 => LlmErrorKind::InvalidRequest,
                    _ => LlmErrorKind::Other,
                };
            }
            if error.is_connect() || error.is_request() || error.is_body() {
                return LlmErrorKind::Network;
            }
        }
        if let Some(error) = source.downcast_ref::<io::Error>() {
            match error.kind() {
                io::ErrorKind::TimedOut => return LlmErrorKind::Timeout,
                io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::NotConnected
                | io::ErrorKind::AddrInUse
                | io::ErrorKind::AddrNotAvailable
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::UnexpectedEof => return LlmErrorKind::Network,
                _ => {}
            }
        }
        current = source.source();
    }
    LlmErrorKind::Other
}

fn error_chain_message(error: &(dyn StdError + 'static)) -> String {
    let mut messages = Vec::new();
    let mut current = Some(error);
    while let Some(source) = current {
        messages.push(source.to_string());
        current = source.source();
    }
    messages.join(": caused by: ")
}

/// 自动将 LlmError 转换为 ErrorInfo。
impl From<LlmError> for ErrorInfo {
    fn from(value: LlmError) -> Self {
        value.as_info()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[derive(Debug, Error)]
    #[error("middle transport wrapper")]
    struct MiddleError(#[source] reqwest::Error);

    #[derive(Debug, Error)]
    #[error("outer provider wrapper")]
    struct OuterError(#[source] MiddleError);

    async fn structured_reqwest_timeout() -> reqwest::Error {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
        qq_maid_common::http_client::try_builder()
            .unwrap()
            .timeout(Duration::from_millis(20))
            .build()
            .unwrap()
            .get(format!("http://{addr}/slow"))
            .send()
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn structured_http_timeout_maps_to_timeout() {
        let source = structured_reqwest_timeout().await;
        assert!(source.is_timeout());

        let error = LlmError::from_error_source(
            &source,
            LlmErrorKind::Network,
            "http_request_timeout",
            "test request failed",
        );

        assert_eq!(error.kind(), LlmErrorKind::Timeout);
        assert_eq!(error.code, "timeout");
        assert_eq!(error.stage, "http_request_timeout");
    }

    #[tokio::test]
    async fn wrapped_source_chain_still_detects_reqwest_timeout() {
        let wrapped = OuterError(MiddleError(structured_reqwest_timeout().await));

        assert_eq!(classify_error_source(&wrapped), LlmErrorKind::Timeout);
        let error = LlmError::from_error_source(
            &wrapped,
            LlmErrorKind::Other,
            "response_read",
            "wrapped request failed",
        );
        assert_eq!(error.code, "timeout");
        assert!(error.message.contains("middle transport wrapper"));
    }

    #[tokio::test]
    async fn connection_failure_is_network_not_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let source = qq_maid_common::http_client::client()
            .get(format!("http://{addr}/unavailable"))
            .send()
            .await
            .unwrap_err();

        let error = LlmError::from_error_source(
            &source,
            LlmErrorKind::Network,
            "http_request",
            "test request failed",
        );
        assert_eq!(error.kind(), LlmErrorKind::Network);
        assert_eq!(error.code, "network_error");
        assert_ne!(error.kind(), LlmErrorKind::Timeout);
    }

    #[test]
    fn upstream_statuses_have_distinct_categories() {
        let cases = [
            (401, LlmErrorKind::Authentication),
            (429, LlmErrorKind::RateLimit),
            (503, LlmErrorKind::UpstreamUnavailable),
            (400, LlmErrorKind::InvalidRequest),
        ];
        for (status, expected) in cases {
            let error = LlmError::from_upstream_status(status, "upstream failed", "http");
            assert_eq!(error.kind(), expected, "status={status}");
            assert_ne!(error.kind(), LlmErrorKind::Timeout, "status={status}");
        }
    }
}

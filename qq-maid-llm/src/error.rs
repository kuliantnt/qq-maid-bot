//! 应用错误类型。定义 `LlmError` 主错误结构体及其便捷构造方法，
//! 以及序列化友好的 `ErrorInfo` 表示。

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
            "invalid_response" => "invalid_response",
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

/// 自动将 LlmError 转换为 ErrorInfo。
impl From<LlmError> for ErrorInfo {
    fn from(value: LlmError) -> Self {
        value.as_info()
    }
}

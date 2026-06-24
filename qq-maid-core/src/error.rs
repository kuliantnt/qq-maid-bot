//! 应用错误类型。定义 `LlmError` 主错误结构体及其便捷构造方法，
//! 以及序列化友好的 `ErrorInfo` 表示。

use serde::{Deserialize, Serialize};
use thiserror::Error;

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
}

/// 自动将 LlmError 转换为 ErrorInfo。
impl From<LlmError> for ErrorInfo {
    fn from(value: LlmError) -> Self {
        value.as_info()
    }
}

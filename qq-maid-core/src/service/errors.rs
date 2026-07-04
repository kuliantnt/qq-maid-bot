use tracing::{error, warn};

use crate::error::{ErrorInfo, LlmError};
use qq_maid_common::{
    redaction::redact_sensitive_text, text::truncate_chars_with_ellipsis_trimmed,
};

use super::{CoreError, CoreFailureKind, CoreRespondFailure};

impl CoreError {
    pub fn new(
        code: impl Into<String>,
        stage: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            stage: stage.into(),
            message: message.into(),
        }
    }

    pub fn as_info(&self) -> ErrorInfo {
        ErrorInfo {
            code: self.code.clone(),
            message: self.message.clone(),
            stage: self.stage.clone(),
        }
    }
}

impl From<LlmError> for CoreError {
    fn from(value: LlmError) -> Self {
        Self {
            code: value.code,
            stage: value.stage,
            message: value.message,
        }
    }
}

impl From<ErrorInfo> for CoreError {
    fn from(value: ErrorInfo) -> Self {
        Self {
            code: value.code,
            stage: value.stage,
            message: value.message,
        }
    }
}

impl CoreRespondFailure {
    pub(super) fn cancelled() -> Self {
        Self {
            kind: CoreFailureKind::Cancelled,
            message: "请求已取消".to_owned(),
            retryable: true,
        }
    }

    pub(super) fn from_llm_error(error: &LlmError) -> Self {
        let core_error = CoreError::from(error.clone());
        Self::from_core_error(&core_error)
    }

    pub(super) fn from_core_error(error: &CoreError) -> Self {
        let kind = match (error.code.as_str(), error.stage.as_str()) {
            ("timeout", "query" | "search" | "web_search") => CoreFailureKind::SearchTimeout,
            ("timeout", _) => CoreFailureKind::LlmTimeout,
            (_, "query" | "search" | "web_search") => CoreFailureKind::SearchFailed,
            ("provider_error" | "http_error" | "upstream_unavailable" | "rate_limited", _) => {
                CoreFailureKind::LlmFailed
            }
            _ => CoreFailureKind::Internal,
        };
        Self {
            kind,
            message: user_visible_failure_message(kind),
            retryable: matches!(
                kind,
                CoreFailureKind::SearchTimeout
                    | CoreFailureKind::SearchFailed
                    | CoreFailureKind::LlmTimeout
                    | CoreFailureKind::LlmFailed
                    | CoreFailureKind::Cancelled
            ),
        }
    }
}

fn user_visible_failure_message(kind: CoreFailureKind) -> String {
    match kind {
        CoreFailureKind::SearchTimeout => "联网查询超时了，请稍后再试。",
        CoreFailureKind::SearchFailed => "联网查询暂时不可用，请稍后再试。",
        CoreFailureKind::LlmTimeout => "LLM 服务处理超时，请稍后再试。",
        CoreFailureKind::LlmFailed => "上游服务暂时不可用，请稍后再试。",
        CoreFailureKind::Cancelled => "请求已取消。",
        CoreFailureKind::Internal => "处理失败，请稍后再试。",
    }
    .to_owned()
}

pub(crate) fn warn_core_error(scope_key: &str, err: &LlmError) {
    warn!(
        scope_key,
        error_code = err.code,
        error_stage = err.stage,
        error_message = %safe_error_message(err),
        "core respond request failed"
    );
}

pub(crate) fn error_core_error(scope_key: &str, err: &LlmError) {
    error!(
        scope_key,
        error_code = err.code,
        error_stage = err.stage,
        error_message = %safe_error_message(err),
        "core respond request timed out"
    );
}

pub(crate) fn safe_error_message(err: &LlmError) -> String {
    // 只把脱敏后的短错误摘要写入日志，避免 HTTP 上游正文携带 token、URL query 或过长 payload。
    truncate_chars_with_ellipsis_trimmed(&redact_sensitive_text(&err.message), 500)
}

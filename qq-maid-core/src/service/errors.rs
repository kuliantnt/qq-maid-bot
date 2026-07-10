use tracing::{error, warn};

use crate::error::{ErrorInfo, LlmError};
use qq_maid_common::{
    redaction::redact_sensitive_text, text::truncate_chars_with_ellipsis_trimmed,
};
use qq_maid_llm::agent_loop::AgentStopReason;

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
    pub(super) fn cancelled(run_handle: Option<&qq_maid_llm::agent_loop::AgentRunHandle>) -> Self {
        Self {
            kind: CoreFailureKind::Cancelled,
            message: "请求已取消".to_owned(),
            retryable: true,
            agent: run_handle.map(|handle| handle.snapshot()),
        }
    }

    pub(super) fn from_llm_error(error: &LlmError) -> Self {
        let core_error = CoreError::from(error.clone());
        let mut failure = Self::from_core_error(&core_error);
        failure.agent = error.agent.as_deref().cloned();
        if let Some(stop_reason) = error
            .agent
            .as_ref()
            .and_then(|diagnostics| diagnostics.stop_reason)
        {
            match stop_reason {
                AgentStopReason::Timeout => {
                    failure.kind = CoreFailureKind::LlmTimeout;
                    failure.message = user_visible_failure_message(failure.kind);
                    failure.retryable = true;
                }
                AgentStopReason::Cancelled => {
                    failure.kind = CoreFailureKind::Cancelled;
                    failure.message = user_visible_failure_message(failure.kind);
                    failure.retryable = true;
                }
                AgentStopReason::DirectAnswer
                | AgentStopReason::ToolUsed
                | AgentStopReason::Clarify
                | AgentStopReason::Rejected
                | AgentStopReason::Failed
                | AgentStopReason::MaxRounds => {}
            }
        }
        failure
    }

    pub(super) fn from_core_error(error: &CoreError) -> Self {
        if error.code == "unsupported_input_part" {
            return Self {
                kind: CoreFailureKind::Internal,
                message: safe_user_visible_input_error(&error.message)
                    .unwrap_or_else(|| user_visible_failure_message(CoreFailureKind::Internal)),
                retryable: false,
                agent: None,
            };
        }
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
            agent: None,
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

fn safe_user_visible_input_error(message: &str) -> Option<String> {
    let compact = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }
    let lower = compact.to_ascii_lowercase();
    if compact.contains('\\')
        || compact.contains("sk-")
        || [
            "authorization",
            "bearer ",
            "access_token",
            "refresh_token",
            "token=",
            "secret=",
            "openid",
            "http://",
            "https://",
            "/home/",
            ".env",
            "-----begin",
        ]
        .iter()
        .any(|fragment| lower.contains(fragment))
    {
        return None;
    }
    Some(truncate_chars_with_ellipsis_trimmed(
        &redact_sensitive_text(compact),
        120,
    ))
}

pub(crate) fn warn_core_error(scope_key: &str, err: &LlmError) {
    let agent = err.agent.as_ref();
    warn!(
        scope_key,
        error_code = err.code,
        error_stage = err.stage,
        error_message = %safe_error_message(err),
        agent_stop_reason = agent
            .and_then(|diagnostics| diagnostics.stop_reason)
            .map(AgentStopReason::as_str)
            .unwrap_or("none"),
        agent_model_rounds = agent.map(|diagnostics| diagnostics.model_rounds),
        agent_tool_execution_attempted = agent
            .map(|diagnostics| diagnostics.tool_execution_attempted),
        agent_emitted_tools = ?agent.map(|diagnostics| &diagnostics.emitted_tools),
        agent_executed_tools = ?agent.map(|diagnostics| &diagnostics.executed_tools),
        agent_streaming_fallback_used = agent
            .map(|diagnostics| diagnostics.streaming_fallback_used),
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

#[cfg(test)]
mod tests {
    use super::*;
    use qq_maid_llm::agent_loop::AgentRunDiagnostics;

    fn agent_error(code: &str, stage: &str, reason: AgentStopReason) -> LlmError {
        LlmError::new(code, "safe summary", stage).with_agent(AgentRunDiagnostics {
            stop_reason: Some(reason),
            ..Default::default()
        })
    }

    #[test]
    fn agent_provider_failures_keep_original_classification() {
        for code in ["provider_error", "rate_limited", "upstream_unavailable"] {
            let failure = CoreRespondFailure::from_llm_error(&agent_error(
                code,
                "provider",
                AgentStopReason::Failed,
            ));
            assert_eq!(failure.kind, CoreFailureKind::LlmFailed, "code={code}");
            assert!(failure.retryable, "code={code}");
            assert!(failure.agent.is_some(), "code={code}");
        }
    }

    #[test]
    fn terminal_agent_reasons_only_override_timeout_and_cancelled() {
        let timeout = CoreRespondFailure::from_llm_error(&agent_error(
            "provider_error",
            "provider",
            AgentStopReason::Timeout,
        ));
        assert_eq!(timeout.kind, CoreFailureKind::LlmTimeout);
        assert!(timeout.retryable);

        let cancelled = CoreRespondFailure::from_llm_error(&agent_error(
            "provider_error",
            "provider",
            AgentStopReason::Cancelled,
        ));
        assert_eq!(cancelled.kind, CoreFailureKind::Cancelled);
        assert!(cancelled.retryable);

        let max_rounds = CoreRespondFailure::from_llm_error(&agent_error(
            "tool_loop_limit",
            "tool_loop",
            AgentStopReason::MaxRounds,
        ));
        assert_eq!(max_rounds.kind, CoreFailureKind::Internal);
        assert!(!max_rounds.retryable);
    }

    #[test]
    fn non_agent_cancellation_has_no_agent_diagnostics() {
        let failure = CoreRespondFailure::cancelled(None);
        assert_eq!(failure.kind, CoreFailureKind::Cancelled);
        assert!(failure.agent.is_none());
    }

    #[test]
    fn unsupported_input_part_failure_keeps_safe_user_message() {
        let failure = CoreRespondFailure::from_core_error(&CoreError::new(
            "unsupported_input_part",
            "request",
            "我收到图片了，但当前入口没有提供可读取图片内容。你可以补充文字说明，我先帮你记录。",
        ));

        assert_eq!(failure.kind, CoreFailureKind::Internal);
        assert!(!failure.retryable);
        assert!(failure.message.contains("当前入口没有提供可读取图片内容"));
    }

    #[test]
    fn unsupported_input_part_failure_hides_unsafe_message() {
        let failure = CoreRespondFailure::from_core_error(&CoreError::new(
            "unsupported_input_part",
            "request",
            "file://C:\\Users\\ThinkPad\\Pictures\\a.jpg",
        ));

        assert_eq!(failure.message, "处理失败，请稍后再试。");
    }
}

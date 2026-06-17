//! OpenAI 聊天链路的 fallback 判定策略。
//!
//! 这些策略需要同时服务 Responses 主链路和 rig-core Chat Completions fallback，
//! 因此单独拆出来，避免各条链路复制同一套“是否值得再试一次”的规则。

use crate::{error::LlmError, provider::ChatOutcome};

/// 当流式链路表面成功但正文为空时，是否应该补一次非流式请求。
pub(crate) fn should_retry_non_stream_after_empty_stream(outcome: &ChatOutcome) -> bool {
    outcome.reply.trim().is_empty()
}

/// 当流式链路直接失败时，是否应该补一次同 provider 的非流式请求。
pub(crate) fn should_retry_non_stream_after_stream_error(err: &LlmError) -> bool {
    matches!(
        err.code.as_str(),
        "provider_error" | "http_error" | "timeout"
    ) && matches!(err.stage.as_str(), "provider" | "stream" | "http")
}

/// 当 Responses 主链路失败时，是否允许降级到 rig-core Chat Completions。
pub(crate) fn should_fallback_to_rig_after_responses_error(err: &LlmError) -> bool {
    matches!(
        err.code.as_str(),
        "provider_error" | "http_error" | "timeout"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::metrics::LlmMetrics;

    fn mock_outcome(reply: &str) -> ChatOutcome {
        ChatOutcome {
            reply: reply.to_owned(),
            metrics: LlmMetrics {
                provider: "mock".to_owned(),
                model: "mock".to_owned(),
                stream: true,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: None,
        }
    }

    #[test]
    fn empty_stream_reply_triggers_non_stream_retry() {
        assert!(should_retry_non_stream_after_empty_stream(&mock_outcome(
            " \n\t "
        )));
    }

    #[test]
    fn non_empty_stream_reply_keeps_stream_result() {
        assert!(!should_retry_non_stream_after_empty_stream(&mock_outcome(
            "你好"
        )));
    }

    #[test]
    fn stream_provider_error_triggers_non_stream_retry() {
        assert!(should_retry_non_stream_after_stream_error(
            &LlmError::provider("upstream 502", "stream")
        ));
        assert!(should_retry_non_stream_after_stream_error(
            &LlmError::provider("bad status", "provider")
        ));
    }

    #[test]
    fn config_error_does_not_trigger_non_stream_retry() {
        assert!(!should_retry_non_stream_after_stream_error(
            &LlmError::config("missing api key")
        ));
    }

    #[test]
    fn responses_errors_trigger_rig_fallback_only_for_upstream_failures() {
        assert!(should_fallback_to_rig_after_responses_error(
            &LlmError::http("OpenAI chat returned HTTP 400")
        ));
        assert!(should_fallback_to_rig_after_responses_error(
            &LlmError::provider("invalid OpenAI chat stream JSON", "sse")
        ));
        assert!(!should_fallback_to_rig_after_responses_error(
            &LlmError::new(
                "bad_request",
                "messages must contain non-empty content",
                "request"
            )
        ));
        assert!(!should_fallback_to_rig_after_responses_error(
            &LlmError::config("OPENAI_API_KEY is required")
        ));
    }
}

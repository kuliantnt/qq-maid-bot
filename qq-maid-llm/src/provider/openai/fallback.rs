//! OpenAI 聊天链路的 fallback 判定策略。
//!
//! 这些策略需要同时服务 Responses 主链路和 Chat Completions fallback，
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
        "provider_error" | "http_error" | "network_error" | "timeout"
    ) && matches!(
        err.stage.as_str(),
        "provider"
            | "stream"
            | "http"
            | "http_request"
            | "http_request_timeout"
            | "response_read"
            | "stream_read"
            | "stream_read_after_delta"
            | "sse"
            | "stream_after_delta"
    )
}

/// 当 Responses 主链路失败时，是否允许降级到 Chat Completions。
/// 认证/授权拒绝对同一 Provider 的另一端点同样无效，应直接交给跨 Provider 候选链。
pub(crate) fn should_fallback_to_chat_after_responses_error(err: &LlmError) -> bool {
    if let Some(status) = err.upstream_status {
        // 只有真实上游返回的端点/协议兼容状态才允许切换端点；本地 bad_request
        // 没有 upstream_status，认证、限流和服务端错误也不应重复请求同一 Provider。
        return matches!(status, 400 | 404 | 422);
    }
    matches!(
        err.code.as_str(),
        "provider_error" | "http_error" | "network_error" | "timeout"
    ) && !matches!(
        err.stage.as_str(),
        "stream_after_delta" | "stream_read_after_delta" | "provider_unavailable"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::LlmMetrics;

    fn mock_outcome(reply: &str) -> ChatOutcome {
        ChatOutcome {
            reply: reply.to_owned(),
            output_parts: Vec::new(),
            metrics: LlmMetrics {
                provider: "mock".to_owned(),
                model: "mock".to_owned(),
                stream: true,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: None,
            fallback_used: false,
            agent: Default::default(),
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
        assert!(!should_retry_non_stream_after_stream_error(
            &LlmError::provider("upstream rejected key", "provider_unavailable")
        ));
    }

    #[test]
    fn responses_incompatible_upstream_statuses_trigger_chat_fallback() {
        for status in [400, 404, 422] {
            let error =
                LlmError::from_upstream_status(status, "incompatible Responses API", "http");
            assert!(
                should_fallback_to_chat_after_responses_error(&error),
                "status={status}"
            );
        }
    }

    #[test]
    fn local_bad_request_does_not_trigger_chat_fallback() {
        assert!(!should_fallback_to_chat_after_responses_error(
            &LlmError::new(
                "bad_request",
                "messages must contain non-empty content",
                "request"
            )
        ));
    }

    #[test]
    fn authentication_statuses_do_not_trigger_chat_fallback() {
        for status in [401, 403] {
            let error = LlmError::from_upstream_status(status, "authentication rejected", "http");
            assert!(
                !should_fallback_to_chat_after_responses_error(&error),
                "status={status}"
            );
        }
    }

    #[test]
    fn responses_protocol_errors_without_http_status_can_trigger_chat_fallback() {
        assert!(should_fallback_to_chat_after_responses_error(
            &LlmError::http("OpenAI Responses transport failed")
        ));
        assert!(should_fallback_to_chat_after_responses_error(
            &LlmError::provider("invalid OpenAI chat stream JSON", "sse")
        ));
        assert!(!should_fallback_to_chat_after_responses_error(
            &LlmError::config("OPENAI_API_KEY is required")
        ));
        assert!(!should_fallback_to_chat_after_responses_error(
            &LlmError::provider("upstream rejected key", "provider_unavailable")
        ));
    }
}

//! Responses 流式诊断状态与 fallback reason 分类。

use std::sync::{Arc, Mutex};

use crate::{agent_loop::AgentStreamingDiagnostics, error::LlmError};

pub(super) fn update_streaming_diagnostics(
    diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>,
    update: impl FnOnce(&mut AgentStreamingDiagnostics),
) {
    let mut diagnostics = diagnostics
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    update(&mut diagnostics);
}

pub(super) fn replace_streaming_diagnostics(
    diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>,
    replacement: AgentStreamingDiagnostics,
) {
    update_streaming_diagnostics(diagnostics, |item| *item = replacement);
}

pub(super) fn set_streaming_fallback_reason(
    diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>,
    fallback_reason: &str,
) {
    update_streaming_diagnostics(diagnostics, |item| {
        if item.fallback_reason.is_none() {
            item.fallback_reason = Some(fallback_reason.to_owned());
        }
    });
}

pub(super) fn sync_responses_stream_diagnostics(
    diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>,
    saw_completed: bool,
    buffered_delta_count: usize,
    active_function_call_count: usize,
) {
    update_streaming_diagnostics(diagnostics, |item| {
        item.saw_completed = saw_completed;
        item.buffered_delta_count = buffered_delta_count;
        item.active_function_call_count = active_function_call_count;
    });
}

pub(super) fn classify_responses_stream_failure(
    diagnostics: &Arc<Mutex<AgentStreamingDiagnostics>>,
    err: &LlmError,
) {
    update_streaming_diagnostics(diagnostics, |item| {
        if item.fallback_reason.is_some() {
            return;
        }
        let reason = if item.saw_completed {
            "completed_response_incomplete"
        } else if item.saw_done {
            "done_without_safe_completion"
        } else if err.message.contains("before response.completed") {
            if item.sse_event_count == 0 {
                "sse_early_eof"
            } else {
                "missing_response_completed"
            }
        } else if err.code == "http_error" || err.stage == "http" {
            "http_sse_parse_error"
        } else {
            "provider_error_other"
        };
        item.fallback_reason = Some(reason.to_owned());
    });
}

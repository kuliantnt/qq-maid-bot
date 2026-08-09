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
    buffered_text_chars: usize,
    active_function_call_count: usize,
) {
    update_streaming_diagnostics(diagnostics, |item| {
        item.saw_completed = saw_completed;
        item.buffered_delta_count = buffered_delta_count;
        item.buffered_text_chars = buffered_text_chars;
        item.saw_text_delta |= buffered_text_chars > 0 || item.visible_text_chars > 0;
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
        let reason = if item.explicit_failure_event {
            "explicit_failure_event"
        } else if item.parse_error {
            "sse_parse_error"
        } else if item.connection_reset {
            "stream_connection_reset"
        } else if item.saw_completed {
            "completed_response_incomplete"
        } else if item.saw_done {
            "done_without_safe_completion"
        } else if item.normal_eof && item.active_function_call_count > 0 {
            "normal_eof_active_function_call"
        } else if item.stream_end_kind.as_deref()
            == Some("normal_eof_completed_function_call_without_terminal_event")
        {
            "normal_eof_completed_function_call_without_terminal_event"
        } else if item.normal_eof && item.saw_text_delta {
            "normal_eof_text_not_committed"
        } else if item.normal_eof {
            "normal_eof_no_content"
        } else if err.code == "http_error" || err.stage == "http" {
            "http_stream_error"
        } else {
            "provider_error_other"
        };
        item.fallback_reason = Some(reason.to_owned());
    });
}

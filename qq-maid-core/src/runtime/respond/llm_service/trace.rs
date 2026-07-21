//! LLM 聊天输入输出的可选脱敏 TRACE 日志。

use std::env;

use qq_maid_llm::provider::types::{ChatMessage, ChatRole};

use crate::runtime::session::redact_sensitive_text;

use super::super::{RespondPurpose, RespondRequest, common::truncate_chars};

pub(super) const CHAT_TRACE_TEXT_LIMIT: usize = 600;

pub(super) fn trace_chat_messages(req: &RespondRequest, messages: &[ChatMessage]) {
    if !tracing::enabled!(tracing::Level::TRACE) {
        return;
    }
    let session_id = trace_session_id(req);
    let roles = messages
        .iter()
        .map(|message| chat_role_name(&message.role))
        .collect::<Vec<_>>()
        .join(",");
    tracing::trace!(
        purpose = %respond_purpose_name(&req.purpose),
        session_id = %session_id,
        scope_key = %trace_scope_key(req),
        message_count = messages.len(),
        roles = %roles,
        model_override = %req.model.as_deref().unwrap_or("-"),
        user_text_chars = req.user_text.trim().chars().count(),
        "llm chat request summary"
    );
    if !trace_chat_input_enabled() {
        return;
    }
    let payload = messages
        .iter()
        .enumerate()
        .map(|(index, message)| format_chat_message_trace(index, message))
        .collect::<Vec<_>>()
        .join("\n");
    tracing::trace!(
        purpose = %respond_purpose_name(&req.purpose),
        session_id = %session_id,
        scope_key = %trace_scope_key(req),
        messages = %payload,
        "llm chat request messages"
    );
}

pub(super) fn trace_chat_raw_reply(req: &RespondRequest, raw_reply: &str) {
    if !tracing::enabled!(tracing::Level::TRACE) || !trace_chat_output_enabled() {
        return;
    }
    tracing::trace!(
        purpose = %respond_purpose_name(&req.purpose),
        session_id = %trace_session_id(req),
        scope_key = %trace_scope_key(req),
        raw_reply_chars = raw_reply.chars().count(),
        raw_reply = %trace_text(raw_reply),
        "llm chat raw reply"
    );
}

pub(super) fn trace_chat_final_reply(req: &RespondRequest, final_reply: &str) {
    if !tracing::enabled!(tracing::Level::TRACE) || !trace_chat_output_enabled() {
        return;
    }
    tracing::trace!(
        purpose = %respond_purpose_name(&req.purpose),
        session_id = %trace_session_id(req),
        scope_key = %trace_scope_key(req),
        final_reply_chars = final_reply.chars().count(),
        final_reply = %trace_text(final_reply),
        "llm chat final reply"
    );
}

fn trace_chat_input_enabled() -> bool {
    trace_chat_flag("LLM_TRACE_CHAT_INPUT")
}

fn trace_chat_output_enabled() -> bool {
    trace_chat_flag("LLM_TRACE_CHAT_OUTPUT")
}

fn trace_chat_flag(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes" | "enabled"
            )
        })
        .unwrap_or(false)
}

fn format_chat_message_trace(index: usize, message: &ChatMessage) -> String {
    format!(
        "#{index} [{}] {}",
        chat_role_name(&message.role),
        trace_text(&message.content)
    )
}

fn chat_role_name(role: &ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
    }
}

pub(super) fn respond_purpose_name(purpose: &RespondPurpose) -> &'static str {
    match purpose {
        RespondPurpose::Chat => "chat",
        RespondPurpose::MemoryDraft => "memory_draft",
        RespondPurpose::TodoParse => "todo_parse",
        RespondPurpose::Compact => "compact",
    }
}

fn trace_session_id(req: &RespondRequest) -> &str {
    let session_id = req.session_id.trim();
    if session_id.is_empty() {
        "-"
    } else {
        session_id
    }
}

fn trace_scope_key(req: &RespondRequest) -> &str {
    let scope_key = req.scope_key.trim();
    if scope_key.is_empty() { "-" } else { scope_key }
}

pub(super) fn trace_text(text: &str) -> String {
    truncate_chars(&redact_sensitive_text(text), CHAT_TRACE_TEXT_LIMIT)
}

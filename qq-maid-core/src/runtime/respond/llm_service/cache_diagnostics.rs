//! Prompt Cache 脱敏指纹诊断。

use qq_maid_common::input_part::MessageInputPart;
use qq_maid_llm::{provider::types::ChatMessage, tool::ToolRegistry};
use sha2::{Digest, Sha256};

use crate::error::LlmError;

use super::{super::RespondRequest, has_request_time_context, normalize_user_message_for_provider};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SegmentFingerprint {
    pub(super) chars: usize,
    pub(super) hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PromptCacheDiagnostics {
    pub(super) agent_scene: String,
    pub(super) stable_system: SegmentFingerprint,
    pub(super) summary: SegmentFingerprint,
    pub(super) history: SegmentFingerprint,
    pub(super) time: SegmentFingerprint,
    pub(super) knowledge: SegmentFingerprint,
    pub(super) memory: SegmentFingerprint,
    pub(super) session: SegmentFingerprint,
    pub(super) tool_schema_hash: String,
    pub(super) current_message_chars: usize,
    pub(super) history_compacted: bool,
    pub(super) summary_revision: u64,
}

pub(super) fn prompt_cache_diagnostics(
    req: &RespondRequest,
    messages: &[ChatMessage],
    tools: Option<&ToolRegistry>,
) -> Result<PromptCacheDiagnostics, LlmError> {
    let stable_system_json = serde_json::to_string(&req.system_prompts).map_err(|err| {
        LlmError::new(
            "cache_diagnostics_failed",
            format!("failed to serialize stable system prompts: {err}"),
            "diagnostics",
        )
    })?;
    let normalized_history = req
        .history_messages
        .iter()
        .filter(|message| !message.content.trim().is_empty())
        .cloned()
        .map(normalize_user_message_for_provider)
        .collect::<Vec<_>>();
    let history_json = serde_json::to_string(&normalized_history).map_err(|err| {
        LlmError::new(
            "cache_diagnostics_failed",
            format!("failed to serialize history messages: {err}"),
            "diagnostics",
        )
    })?;
    let time_context = messages
        .iter()
        .find(|message| has_request_time_context(std::slice::from_ref(*message)))
        .map(|message| message.content.as_str())
        .unwrap_or("");
    let mut tool_schema = tools
        .map(ToolRegistry::stable_schema_json)
        .transpose()?
        .unwrap_or_else(|| "[]".to_owned());
    if req
        .metadata
        .get("image_generation")
        .is_some_and(|value| value == "true")
    {
        tool_schema.push_str("|native:image_generation");
    }

    Ok(PromptCacheDiagnostics {
        agent_scene: req
            .metadata
            .get("agent_scene")
            .cloned()
            .unwrap_or_else(|| "-".to_owned()),
        stable_system: fingerprint_with_chars(
            req.system_prompts
                .iter()
                .map(|item| item.chars().count())
                .sum(),
            &stable_system_json,
        ),
        summary: fingerprint(req.history_summary.as_str()),
        history: fingerprint_with_chars(
            normalized_history.iter().map(chat_message_chars).sum(),
            &history_json,
        ),
        time: fingerprint(time_context),
        knowledge: fingerprint(req.knowledge_context.as_str()),
        memory: fingerprint(req.memory_context.as_str()),
        session: fingerprint(req.session_context.as_str()),
        tool_schema_hash: stable_hash(&tool_schema),
        current_message_chars: messages.last().map(chat_message_chars).unwrap_or_default(),
        history_compacted: req
            .metadata
            .get("history_compacted")
            .is_some_and(|value| value == "true"),
        summary_revision: req
            .metadata
            .get("summary_revision")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    })
}

fn chat_message_chars(message: &ChatMessage) -> usize {
    message.content.chars().count()
        + message
            .content_parts
            .iter()
            .map(|part| match part {
                MessageInputPart::Text { text, .. } => text.chars().count(),
                _ => 0,
            })
            .sum::<usize>()
}

fn fingerprint(value: &str) -> SegmentFingerprint {
    fingerprint_with_chars(value.chars().count(), value)
}

fn fingerprint_with_chars(chars: usize, value: &str) -> SegmentFingerprint {
    SegmentFingerprint {
        chars,
        hash: stable_hash(value),
    }
}

fn stable_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

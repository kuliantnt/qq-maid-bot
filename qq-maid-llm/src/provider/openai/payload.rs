//! OpenAI Responses 请求体构造。
//!
//! Responses API 对 assistant 历史的格式要求与 Chat Completions 不同：回放历史时必须
//! 使用 `output_text` / `refusal`，不能继续复用用户输入的 `input_text`。这里集中维护
//! 该映射，避免不同调用点各自拼 JSON 时再把 assistant 历史序列化错。

use serde_json::{Value, json};

use crate::{
    error::LlmError,
    provider::types::{ChatMessage, ChatRole, ReasoningEffort},
};

/// 构造 OpenAI Responses API 请求体。
pub(crate) fn openai_responses_payload(
    messages: &[ChatMessage],
    model: &str,
    max_output_tokens: u64,
    reasoning_effort: Option<ReasoningEffort>,
    stream: bool,
) -> Result<Value, LlmError> {
    let mut payload = json!({
        "model": model,
        "input": openai_responses_input(messages)?,
        "max_output_tokens": max_output_tokens,
    });
    if let Some(effort) = reasoning_effort.filter(|_| openai_model_supports_reasoning(model)) {
        payload["reasoning"] = json!({ "effort": effort.as_str() });
    }
    if stream {
        payload["stream"] = json!(true);
    }
    Ok(payload)
}

/// 将内部聊天消息转换为 Responses input items。
fn openai_responses_input(messages: &[ChatMessage]) -> Result<Vec<Value>, LlmError> {
    let input = messages
        .iter()
        .filter(|message| !message.content.trim().is_empty())
        .map(openai_responses_message)
        .collect::<Vec<_>>();

    if input.is_empty() {
        return Err(LlmError::new(
            "bad_request",
            "messages must contain non-empty content",
            "request",
        ));
    }
    Ok(input)
}

/// 将单条聊天消息映射成 OpenAI Responses message item。
pub(crate) fn openai_responses_message(message: &ChatMessage) -> Value {
    match message.role {
        ChatRole::System => json!({
            "type": "message",
            "role": "system",
            "content": [{"type": "input_text", "text": message.content.as_str()}],
        }),
        ChatRole::User => json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": message.content.as_str()}],
        }),
        ChatRole::Assistant => json!({
            "type": "message",
            "role": "assistant",
            "status": "completed",
            // Responses API 回放 assistant 历史时必须使用 output_text/refusal；
            // input_text 只用于用户/系统输入，兼容网关会按角色严格校验。
            "content": [{"type": "output_text", "text": message.content.as_str()}],
        }),
    }
}

/// OpenAI 的 `reasoning` 参数只对 reasoning 模型族有效。
///
/// 这里在 provider 边界显式忽略不支持模型的配置，避免配置了通用
/// `reasoning_effort` 后让普通 GPT 模型请求被 Responses API 拒绝。
pub(crate) fn openai_model_supports_reasoning(model: &str) -> bool {
    let model = model.trim().strip_prefix("openai:").unwrap_or(model.trim());
    model.starts_with("gpt-5")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::ChatMessage;

    #[test]
    fn openai_responses_payload_replays_assistant_history_as_output_text() {
        let messages = vec![
            ChatMessage::system("system"),
            ChatMessage::user("hi"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "old reply".to_owned(),
            },
            ChatMessage::user("again"),
        ];

        let payload = openai_responses_payload(
            &messages,
            "gpt-5.5",
            1200,
            Some(ReasoningEffort::Medium),
            true,
        )
        .unwrap();
        let input = payload["input"].as_array().unwrap();

        assert_eq!(payload["model"], "gpt-5.5");
        assert_eq!(payload["reasoning"]["effort"], "medium");
        assert_eq!(payload["stream"], true);
        assert_eq!(input.len(), 4);
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["content"][0]["type"], "input_text");
        assert_eq!(input[2]["role"], "assistant");
        assert_eq!(input[2]["status"], "completed");
        assert_eq!(input[2]["content"][0]["type"], "output_text");
        assert_eq!(input[2]["content"][0]["text"], "old reply");
        assert_eq!(input[3]["role"], "user");
        assert_eq!(input[3]["content"][0]["type"], "input_text");
    }

    #[test]
    fn openai_responses_payload_omits_reasoning_for_non_reasoning_models() {
        let payload = openai_responses_payload(
            &[ChatMessage::user("hi")],
            "gpt-4.1",
            1200,
            Some(ReasoningEffort::Medium),
            false,
        )
        .unwrap();

        assert!(payload.get("reasoning").is_none());
    }

    #[test]
    fn openai_reasoning_support_matches_reasoning_model_families() {
        assert!(openai_model_supports_reasoning("gpt-5.5"));
        assert!(openai_model_supports_reasoning("openai:o4-mini"));
        assert!(!openai_model_supports_reasoning("gpt-4.1"));
        assert!(!openai_model_supports_reasoning("gpt-4o"));
    }

    #[test]
    fn openai_responses_payload_rejects_empty_messages() {
        let err = openai_responses_payload(&[], "gpt-5.5", 1200, None, false).unwrap_err();
        assert_eq!(err.code, "bad_request");

        let err =
            openai_responses_payload(&[ChatMessage::user(" \n\t ")], "gpt-5.5", 1200, None, false)
                .unwrap_err();
        assert_eq!(err.code, "bad_request");
    }
}

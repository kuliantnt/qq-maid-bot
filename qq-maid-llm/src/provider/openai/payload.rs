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
use qq_maid_common::input_part::{MediaStatus, MessageInputPart};

use super::chat::{file_unsupported_error, image_reference_error, image_reference_for_openai};

/// 构造 OpenAI Responses API 请求体。
pub(crate) fn openai_responses_payload(
    messages: &[ChatMessage],
    model: &str,
    media_max_bytes: u64,
    max_output_tokens: u64,
    reasoning_effort: Option<ReasoningEffort>,
    stream: bool,
    image_generation_enabled: bool,
) -> Result<Value, LlmError> {
    let mut payload = json!({
        "model": model,
        "input": openai_responses_input(messages, media_max_bytes)?,
        "max_output_tokens": max_output_tokens,
    });
    if let Some(effort) = reasoning_effort.filter(|_| openai_model_supports_reasoning(model)) {
        payload["reasoning"] = json!({ "effort": effort.as_str() });
    }
    if stream {
        payload["stream"] = json!(true);
    }
    if image_generation_enabled {
        // 只有服务端场景策略开启时才向 Provider 暴露原生图片工具。
        payload["tools"] = json!([{ "type": "image_generation" }]);
    }
    Ok(payload)
}

/// 将内部聊天消息转换为 Responses input items。
fn openai_responses_input(
    messages: &[ChatMessage],
    media_max_bytes: u64,
) -> Result<Vec<Value>, LlmError> {
    let input = messages
        .iter()
        .filter(|message| message_has_payload(message))
        .map(|message| openai_responses_message(message, media_max_bytes))
        .collect::<Result<Vec<_>, _>>()?;

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
pub(crate) fn openai_responses_message(
    message: &ChatMessage,
    media_max_bytes: u64,
) -> Result<Value, LlmError> {
    match message.role {
        ChatRole::System => Ok(json!({
            "type": "message",
            "role": "system",
            "content": [{"type": "input_text", "text": message.content.as_str()}],
        })),
        ChatRole::User => Ok(json!({
            "type": "message",
            "role": "user",
            "content": openai_responses_user_content(message, media_max_bytes)?,
        })),
        ChatRole::Assistant => Ok(json!({
            "type": "message",
            "role": "assistant",
            "status": "completed",
            // Responses API 回放 assistant 历史时必须使用 output_text/refusal；
            // input_text 只用于用户/系统输入，兼容网关会按角色严格校验。
            "content": [{"type": "output_text", "text": message.content.as_str()}],
        })),
    }
}

fn message_has_payload(message: &ChatMessage) -> bool {
    !message.content.trim().is_empty() || !message.content_parts.is_empty()
}

fn openai_responses_user_content(
    message: &ChatMessage,
    media_max_bytes: u64,
) -> Result<Vec<Value>, LlmError> {
    if message.content_parts.is_empty() {
        return Ok(vec![
            json!({"type": "input_text", "text": message.content.as_str()}),
        ]);
    }
    let mut content = Vec::new();
    for part in message.effective_content_parts() {
        match part {
            MessageInputPart::Text { text, .. } => {
                if !text.trim().is_empty() {
                    content.push(json!({"type": "input_text", "text": text}));
                }
            }
            MessageInputPart::Image { media } => {
                ensure_media_available(media.status, "图片")?;
                let url = image_reference_for_openai(&media, media_max_bytes)?;
                content.push(json!({
                    "type": "input_image",
                    "image_url": url,
                }));
            }
            MessageInputPart::File { .. } | MessageInputPart::Unknown { .. } => {
                return Err(file_unsupported_error());
            }
        }
    }
    if content.is_empty() {
        return Err(LlmError::new(
            "bad_request",
            "messages must contain non-empty content",
            "request",
        ));
    }
    Ok(content)
}

fn ensure_media_available(status: MediaStatus, label: &str) -> Result<(), LlmError> {
    match status {
        MediaStatus::Available => Ok(()),
        MediaStatus::MissingReadableUrl => Err(image_reference_error()),
        MediaStatus::SizeExceeded => Err(LlmError::new(
            "unsupported_input_part",
            format!("{label}太大了，暂时无法处理。"),
            "request",
        )),
        MediaStatus::UnsupportedType => Err(LlmError::new(
            "unsupported_input_part",
            format!("我收到这个{label}了，但目前还不能读取这种类型。"),
            "request",
        )),
        MediaStatus::DownloadFailed => Err(LlmError::new(
            "unsupported_input_part",
            format!("{label}已收到，但下载失败，请重新发送一次。"),
            "request",
        )),
        MediaStatus::Expired => Err(LlmError::new(
            "unsupported_input_part",
            format!("{label}已收到，但访问地址已过期，请重新发送一次。"),
            "request",
        )),
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
    use qq_maid_common::input_part::{MessageInputPart, MessageMedia, TextSource};

    fn common_prefix_len(left: &[u8], right: &[u8]) -> usize {
        left.iter()
            .zip(right)
            .take_while(|(left, right)| left == right)
            .count()
    }

    #[test]
    fn responses_payload_keeps_byte_prefix_stable_between_compactions() {
        let stable = vec![
            ChatMessage::system("固定 system"),
            ChatMessage::system("摘要 revision 3"),
            ChatMessage::user("历史用户"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "历史助手".to_owned(),
                content_parts: Vec::new(),
            },
        ];
        let mut first_messages = stable.clone();
        first_messages.extend([
            ChatMessage::system("动态时间一"),
            ChatMessage::user("本轮一"),
        ]);
        let mut second_messages = stable;
        second_messages.extend([
            ChatMessage::user("本轮一"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "回复一".to_owned(),
                content_parts: Vec::new(),
            },
            ChatMessage::system("动态时间二"),
            ChatMessage::user("本轮二"),
        ]);

        let first =
            openai_responses_payload(&first_messages, "gpt-5.5", 1024, 1200, None, false, false)
                .unwrap();
        let second =
            openai_responses_payload(&second_messages, "gpt-5.5", 1024, 1200, None, false, false)
                .unwrap();
        let first_bytes = serde_json::to_vec(&first).unwrap();
        let second_bytes = serde_json::to_vec(&second).unwrap();
        let history_end = first_bytes
            .windows("历史助手".len())
            .position(|window| window == "历史助手".as_bytes())
            .unwrap()
            + "历史助手".len();

        assert!(common_prefix_len(&first_bytes, &second_bytes) >= history_end);

        let streaming =
            openai_responses_payload(&first_messages, "gpt-5.5", 1024, 1200, None, true, false)
                .unwrap();
        assert_eq!(first["input"], streaming["input"]);
    }

    #[test]
    fn openai_responses_payload_replays_assistant_history_as_output_text() {
        let messages = vec![
            ChatMessage::system("system"),
            ChatMessage::user("hi"),
            ChatMessage {
                role: ChatRole::Assistant,
                content: "old reply".to_owned(),
                content_parts: Vec::new(),
            },
            ChatMessage::user("again"),
        ];

        let payload = openai_responses_payload(
            &messages,
            "gpt-5.5",
            10 * 1024 * 1024,
            1200,
            Some(ReasoningEffort::Medium),
            true,
            false,
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
            10 * 1024 * 1024,
            1200,
            Some(ReasoningEffort::Medium),
            false,
            false,
        )
        .unwrap();

        assert!(payload.get("reasoning").is_none());
    }

    #[test]
    fn image_generation_tool_is_only_exposed_when_enabled() {
        let disabled = openai_responses_payload(
            &[ChatMessage::user("画一张图")],
            "gpt-5.6-luna",
            10 * 1024 * 1024,
            1200,
            None,
            false,
            false,
        )
        .unwrap();
        let enabled = openai_responses_payload(
            &[ChatMessage::user("画一张图")],
            "gpt-5.6-luna",
            10 * 1024 * 1024,
            1200,
            None,
            false,
            true,
        )
        .unwrap();

        assert!(disabled.get("tools").is_none());
        assert_eq!(enabled["tools"], json!([{"type": "image_generation"}]));
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
        let err =
            openai_responses_payload(&[], "gpt-5.5", 10 * 1024 * 1024, 1200, None, false, false)
                .unwrap_err();
        assert_eq!(err.code, "bad_request");

        let err = openai_responses_payload(
            &[ChatMessage::user(" \n\t ")],
            "gpt-5.5",
            10 * 1024 * 1024,
            1200,
            None,
            false,
            false,
        )
        .unwrap_err();
        assert_eq!(err.code, "bad_request");
    }

    #[test]
    fn openai_responses_payload_preserves_ordered_text_and_image_parts() {
        let payload = openai_responses_payload(
            &[ChatMessage::user_with_parts(
                "看图",
                vec![
                    MessageInputPart::text("先看这张"),
                    MessageInputPart::image(MessageMedia {
                        mime_type: Some("image/jpeg".to_owned()),
                        url: Some("https://example.test/a.jpg".to_owned()),
                        ..Default::default()
                    }),
                    MessageInputPart::text("再结合这句"),
                ],
            )],
            "gpt-5.5",
            10 * 1024 * 1024,
            1200,
            None,
            false,
            false,
        )
        .unwrap();
        let content = payload["input"][0]["content"].as_array().unwrap();

        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "先看这张");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "https://example.test/a.jpg");
        assert_eq!(content[2]["type"], "input_text");
        assert_eq!(content[2]["text"], "再结合这句");
    }

    #[test]
    fn openai_responses_payload_rejects_file_url_image_part() {
        let err = openai_responses_payload(
            &[ChatMessage::user_with_parts(
                "看图",
                vec![
                    MessageInputPart::text("看图"),
                    MessageInputPart::image(MessageMedia {
                        mime_type: Some("image/jpeg".to_owned()),
                        filename: Some("a.jpg".to_owned()),
                        url: Some("file://C:\\Users\\ThinkPad\\Pictures\\a.jpg".to_owned()),
                        ..Default::default()
                    }),
                ],
            )],
            "gpt-5.5",
            10 * 1024 * 1024,
            1200,
            None,
            false,
            false,
        )
        .unwrap_err();

        assert_eq!(err.code, "unsupported_input_part");
        assert!(err.message.contains("当前入口没有提供可读取图片内容"));
        assert!(!err.message.contains("C:\\Users"));
    }

    #[test]
    fn openai_responses_payload_uses_local_path_as_data_url() {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-openai-local-image-{}.png",
            std::process::id()
        ));
        std::fs::write(&path, b"fake-png").unwrap();

        let payload = openai_responses_payload(
            &[ChatMessage::user_with_parts(
                "看图",
                vec![MessageInputPart::image(MessageMedia {
                    mime_type: Some("image/png".to_owned()),
                    filename: Some("a.png".to_owned()),
                    local_path: Some(path.to_string_lossy().to_string()),
                    ..Default::default()
                })],
            )],
            "gpt-5.5",
            10 * 1024 * 1024,
            1200,
            None,
            false,
            false,
        )
        .unwrap();
        let image_url = payload["input"][0]["content"][0]["image_url"]
            .as_str()
            .unwrap();

        assert!(image_url.starts_with("data:image/png;base64,"));
        assert!(!image_url.contains(path.to_string_lossy().as_ref()));
    }

    #[test]
    fn openai_responses_payload_rejects_oversized_local_image() {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-openai-local-image-too-large-{}.png",
            std::process::id()
        ));
        std::fs::write(&path, b"12345678").unwrap();

        let err = openai_responses_payload(
            &[ChatMessage::user_with_parts(
                "看图",
                vec![MessageInputPart::image(MessageMedia {
                    mime_type: Some("image/png".to_owned()),
                    filename: Some("a.png".to_owned()),
                    local_path: Some(path.to_string_lossy().to_string()),
                    ..Default::default()
                })],
            )],
            "gpt-5.5",
            4,
            1200,
            None,
            false,
            false,
        )
        .unwrap_err();

        assert_eq!(err.code, "unsupported_input_part");
        assert!(err.message.contains("图片太大了"));
        assert!(!err.message.contains(path.to_string_lossy().as_ref()));
    }

    #[test]
    fn openai_responses_payload_ignores_generic_mime_when_path_is_png() {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-openai-local-generic-mime-{}.png",
            std::process::id()
        ));
        std::fs::write(&path, b"fake-png").unwrap();

        let payload = openai_responses_payload(
            &[ChatMessage::user_with_parts(
                "看图",
                vec![MessageInputPart::image(MessageMedia {
                    mime_type: Some("image".to_owned()),
                    filename: Some("upload".to_owned()),
                    local_path: Some(path.to_string_lossy().to_string()),
                    ..Default::default()
                })],
            )],
            "gpt-5.5",
            10 * 1024 * 1024,
            1200,
            None,
            false,
            false,
        )
        .unwrap();

        assert_eq!(
            payload["input"][0]["content"][0]["image_url"].as_str(),
            Some("data:image/png;base64,ZmFrZS1wbmc=")
        );
    }

    #[test]
    fn openai_responses_payload_keeps_reply_context_before_image_parts() {
        let payload = openai_responses_payload(
            &[ChatMessage::user_with_parts(
                "[reply message_id=quoted-1]\n上一条\n[/reply]\n看图",
                vec![
                    MessageInputPart::text("[reply message_id=quoted-1]\n上一条\n[/reply]\n"),
                    MessageInputPart::text("看图"),
                    MessageInputPart::image(MessageMedia {
                        mime_type: Some("image/jpeg".to_owned()),
                        filename: Some("a.jpg".to_owned()),
                        url: Some("https://example.test/a.jpg".to_owned()),
                        ..Default::default()
                    }),
                ],
            )],
            "gpt-5.5",
            10 * 1024 * 1024,
            1200,
            None,
            false,
            false,
        )
        .unwrap();
        let content = payload["input"][0]["content"].as_array().unwrap();

        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(
            content[0]["text"],
            "[reply message_id=quoted-1]\n上一条\n[/reply]\n"
        );
        assert_eq!(content[1]["type"], "input_text");
        assert_eq!(content[1]["text"], "看图");
        assert_eq!(content[2]["type"], "input_image");
        assert_eq!(content[2]["image_url"], "https://example.test/a.jpg");
    }

    #[test]
    fn openai_responses_payload_keeps_quote_and_current_text_once() {
        // Core 已将引用正文和当前正文分别组装为可信有序 parts；Provider 只能原样序列化。
        let payload = openai_responses_payload(
            &[ChatMessage::user_with_parts(
                "引用内容查看",
                vec![
                    MessageInputPart::text("引用元数据：reference=REFIDX_quoted"),
                    MessageInputPart::text("引用文本：测试"),
                    MessageInputPart::text("引用内容查看"),
                ],
            )],
            "gpt-5.5",
            10 * 1024 * 1024,
            1200,
            None,
            false,
            false,
        )
        .unwrap();
        let payload_text = payload["input"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(payload_text.matches("测试").count(), 1);
        assert_eq!(payload_text.matches("引用内容查看").count(), 1);
        assert!(!payload_text.contains("测试引用内容查看"));
    }

    #[test]
    fn openai_responses_payload_never_serializes_untrusted_mixed_quote() {
        let payload = openai_responses_payload(
            &[ChatMessage::user_with_parts(
                "引用内容查看",
                vec![
                    MessageInputPart::text("引用元数据：reference=REFIDX_quoted"),
                    MessageInputPart::Text {
                        text: "测试引用内容查看".to_owned(),
                        source: Some(TextSource::QuoteContaminated),
                    },
                    MessageInputPart::text("引用内容查看"),
                ],
            )],
            "gpt-5.5",
            10 * 1024 * 1024,
            1200,
            None,
            false,
            false,
        )
        .unwrap();
        let payload_text = payload["input"][0]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(payload_text.matches("引用内容查看").count(), 1);
        assert!(!payload_text.contains("测试引用内容查看"));
    }
}

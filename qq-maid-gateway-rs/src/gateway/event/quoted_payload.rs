//! QQ 引用正文、媒体与 payload fallback 组装。

use qq_maid_common::input_part::{MessageInputPart, QuotedMediaSummary, TextSource};
use serde_json::Value;

use crate::gateway::ref_index::qq::{MSG_TYPE_QUOTE, RawMsgElement};

use super::{
    QuotedPayloadFallback, RawParallelMessage, input_parts_from_content_and_attachments,
    parse_safe_content_parts,
};

pub(super) fn quoted_payload_fallback(
    message_type: Option<u64>,
    msg_elements: &[RawMsgElement],
    parallel_message: Option<&RawParallelMessage>,
    ref_msg_idx: Option<&str>,
) -> QuotedPayloadFallback {
    if message_type != Some(MSG_TYPE_QUOTE) {
        return QuotedPayloadFallback::default();
    }
    let Some(ref_msg_idx) = ref_msg_idx else {
        return QuotedPayloadFallback::default();
    };

    let mut content_fragments = Vec::new();
    let mut input_parts = Vec::new();
    // QQ 官方明确以 ref_msg_idx 定位被引用消息；无关顶层元素不得进入引用上下文。
    if let Some(quoted_root) = msg_elements.iter().find(|element| {
        element
            .msg_idx
            .as_deref()
            .is_some_and(|idx| idx.trim() == ref_msg_idx)
    }) {
        append_quoted_element_parts(quoted_root, &mut content_fragments, &mut input_parts);
    }

    let mut content = content_fragments.join("\n");
    if content.is_empty()
        && let Some(fallback) = parallel_message_text_for_idx(parallel_message, ref_msg_idx)
    {
        content = fallback;
        input_parts.insert(
            0,
            MessageInputPart::Text {
                text: content.clone(),
                source: Some(TextSource::Quote),
            },
        );
    }
    let media_summaries = input_parts
        .iter()
        .filter_map(QuotedMediaSummary::from_input_part)
        .collect::<Vec<_>>();

    QuotedPayloadFallback {
        content: (!content.is_empty()).then_some(content),
        input_parts,
        media_summaries,
    }
}

fn append_quoted_element_parts(
    element: &RawMsgElement,
    content_fragments: &mut Vec<String>,
    input_parts: &mut Vec<MessageInputPart>,
) {
    let raw_content = element.content.as_deref().unwrap_or_default();
    let cleaned_content = strip_qq_image_placeholders(raw_content);
    let parsed = parse_safe_content_parts(&cleaned_content, "qq_official");
    let element_content = parsed.text.trim().to_owned();
    if !element_content.is_empty() {
        content_fragments.push(element_content.clone());
    }

    let mut element_parts = input_parts_from_content_and_attachments(
        &element_content,
        parsed.input_parts,
        &element.attachments,
        "qq_official",
        TextSource::Quote,
    );
    for part in &mut element_parts {
        if let MessageInputPart::Text { source, .. } = part {
            *source = Some(TextSource::Quote);
        }
    }
    input_parts.extend(element_parts);

    for child in &element.msg_elements {
        append_quoted_element_parts(child, content_fragments, input_parts);
    }
}

fn parallel_message_text_for_idx(
    parallel_message: Option<&RawParallelMessage>,
    ref_msg_idx: &str,
) -> Option<String> {
    let text = parallel_message?
        .msg_nodes
        .iter()
        .filter(|node| parallel_message_node_idx(node) == Some(ref_msg_idx))
        .filter_map(parallel_message_node_text)
        .map(strip_qq_image_placeholders)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn parallel_message_node_idx(node: &Value) -> Option<&str> {
    node.as_object()?
        .get("msg_idx")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn parallel_message_node_text(node: &Value) -> Option<&str> {
    let object = node.as_object()?;
    // 只读取显式带匹配索引节点的展示文本，不接受无索引字符串兜底。
    ["content", "text", "msg_content"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
}

fn strip_qq_image_placeholders(value: &str) -> String {
    value.replace("[图片]", "").trim().to_owned()
}

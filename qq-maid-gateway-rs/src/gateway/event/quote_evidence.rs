//! QQ 引用 payload 的结构证据解析。
//!
//! 这里只在 `parallel_message` 节点与 ref/current idx 能互相印证时判断引用根混入
//! 当前正文；无法确认引用文字边界时返回污染标记，让上层保留元数据和媒体并丢弃文字。

use qq_maid_common::input_part::{MessageInputPart, TextSource};
use serde_json::Value;

use crate::gateway::ref_index::qq::RawMsgElement;

use super::RawParallelMessage;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct QuotedRootText {
    pub(super) override_text: Option<String>,
    pub(super) contaminated: bool,
}

pub(super) fn resolve_quoted_root_text(
    quoted_root: &RawMsgElement,
    parallel_message: Option<&RawParallelMessage>,
    ref_msg_idx: Option<&str>,
    current_msg_idx: Option<&str>,
) -> QuotedRootText {
    let (Some(ref_msg_idx), Some(current_msg_idx)) = (ref_msg_idx, current_msg_idx) else {
        return QuotedRootText::default();
    };
    if ref_msg_idx == current_msg_idx || quoted_root.msg_idx.as_deref() != Some(ref_msg_idx) {
        return QuotedRootText::default();
    }
    let Some(current_text) = parallel_message_text_for_idx(parallel_message, current_msg_idx)
    else {
        return QuotedRootText::default();
    };
    let root_text = strip_qq_image_placeholders(quoted_root.content.as_deref().unwrap_or_default());
    if root_text.is_empty()
        || current_text.is_empty()
        || root_text.chars().count() <= current_text.chars().count()
        || !root_text.ends_with(&current_text)
    {
        return QuotedRootText::default();
    }

    if let Some(quoted_text) = parallel_message_text_for_idx(parallel_message, ref_msg_idx)
        && root_text == format!("{quoted_text}{current_text}")
    {
        // 两个不同 idx 的 parallel 节点完整重建了混合根，可直接采用引用节点原文。
        return QuotedRootText {
            override_text: Some(quoted_text),
            contaminated: false,
        };
    }

    // current 节点证明根文本尾部混入本轮正文，但缺少可信引用原文；整段文字 fail-closed。
    QuotedRootText {
        override_text: Some(String::new()),
        contaminated: true,
    }
}

pub(super) fn has_contaminated_quote_marker(input_parts: &[MessageInputPart]) -> bool {
    input_parts.iter().any(|part| {
        matches!(
            part,
            MessageInputPart::Text {
                source: Some(TextSource::QuoteContaminated),
                ..
            }
        )
    })
}

pub(super) fn parallel_message_display_text(
    parallel_message: Option<&RawParallelMessage>,
    ref_msg_idx: Option<&str>,
    current_msg_idx: Option<&str>,
) -> Option<String> {
    let parallel_message = parallel_message?;
    if let Some(ref_msg_idx) = ref_msg_idx
        && let Some(text) = parallel_message_text_for_idx(Some(parallel_message), ref_msg_idx)
    {
        return Some(text);
    }
    let text = parallel_message
        .msg_nodes
        .iter()
        .filter(|node| {
            current_msg_idx.is_none_or(|idx| parallel_message_node_idx(node) != Some(idx))
        })
        .filter_map(parallel_message_node_text)
        .map(strip_qq_image_placeholders)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

pub(super) fn strip_qq_image_placeholders(value: &str) -> String {
    value.replace("[图片]", "").trim().to_owned()
}

fn parallel_message_text_for_idx(
    parallel_message: Option<&RawParallelMessage>,
    msg_idx: &str,
) -> Option<String> {
    let text = parallel_message?
        .msg_nodes
        .iter()
        .filter(|node| parallel_message_node_idx(node) == Some(msg_idx))
        .filter_map(parallel_message_node_text)
        .map(strip_qq_image_placeholders)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn parallel_message_node_idx(node: &Value) -> Option<&str> {
    let object = node.as_object()?;
    object
        .get("msg_idx")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            object
                .get("message_scene")
                .and_then(|scene| scene.get("ext"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .find_map(|item| item.trim().strip_prefix("msg_idx="))
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
}

fn parallel_message_node_text(node: &Value) -> Option<&str> {
    if let Some(text) = node.as_str() {
        return Some(text);
    }
    let object = node.as_object()?;
    // 只读取明确的展示文本键，避免 message_scene.ext 中临时凭证进入模型或日志。
    ["content", "text", "msg_content"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
}

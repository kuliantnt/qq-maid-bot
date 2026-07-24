//! QQ 引用 payload 的结构证据解析。
//!
//! 只有明确进入 [`QuotedRootText::Trusted`] 的文字才允许进入引用上下文；污染或证据
//! 不足时都由上层保留元数据和媒体、丢弃根文字。

use qq_maid_common::input_part::{MessageInputPart, TextSource};
use serde_json::Value;

use crate::gateway::ref_index::qq::RawMsgElement;

use super::RawParallelMessage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum QuotedRootText {
    Trusted(String),
    Contaminated,
    Unresolved,
}

pub(super) fn resolve_quoted_root_text(
    quoted_root: &RawMsgElement,
    parallel_message: Option<&RawParallelMessage>,
    ref_msg_idx: Option<&str>,
    ref_msg_idx_inferred: bool,
    current_msg_idx: Option<&str>,
    normalized_current_text: &str,
) -> QuotedRootText {
    let root_text = strip_qq_image_placeholders(quoted_root.content.as_deref().unwrap_or_default());
    if root_text.is_empty() {
        return QuotedRootText::Unresolved;
    }
    let current_text = strip_qq_image_placeholders(normalized_current_text);
    let indices_conflict = ref_msg_idx.is_some_and(|ref_idx| {
        !ref_msg_idx_inferred && quoted_root.msg_idx.as_deref() != Some(ref_idx)
    });

    if let Some(ref_idx) = ref_msg_idx
        && current_msg_idx != Some(ref_idx)
        && let Some(quoted_text) = parallel_message_text_for_idx(parallel_message, ref_idx)
    {
        if root_text == quoted_text {
            return QuotedRootText::Trusted(quoted_text);
        }
        if !current_text.is_empty() && root_text == format!("{quoted_text}{current_text}") {
            // ref 节点原文与标准化当前正文已完整重建混合根，不要求 parallel 再提供 current 节点。
            return QuotedRootText::Trusted(quoted_text);
        }
    }

    if !current_text.is_empty() && root_text.contains(&current_text) {
        // 能确认根文本混入当前正文，却没有可信 ref 原文重建前半段，整段文字 fail-closed。
        return QuotedRootText::Contaminated;
    }

    if current_text.is_empty() || indices_conflict {
        return QuotedRootText::Unresolved;
    }

    // 首元素属于引用根，且标准化当前正文未出现在其中；这里不使用由首元素自身推断的
    // ref_msg_idx 反向证明可信，避免形成循环证据。
    QuotedRootText::Trusted(root_text)
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

pub(super) fn trusted_parallel_ref_text(
    parallel_message: Option<&RawParallelMessage>,
    ref_msg_idx: Option<&str>,
) -> Option<String> {
    parallel_message_text_for_idx(parallel_message, ref_msg_idx?)
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

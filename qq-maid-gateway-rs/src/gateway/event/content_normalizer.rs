//! QQ 非引用消息内容归一化。
//!
//! 引用消息（103）由 `quoted_payload` 独立处理，避免这层重新解释 PR #582 已收敛的
//! 正文边界。ARK、平行消息和聊天记录只转成受限、可读的输入部分，不会访问卡片 URL。

use qq_maid_common::input_part::{MessageInputPart, TextSource};
use serde_json::Value;
use tracing::debug;

use crate::gateway::{
    logging::mask_url,
    ref_index::qq::{RawArkData, RawMsgElement},
};

use super::{Attachment, input_parts_from_content_and_attachments, parse_safe_content_parts};

const MESSAGE_TYPE_TEXT: u64 = 0;
const MESSAGE_TYPE_ARK: u64 = 3;
const MESSAGE_TYPE_PARALLEL: u64 = 101;
const MESSAGE_TYPE_CHAT_HISTORY: u64 = 102;
const MESSAGE_TYPE_QUOTE: u64 = 103;

#[derive(Debug, Clone, Copy)]
struct NormalizerLimits {
    max_depth: usize,
    max_nodes: usize,
    max_text_chars: usize,
    max_media: usize,
}

impl Default for NormalizerLimits {
    fn default() -> Self {
        Self {
            max_depth: 8,
            max_nodes: 256,
            max_text_chars: 16_000,
            max_media: 32,
        }
    }
}

pub(super) struct NormalizedInboundContent {
    /// 仅当前用户明确输入的顶层正文。Gateway 本地命令识别只使用此字段。
    pub(super) text: String,
    /// 顶层正文、附件及 3/101/102 安全摘要，按事件原始顺序排列。
    pub(super) input_parts: Vec<MessageInputPart>,
}

pub(super) fn normalize_qq_inbound_content(
    message_type: Option<u64>,
    content: &str,
    attachments: &[Attachment],
    ark_data: Option<&RawArkData>,
    msg_elements: &[RawMsgElement],
) -> NormalizedInboundContent {
    normalize_with_limits(
        message_type,
        content,
        attachments,
        ark_data,
        msg_elements,
        NormalizerLimits::default(),
    )
}

fn normalize_with_limits(
    message_type: Option<u64>,
    content: &str,
    attachments: &[Attachment],
    ark_data: Option<&RawArkData>,
    msg_elements: &[RawMsgElement],
    limits: NormalizerLimits,
) -> NormalizedInboundContent {
    let mut state = NormalizerState::new(limits, message_type.unwrap_or(MESSAGE_TYPE_TEXT));
    let explicit_content = state.limit_text(content).trim().to_owned();
    let parsed = parse_safe_content_parts(&explicit_content, "qq_official");
    let mut input_parts = input_parts_from_content_and_attachments(
        &parsed.text,
        parsed.input_parts,
        attachments,
        "qq_official",
        TextSource::Transcript,
    );
    retain_media_with_limit(&mut input_parts, &mut state);

    match message_type {
        Some(MESSAGE_TYPE_ARK) => {
            if let Some(summary) = ark_summary(ark_data, &mut state) {
                input_parts.push(MessageInputPart::Text {
                    text: summary,
                    source: Some(TextSource::Transcript),
                });
            }
        }
        Some(MESSAGE_TYPE_PARALLEL | MESSAGE_TYPE_CHAT_HISTORY) => {
            for element in msg_elements {
                append_element(element, 0, &mut input_parts, &mut state);
            }
        }
        Some(MESSAGE_TYPE_TEXT | MESSAGE_TYPE_QUOTE) | None => {}
        Some(kind) => {
            // 未知类型不能丢弃正文或附件；上面的顶层通用路径已保留它们。
            debug!(
                message_type = kind,
                "QQ inbound message used generic content normalization"
            );
        }
    }

    state.emit_diagnostic();
    NormalizedInboundContent {
        text: parsed.text.trim().to_owned(),
        input_parts,
    }
}

fn append_element(
    element: &RawMsgElement,
    depth: usize,
    output: &mut Vec<MessageInputPart>,
    state: &mut NormalizerState,
) {
    if depth >= state.limits.max_depth {
        state.depth_limited = true;
        return;
    }
    if state.node_count >= state.limits.max_nodes {
        state.node_limited = true;
        return;
    }
    state.node_count += 1;

    if element.message_type == Some(MESSAGE_TYPE_ARK) {
        if let Some(summary) = ark_summary(element.ark_data.as_ref(), state) {
            output.push(MessageInputPart::Text {
                text: summary,
                source: Some(TextSource::Transcript),
            });
        }
    } else {
        let content = state.limit_text(element.content.as_deref().unwrap_or_default());
        let parsed = parse_safe_content_parts(&content, "qq_official");
        let mut parts = input_parts_from_content_and_attachments(
            &parsed.text,
            parsed.input_parts,
            &element.attachments,
            "qq_official",
            TextSource::Transcript,
        );
        retain_media_with_limit(&mut parts, state);
        output.extend(parts);
    }

    for child in &element.msg_elements {
        append_element(child, depth + 1, output, state);
    }
}

fn ark_summary(ark_data: Option<&RawArkData>, state: &mut NormalizerState) -> Option<String> {
    let ark = ark_data?;
    let mut lines = Vec::new();
    let header = state.limit_text("[ARK 卡片]");
    if !header.is_empty() {
        lines.push(header);
    }
    append_field(&mut lines, "prompt", ark.prompt.as_deref(), state);
    append_field(
        &mut lines,
        "type",
        value_as_text(ark.ark_type.as_ref()).as_deref(),
        state,
    );
    append_field(&mut lines, "name", ark.name.as_deref(), state);
    append_field(&mut lines, "name", ark.ark_name.as_deref(), state);
    for (label, keys) in [
        ("title", &["title"][..]),
        ("description", &["description", "desc"][..]),
        ("source", &["source", "tag", "tags"][..]),
        ("address", &["address"][..]),
        // URL 仅作为文本元数据，归一化过程不发起任何网络请求。
        ("url", &["jump_url", "url"][..]),
    ] {
        let value = keys.iter().find_map(|key| {
            value_as_text(ark.fields.get(*key)).or_else(|| value_as_text(ark.extra.get(*key)))
        });
        let safe_value = (label == "url")
            .then(|| value.as_deref().map(mask_url))
            .flatten();
        append_field(
            &mut lines,
            label,
            safe_value.as_deref().or(value.as_deref()),
            state,
        );
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn append_field(
    lines: &mut Vec<String>,
    label: &str,
    value: Option<&str>,
    state: &mut NormalizerState,
) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    // 标签、分隔符和换行同样属于模型可见文本，必须计入总长度上限。
    let field = state.limit_text(&format!("{label}: {value}"));
    if !field.is_empty() {
        lines.push(field);
    }
}

fn value_as_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => Some(value.to_owned()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn retain_media_with_limit(parts: &mut Vec<MessageInputPart>, state: &mut NormalizerState) {
    parts.retain(|part| {
        if matches!(part, MessageInputPart::Text { .. }) {
            return true;
        }
        if state.media_count >= state.limits.max_media {
            state.media_limited = true;
            return false;
        }
        state.media_count += 1;
        true
    });
}

struct NormalizerState {
    limits: NormalizerLimits,
    root_message_type: u64,
    node_count: usize,
    text_chars: usize,
    media_count: usize,
    depth_limited: bool,
    node_limited: bool,
    text_limited: bool,
    media_limited: bool,
}

impl NormalizerState {
    fn new(limits: NormalizerLimits, root_message_type: u64) -> Self {
        Self {
            limits,
            root_message_type,
            node_count: 0,
            text_chars: 0,
            media_count: 0,
            depth_limited: false,
            node_limited: false,
            text_limited: false,
            media_limited: false,
        }
    }

    fn limit_text(&mut self, value: &str) -> String {
        let remaining = self.limits.max_text_chars.saturating_sub(self.text_chars);
        let original_len = value.chars().count();
        let limited = if original_len > remaining {
            let notice = "[内容已截断]";
            let notice_len = notice.chars().count();
            let prefix_len = remaining.saturating_sub(notice_len);
            let mut result = value.chars().take(prefix_len).collect::<String>();
            if remaining >= notice_len {
                result.push_str(notice);
            }
            result
        } else {
            value.to_owned()
        };
        let limited_len = limited.chars().count();
        self.text_chars += limited_len;
        if limited_len < original_len {
            self.text_limited = true;
        }
        limited
    }

    fn emit_diagnostic(&self) {
        if self.depth_limited || self.node_limited || self.text_limited || self.media_limited {
            debug!(
                message_type = self.root_message_type,
                node_count = self.node_count,
                text_chars = self.text_chars,
                media_count = self.media_count,
                depth_limited = self.depth_limited,
                node_limited = self.node_limited,
                text_limited = self.text_limited,
                media_limited = self.media_limited,
                "QQ inbound content normalization truncated"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ark_summary_only_exposes_allowlisted_text_metadata() {
        let normalized = normalize_with_limits(
            Some(MESSAGE_TYPE_ARK),
            "",
            &[],
            Some(&RawArkData {
                prompt: Some("分享卡片".to_owned()),
                ark_type: Some(Value::String("news".to_owned())),
                name: None,
                ark_name: Some("图文".to_owned()),
                fields: serde_json::json!({
                    "title": "标题",
                    "desc": "说明",
                    "jump_url": "https://example.test/card?token=secret",
                    "ignored": {"nested": "value"}
                })
                .as_object()
                .unwrap()
                .clone(),
                extra: serde_json::json!({"address": "顶层地址"})
                    .as_object()
                    .unwrap()
                    .clone(),
            }),
            &[],
            NormalizerLimits::default(),
        );

        let text = normalized.input_parts[0].text_content().unwrap();
        assert!(text.contains("prompt: 分享卡片"));
        assert!(text.contains("title: 标题"));
        assert!(text.contains("address: 顶层地址"));
        assert!(text.contains("url: https://example.test/card?token=***"));
        assert!(!text.contains("ignored"));
        assert!(normalized.text.is_empty());
    }

    #[test]
    fn recursive_elements_preserve_order_and_apply_limits() {
        let elements = vec![RawMsgElement {
            msg_idx: None,
            content: Some("first".to_owned()),
            message_type: Some(MESSAGE_TYPE_TEXT),
            attachments: Vec::new(),
            ark_data: None,
            msg_elements: vec![RawMsgElement {
                msg_idx: None,
                content: Some("second".to_owned()),
                message_type: Some(MESSAGE_TYPE_TEXT),
                attachments: Vec::new(),
                ark_data: None,
                msg_elements: Vec::new(),
            }],
        }];
        let normalized = normalize_with_limits(
            Some(MESSAGE_TYPE_PARALLEL),
            "current",
            &[],
            None,
            &elements,
            NormalizerLimits {
                max_depth: 1,
                max_nodes: 1,
                max_text_chars: 64,
                max_media: 1,
            },
        );

        assert_eq!(normalized.text, "current");
        assert_eq!(normalized.input_parts[0].text_content(), Some("current"));
        assert_eq!(normalized.input_parts[1].text_content(), Some("first"));
        assert_eq!(normalized.input_parts.len(), 2);
    }
}

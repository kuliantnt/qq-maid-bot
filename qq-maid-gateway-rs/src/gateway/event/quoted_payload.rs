//! QQ 引用消息 msg_elements 递归解析。
//!
//! 根据 QQ 最新消息结构文档：
//! - 顶层 `content` 是当前用户本轮发送的正文。
//! - `message_type = 103` 时的 `msg_elements` 是引用消息的内容元素，可递归嵌套。
//! - 不再要求元素的 `msg_idx` 必须等于 `ref_msg_idx`（官方事件不保证携带 `msg_idx`）。
//! - `ref_msg_idx` 仅用于 RefIndex 查询和引用元数据展示。

use qq_maid_common::input_part::{
    MessageInputPart, QuotedMediaSummary, QuotedMessageContext, TextSource,
};

use crate::gateway::ref_index::qq::{MSG_TYPE_QUOTE, RawMsgElement};

use super::{input_parts_from_content_and_attachments, parse_safe_content_parts};

/// 使用归一化后的当前正文检测并移除 `QuotedMessageContext` 中被污染的引用文字。
///
/// 应在群聊 inbound 完成 @机器人/唤醒词/分隔符剥离后、RefIndex enrich 前调用，
/// 确保检测用的当前正文与最终进入 Core 的正文一致。
///
/// RefIndex 命中时会用索引原文覆盖 `input_parts`，因此本函数只影响 RefIndex miss 的最终状态。
pub(crate) fn strip_contaminated_quote_from_context(
    quoted: &mut QuotedMessageContext,
    current_body: &str,
) {
    let current_body = current_body.trim();
    if current_body.is_empty() {
        return;
    }

    let has_text = quoted
        .input_parts
        .iter()
        .any(|part| matches!(part, MessageInputPart::Text { .. }));
    if !has_text {
        return;
    }

    let any_contaminated = quoted.input_parts.iter().any(|part| {
        if let MessageInputPart::Text { text, .. } = part {
            let trimmed = text.trim();
            trimmed != current_body && trimmed.ends_with(current_body)
        } else {
            false
        }
    });

    if !any_contaminated {
        return;
    }

    // 丢弃所有引用文字，保留图片和其他媒体。
    quoted
        .input_parts
        .retain(|part| !matches!(part, MessageInputPart::Text { .. }));
    quoted.text_summary = None;
}

/// 当 `message_type == 103` 时，按原始顺序递归解析全部 `msg_elements` 作为引用内容。
///
/// 无论元素是否携带 `msg_idx`，所有文字、附件及嵌套子元素均组成引用内容。
/// `ref_msg_idx` 不参与元素筛选；调用方自行决定是否用于 RefIndex 查询和元数据展示。
pub(super) fn parse_quoted_message_elements(
    message_type: Option<u64>,
    msg_elements: &[RawMsgElement],
) -> QuotedPayloadFallback {
    if message_type != Some(MSG_TYPE_QUOTE) {
        return QuotedPayloadFallback::default();
    }

    let mut content_fragments = Vec::new();
    let mut input_parts = Vec::new();

    for element in msg_elements {
        append_quoted_element_parts(element, &mut content_fragments, &mut input_parts);
    }

    let content = content_fragments.join("\n");
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

    // 递归解析嵌套子元素（图文引用可能含多级嵌套）。
    for child in &element.msg_elements {
        append_quoted_element_parts(child, content_fragments, input_parts);
    }
}

fn strip_qq_image_placeholders(value: &str) -> String {
    value.replace("[图片]", "").trim().to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct QuotedPayloadFallback {
    pub(super) content: Option<String>,
    pub(super) input_parts: Vec<MessageInputPart>,
    pub(super) media_summaries: Vec<QuotedMediaSummary>,
}

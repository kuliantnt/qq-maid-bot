//! QQ 引用消息直接引用层解析。
//!
//! 根据 QQ 最新消息结构文档：
//! - 顶层 `content` 是当前用户本轮发送的正文。
//! - `message_type = 103` 时的顶层 `msg_elements` 是本轮直接引用目标的内容元素。
//! - 元素内部的 `msg_elements` 属于更早的历史引用，不递归恢复其中正文或媒体。
//! - QQ 拍平到顶层元素 `content` 的展示文本按普通引用文本保留，不解释其中字段。
//! - 不再要求元素的 `msg_idx` 必须等于 `ref_msg_idx`（官方事件不保证携带 `msg_idx`）。
//! - `ref_msg_idx` 仅用于 RefIndex 查询和引用元数据展示。

use qq_maid_common::input_part::{
    MessageInputPart, QuotedMediaSummary, QuotedMessageContext, TextSource,
};
use tracing::debug;

use crate::gateway::ref_index::qq::{MSG_TYPE_QUOTE, RawMsgElement};

use super::content_normalizer::MAX_NORMALIZED_MEDIA_PARTS;
use super::{
    AttachmentKind, attachment_kind, input_parts_from_content_and_attachments,
    parse_safe_content_parts,
};

/// 使用归一化后的当前正文检测并移除 `QuotedMessageContext` 中被污染的引用文字。
///
/// 应在群聊 inbound 完成 @机器人/唤醒词/分隔符剥离后、RefIndex enrich 前调用，
/// 确保检测用的当前正文与最终进入 Core 的正文一致。
///
/// 完整 RefIndex 命中时会用索引原文覆盖 `input_parts`；被动观察命中时会用索引正文
/// 与这里保留下来的事件媒体合并，因此本检测也负责避免污染文字进入该合并结果。
///
/// 仅在引用上下文恰好只有一个非空 Text part 时启用检测——
/// 这是 QQ msg_elements 引用消息中最常见的混合形态。多段落或零文字不触发。
pub(crate) fn strip_contaminated_quote_from_context(
    quoted: &mut QuotedMessageContext,
    current_body: &str,
) {
    let current_body = current_body.trim();
    if current_body.is_empty() {
        return;
    }

    // 只收集非空 Text part。
    let text_parts: Vec<&str> = quoted
        .input_parts
        .iter()
        .filter_map(|part| {
            if let MessageInputPart::Text { text, .. } = part {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
            None
        })
        .collect();

    // 仅在引用上下文恰好只有一个非空 Text part 时启用检测——
    // 这是 QQ 引用消息中最常见的混合形态。多个文字段落不触发，
    // 避免误删独立的引用正文。
    if text_parts.len() != 1 {
        return;
    }

    // 当前正文过短（如单字 "好"）时后缀匹配无区分力，不触发。
    if current_body.chars().count() < 2 {
        return;
    }

    let text = text_parts[0];
    // 以当前正文结尾且不等同 → 判定为混合串，丢弃引用文字。
    if text != current_body && text.ends_with(current_body) {
        quoted
            .input_parts
            .retain(|part| !matches!(part, MessageInputPart::Text { .. }));
        quoted.text_summary = None;
    }
}

/// 当 `message_type == 103` 时，按原始顺序解析顶层 `msg_elements` 作为直接引用内容。
///
/// 无论元素是否携带 `msg_idx`，该层的文字和全部附件均组成引用内容；子元素只表示
/// 更早的结构化历史引用，不递归恢复。平台拍平到该层 `content` 的展示文本仍按普通
/// 引用文本保留，不提取其中的附件描述或临时 URL。
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
        append_direct_quoted_element_parts(element, &mut content_fragments, &mut input_parts);
    }
    retain_direct_quoted_media_with_limit(&mut input_parts);

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

fn retain_direct_quoted_media_with_limit(input_parts: &mut Vec<MessageInputPart>) {
    let mut media_count = 0usize;
    let original_media_count = input_parts.iter().filter(|part| part.is_non_text()).count();
    input_parts.retain(|part| {
        if !part.is_non_text() {
            return true;
        }
        if media_count >= MAX_NORMALIZED_MEDIA_PARTS {
            return false;
        }
        media_count += 1;
        true
    });
    if original_media_count > media_count {
        debug!(
            media_count,
            original_media_count,
            max_media = MAX_NORMALIZED_MEDIA_PARTS,
            "QQ 直接引用媒体因标准化限制被截断"
        );
    }
}

fn append_direct_quoted_element_parts(
    element: &RawMsgElement,
    content_fragments: &mut Vec<String>,
    input_parts: &mut Vec<MessageInputPart>,
) {
    let raw_content = element.content.as_deref().unwrap_or_default();
    // 只有同层确实携带结构化图片附件时，`[图片]` 才作为附件顺序占位符处理。
    // 文件或音频不能打开媒体标记解析，否则拍平文本中的 `[图片]` / `<img>` 会伪造图片。
    let has_structured_image_attachment = element.attachments.iter().any(|attachment| {
        attachment_kind(
            attachment.content_type.as_deref(),
            attachment.filename.as_deref(),
        ) == AttachmentKind::Image
    });
    let (summary_content, mut element_parts) = if has_structured_image_attachment {
        let cleaned_content = strip_qq_image_placeholders(raw_content);
        let summary_content = parse_safe_content_parts(&cleaned_content, "qq_official")
            .text
            .trim()
            .to_owned();
        let protocol_content = raw_content.replace("[图片]", "<img>");
        let parsed = parse_safe_content_parts(&protocol_content, "qq_official");
        let element_parts = input_parts_from_content_and_attachments(
            &parsed.text,
            parsed.input_parts,
            &element.attachments,
            "qq_official",
            TextSource::Quote,
        );
        (summary_content, element_parts)
    } else {
        // 无同层结构化图片时完全绕过 `<img>` 解析器；正文仅做既有的边界空白清理，
        // 文件、音频等附件仍由统一附件转换逻辑追加为原有类型。
        let plain_content = raw_content.trim().to_owned();
        let element_parts = input_parts_from_content_and_attachments(
            &plain_content,
            Vec::new(),
            &element.attachments,
            "qq_official",
            TextSource::Quote,
        );
        (plain_content, element_parts)
    };
    if !summary_content.is_empty() {
        content_fragments.push(summary_content);
    }

    for part in &mut element_parts {
        if let MessageInputPart::Text { text, source } = part {
            // QQ 会在 `[图片]` 占位符两侧注入展示空格；结构化拆分后清理段落边界，
            // 避免模型看到仅由平台布局产生的前导/尾随空白。
            *text = text.trim().to_owned();
            *source = Some(TextSource::Quote);
        }
    }
    element_parts
        .retain(|part| !matches!(part, MessageInputPart::Text { text, .. } if text.is_empty()));
    input_parts.extend(element_parts);

    // `element.msg_elements` 是更早的结构化引用：不递归访问即可阻止历史媒体恢复。
    // 若 QQ 已把二次引用展示拍平进本层 `content`，上面的普通文本路径会原样保留它。
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

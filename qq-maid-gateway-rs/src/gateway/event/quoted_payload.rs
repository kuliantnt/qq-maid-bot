//! QQ 引用正文、媒体与 payload fallback 组装。

use qq_maid_common::input_part::{MessageInputPart, QuotedMediaSummary, TextSource};

use crate::gateway::ref_index::qq::{MSG_TYPE_QUOTE, RawMsgElement};

use super::{
    QuotedPayloadFallback, RawParallelMessage, input_parts_from_content_and_attachments,
    parse_safe_content_parts,
    quote_evidence::{
        QuotedRootText, has_contaminated_quote_marker, resolve_quoted_root_text,
        strip_qq_image_placeholders, trusted_parallel_ref_text,
    },
};

pub(super) fn quoted_payload_fallback(
    message_type: Option<u64>,
    msg_elements: &[RawMsgElement],
    parallel_message: Option<&RawParallelMessage>,
    current_msg_idx: Option<&str>,
    ref_msg_idx: Option<&str>,
    ref_msg_idx_inferred: bool,
    normalized_current_content: &str,
) -> QuotedPayloadFallback {
    if message_type != Some(MSG_TYPE_QUOTE) {
        return QuotedPayloadFallback::default();
    }

    // 只有首个顶层元素属于引用根；后续顶层元素可能是等价表示或当前消息内容。
    let mut content_fragments = Vec::new();
    let mut input_parts = Vec::new();
    if let Some(quoted_root) = msg_elements.first() {
        let root_text = resolve_quoted_root_text(
            quoted_root,
            parallel_message,
            ref_msg_idx,
            ref_msg_idx_inferred,
            current_msg_idx,
            normalized_current_content,
        );
        append_quoted_element_parts(
            quoted_root,
            current_msg_idx,
            normalized_current_content,
            true,
            Some(&root_text),
            false,
            &mut content_fragments,
            &mut input_parts,
        );
    }

    let mut content = content_fragments.join("\n");
    if content.is_empty()
        && !has_contaminated_quote_marker(&input_parts)
        && let Some(fallback) = trusted_parallel_ref_text(parallel_message, ref_msg_idx)
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

#[allow(clippy::too_many_arguments)]
fn append_quoted_element_parts(
    element: &RawMsgElement,
    current_msg_idx: Option<&str>,
    current_content: &str,
    is_quoted_root: bool,
    root_text: Option<&QuotedRootText>,
    discard_text: bool,
    content_fragments: &mut Vec<String>,
    input_parts: &mut Vec<MessageInputPart>,
) {
    let raw_content = element.content.as_deref().unwrap_or_default();
    let cleaned_content = strip_qq_image_placeholders(raw_content);
    let parsed = parse_safe_content_parts(&cleaned_content, "qq_official");
    let parsed_element_content = parsed.text.trim().to_owned();
    let root_is_contaminated = matches!(root_text, Some(QuotedRootText::Contaminated));
    let element_content = match root_text {
        Some(QuotedRootText::Trusted(text)) => text.clone(),
        Some(QuotedRootText::Contaminated | QuotedRootText::Unresolved) => String::new(),
        None if discard_text => String::new(),
        None => parsed_element_content.clone(),
    };

    let matches_current_idx =
        current_msg_idx.is_some_and(|idx| element.msg_idx.as_deref() == Some(idx));
    let lacks_comparable_idx = current_msg_idx.is_none() || element.msg_idx.is_none();
    let matches_current_content = lacks_comparable_idx
        && !current_content.is_empty()
        && parsed_element_content == current_content;
    if !is_quoted_root && (matches_current_idx || matches_current_content) {
        return;
    }
    if !element_content.is_empty() {
        content_fragments.push(element_content.clone());
    }

    let mut parsed_parts = parsed.input_parts;
    if discard_text {
        parsed_parts.retain(|part| !matches!(part, MessageInputPart::Text { .. }));
    }
    if let Some(resolution) = root_text {
        parsed_parts.retain(|part| !matches!(part, MessageInputPart::Text { .. }));
        if !element_content.is_empty() {
            parsed_parts.insert(
                0,
                MessageInputPart::Text {
                    text: element_content.clone(),
                    source: Some(TextSource::Quote),
                },
            );
        }
        if matches!(resolution, QuotedRootText::Contaminated) {
            parsed_parts.insert(
                0,
                MessageInputPart::Text {
                    text: String::new(),
                    source: Some(TextSource::QuoteContaminated),
                },
            );
        }
    }
    let mut element_parts = input_parts_from_content_and_attachments(
        &element_content,
        parsed_parts,
        &element.attachments,
        "qq_official",
        TextSource::Quote,
    );
    for part in &mut element_parts {
        if let MessageInputPart::Text { source, .. } = part
            && *source != Some(TextSource::QuoteContaminated)
        {
            *source = Some(TextSource::Quote);
        }
    }
    input_parts.extend(element_parts);

    for child in &element.msg_elements {
        append_quoted_element_parts(
            child,
            current_msg_idx,
            current_content,
            false,
            None,
            discard_text || root_is_contaminated,
            content_fragments,
            input_parts,
        );
    }
}

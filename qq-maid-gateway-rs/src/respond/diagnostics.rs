//! Core 输出、入站媒体和脱敏上下文的 Gateway 诊断日志。

use qq_maid_common::input_part::{MediaStatus, MessageInputPart};
use qq_maid_core::service::CoreRespondOutput;
use tracing::{debug, info};

use crate::{gateway::platform, logging::mask_openid};

pub(super) fn log_core_output_success(
    message_id: &str,
    masked_user: Option<&str>,
    masked_group: Option<&str>,
    output: &CoreRespondOutput,
) {
    let output_policy = output.output_policy().as_str();
    match output {
        CoreRespondOutput::Complete(response) => {
            info!(
                message_id,
                user = masked_user.unwrap_or(""),
                group = masked_group.unwrap_or(""),
                handled = response.handled.unwrap_or(false),
                handled_present = response.handled.is_some(),
                command = response.command.as_deref().unwrap_or(""),
                reply_len = response
                    .text_content()
                    .map(|text| text.chars().count())
                    .unwrap_or(0),
                transport = "complete",
                response_delivery_mode = output_policy,
                "Core 回复请求成功"
            );
        }
        CoreRespondOutput::Stream(_) => {
            debug!(
                message_id,
                user = masked_user.unwrap_or(""),
                group = masked_group.unwrap_or(""),
                transport = "stream",
                response_delivery_mode = output_policy,
                "Core 回复流已初始化"
            );
        }
    }
}

pub(super) fn masked_log_context_from_inbound(
    inbound: &platform::InboundMessage,
) -> (Option<String>, Option<String>) {
    match inbound.conversation.kind() {
        "private" | "service_account" => {
            (inbound.actor.sender_id.as_deref().map(mask_openid), None)
        }
        "group" => (None, Some(mask_openid(inbound.conversation.target_id()))),
        _ => (None, None),
    }
}

pub(super) fn log_inbound_media_diagnostics(inbound: &platform::InboundMessage) {
    let mut image_part_count = 0usize;
    let mut file_part_count = 0usize;
    let mut image_has_remote_url = false;
    let mut image_has_media_id = false;
    let mut image_url_scheme = "none";
    let mut media_status = "none";

    for part in &inbound.input_parts {
        match part {
            MessageInputPart::Image { media } => {
                image_part_count += 1;
                image_has_remote_url |= media.remote_url().is_some();
                image_has_media_id |= has_any_media_id(
                    media.media_id.as_deref(),
                    media.file_id.as_deref(),
                    media.attachment_id.as_deref(),
                );
                image_url_scheme = media.url_scheme().as_str();
                media_status = media_status_label(media.status);
            }
            MessageInputPart::File { media } => {
                file_part_count += 1;
                media_status = media_status_label(media.status);
            }
            MessageInputPart::Text { .. } | MessageInputPart::Unknown { .. } => {}
        }
    }

    if image_part_count == 0 && file_part_count == 0 {
        return;
    }

    debug!(
        message_id = %inbound.message_id,
        platform = %inbound.platform.as_str(),
        conversation_kind = %inbound.conversation.kind(),
        input_part_count = inbound.input_parts.len(),
        image_part_count,
        file_part_count,
        image_has_remote_url,
        image_has_media_id,
        image_url_scheme,
        media_status,
        "入站媒体可读性诊断"
    );
}

fn has_any_media_id(
    media_id: Option<&str>,
    file_id: Option<&str>,
    attachment_id: Option<&str>,
) -> bool {
    [media_id, file_id, attachment_id]
        .into_iter()
        .flatten()
        .any(|value| !value.trim().is_empty())
}

fn media_status_label(status: MediaStatus) -> &'static str {
    match status {
        MediaStatus::Available => "available",
        MediaStatus::MissingReadableUrl => "missing_readable_url",
        MediaStatus::SizeExceeded => "size_exceeded",
        MediaStatus::UnsupportedType => "unsupported_type",
        MediaStatus::DownloadFailed => "download_failed",
        MediaStatus::Expired => "expired",
    }
}

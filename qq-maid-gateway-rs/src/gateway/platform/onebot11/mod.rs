//! OneBot 11 消息事件到统一入站模型的 adapter。
//!
//! 本模块处理一期文本、结构化 `at`、reply、图片/文件段与触发语义。CQ 字符串和
//! OneBot 客户端本机路径不进入 Core，原始 segment payload 也不得向后泄漏。

use qq_maid_common::{
    command_prefix::CommandPrefix,
    identity_context::{IdentitySource, MentionConfidence, MentionIdentity, MessageActorContext},
    input_part::{MediaStatus, MessageInputPart, MessageMedia, QuotedMessageContext, TextSource},
};
use serde_json::{Map, Value};

use crate::gateway::onebot11::protocol::{MessageSegment, OneBotEvent, OneBotMessage};

use super::model::{Actor, ConversationTarget, GroupMemberRoleKind, InboundMessage, Platform};

mod sanitize;

use sanitize::{
    clean_data_id, clean_data_string, clean_data_u64, explicit_media_status, infer_image_mime,
    safe_filename, safe_mime_type, safe_opaque_reference, safe_remote_url,
};

/// OneBot 事件的 adapter 结果。被忽略的事件保留稳定分类，便于调用方做限量结构化观测，
/// 但不得记录消息正文或完整 ID。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OneBotInboundOutcome {
    Message(Box<InboundMessage>),
    Ignored(OneBotIgnoreReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OneBotIgnoreReason {
    NonMessageEvent,
    MessageSent,
    UnsupportedMessageType,
    UnsupportedMessageEncoding,
    MissingUserId,
    MissingGroupId,
    MissingMessageId,
    MissingMessage,
    SelfMessage,
    GroupNotTriggered,
}

impl OneBotIgnoreReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NonMessageEvent => "non_message_event",
            Self::MessageSent => "message_sent",
            Self::UnsupportedMessageType => "unsupported_message_type",
            Self::UnsupportedMessageEncoding => "unsupported_message_encoding",
            Self::MissingUserId => "missing_user_id",
            Self::MissingGroupId => "missing_group_id",
            Self::MissingMessageId => "missing_message_id",
            Self::MissingMessage => "missing_message",
            Self::SelfMessage => "self_message",
            Self::GroupNotTriggered => "group_not_triggered",
        }
    }
}

/// 将已通过协议层反序列化的事件适配为统一入站消息。
///
/// 一期群聊只接受明确 `at` 当前 `self_id`、配置前缀命令或携带 reply 的候选消息；
/// reply 是否确实指向机器人由后续 ref_index 判定。当前账号自己发送的 `message` 和
/// `message_sent` 均被过滤，避免后续聊天闭环形成回声循环。
#[cfg(test)]
pub(crate) fn inbound_from_event(event: &OneBotEvent) -> OneBotInboundOutcome {
    inbound_from_event_with_media_limit(
        event,
        crate::config::DEFAULT_MEDIA_MAX_BYTES,
        CommandPrefix::default(),
    )
}

pub(crate) fn inbound_from_event_with_media_limit(
    event: &OneBotEvent,
    media_max_bytes: u64,
    command_prefix: CommandPrefix,
) -> OneBotInboundOutcome {
    if event.post_type == "message_sent" {
        return OneBotInboundOutcome::Ignored(OneBotIgnoreReason::MessageSent);
    }
    if event.post_type != "message" {
        return OneBotInboundOutcome::Ignored(OneBotIgnoreReason::NonMessageEvent);
    }

    let message_type = match event.message_type.as_deref() {
        Some("private") => MessageType::Private,
        Some("group") => MessageType::Group,
        _ => {
            return OneBotInboundOutcome::Ignored(OneBotIgnoreReason::UnsupportedMessageType);
        }
    };
    let Some(user_id) = event_id(event, "user_id").or_else(|| sender_id(event)) else {
        return OneBotInboundOutcome::Ignored(OneBotIgnoreReason::MissingUserId);
    };
    if user_id == event.self_id.as_str() {
        return OneBotInboundOutcome::Ignored(OneBotIgnoreReason::SelfMessage);
    }
    let Some(message_id) = event_id(event, "message_id") else {
        return OneBotInboundOutcome::Ignored(OneBotIgnoreReason::MissingMessageId);
    };
    let Some(message) = event.message.as_ref() else {
        return OneBotInboundOutcome::Ignored(OneBotIgnoreReason::MissingMessage);
    };
    let OneBotMessage::Segments(segments) = message else {
        // 一期内部格式只接受 segment 数组，不能把 CQ 字符串解析扩散到核心链路。
        return OneBotInboundOutcome::Ignored(OneBotIgnoreReason::UnsupportedMessageEncoding);
    };

    let parsed = parse_segments(
        segments,
        event.self_id.as_str(),
        &message_id,
        media_max_bytes,
    );
    let conversation = match message_type {
        MessageType::Private => ConversationTarget::Private {
            target_id: user_id.clone(),
        },
        MessageType::Group => {
            // reply 当前机器人时是否触发，需要在 scope worker 内通过 ref_index 判定；
            // adapter 只允许含结构化 reply 的候选继续，不能把任意群消息都送入 Core。
            if !parsed.mentioned_bot
                && parsed.quoted.is_none()
                && !command_prefix.is_candidate_with_sealdice_compat(&parsed.text)
            {
                return OneBotInboundOutcome::Ignored(OneBotIgnoreReason::GroupNotTriggered);
            }
            let Some(group_id) = event_id(event, "group_id") else {
                return OneBotInboundOutcome::Ignored(OneBotIgnoreReason::MissingGroupId);
            };
            ConversationTarget::Group {
                target_id: group_id,
            }
        }
    };

    OneBotInboundOutcome::Message(Box::new(InboundMessage {
        platform: Platform::OneBot11,
        account_id: Some(event.self_id.as_str().to_owned()),
        conversation,
        actor: Actor {
            sender_id: Some(user_id),
            union_id: None,
            display_name: sender_display_name(event),
            group_member_role: (message_type == MessageType::Group)
                .then(|| sender_role(event))
                .flatten(),
            is_bot: false,
            source: IdentitySource::Event,
        },
        message_id,
        current_msg_idx: None,
        timestamp: event.time.map(|time| time.to_string()),
        text: parsed.text,
        input_parts: parsed.input_parts,
        attachments: Vec::new(),
        quoted: parsed.quoted,
        visible_entity_snapshot: None,
        mentions: parsed.mentions,
        mentioned_bot: parsed.mentioned_bot,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageType {
    Private,
    Group,
}

#[derive(Debug)]
struct ParsedSegments {
    text: String,
    input_parts: Vec<MessageInputPart>,
    mentions: Vec<MentionIdentity>,
    mentioned_bot: bool,
    quoted: Option<QuotedMessageContext>,
}

fn parse_segments(
    segments: &[MessageSegment],
    self_id: &str,
    message_id: &str,
    media_max_bytes: u64,
) -> ParsedSegments {
    let mut text = String::new();
    let mut input_parts = Vec::new();
    let mut mentions = Vec::new();
    let mut mentioned_bot = false;
    let mut quoted = None;

    for segment in segments {
        match segment.kind.as_str() {
            "text" => {
                let Some(value) = segment.data.get("text").and_then(Value::as_str) else {
                    continue;
                };
                text.push_str(value);
                push_text_part(&mut input_parts, value);
            }
            "at" => {
                let Some(target_id) = segment.data.get("qq").and_then(id_from_value) else {
                    continue;
                };
                let is_self = target_id == self_id;
                mentioned_bot |= is_self;
                mentions.push(mention_identity(target_id, is_self));
                // `at` 当前机器人只用于触发，普通 `at` 也由 mentions 表达；二者均不伪造成
                // MessageInputPart::Text，因此正文只保留平台原始 text segment 的顺序。
            }
            "reply" => {
                if quoted.is_none() {
                    quoted = quoted_from_segment(segment, message_id);
                }
            }
            "image" => {
                input_parts.push(media_part(segment, OneBotMediaKind::Image, media_max_bytes))
            }
            "file" => input_parts.push(media_part(segment, OneBotMediaKind::File, media_max_bytes)),
            _ => {
                // 未知 segment 只保留脱敏媒体占位，不复制原始 payload。这样整条消息仍可
                // 处理，模型也不会被告知已读取未知附件内容。
                input_parts.push(MessageInputPart::unknown(
                    MessageMedia {
                        platform: Some(Platform::OneBot11.as_str().to_owned()),
                        status: MediaStatus::UnsupportedType,
                        ..Default::default()
                    },
                    "unsupported_onebot_segment",
                ));
            }
        }
    }

    ParsedSegments {
        text,
        input_parts,
        mentions,
        mentioned_bot,
        quoted,
    }
}

#[derive(Debug, Clone, Copy)]
enum OneBotMediaKind {
    Image,
    File,
}

fn push_text_part(parts: &mut Vec<MessageInputPart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(MessageInputPart::Text { text, .. }) = parts.last_mut() {
        text.push_str(value);
    } else {
        parts.push(MessageInputPart::Text {
            text: value.to_owned(),
            source: Some(TextSource::Body),
        });
    }
}

fn quoted_from_segment(
    segment: &MessageSegment,
    current_message_id: &str,
) -> Option<QuotedMessageContext> {
    let reference_id = segment.data.get("id").and_then(id_from_value)?;
    let text_summary = clean_data_string(&segment.data, &["text", "content"]);
    let input_parts = text_summary
        .as_ref()
        .map(|text| vec![MessageInputPart::text(text.clone())])
        .unwrap_or_default();
    let sender = clean_data_id(&segment.data, &["user_id", "sender_id"]).map(|user_id| {
        MessageActorContext {
            user_id: Some(user_id),
            source: IdentitySource::Event,
            ..Default::default()
        }
    });
    Some(QuotedMessageContext {
        current_message_id: Some(current_message_id.to_owned()),
        // OneBot reply.id 是平台 message_id；不能写进 QQ 专属 ref_msg_idx。
        reference_id: Some(reference_id),
        text_summary,
        input_parts,
        sender,
        fallback_reason: Some("pending_ref_index_lookup".to_owned()),
        ..Default::default()
    })
}

fn media_part(
    segment: &MessageSegment,
    kind: OneBotMediaKind,
    media_max_bytes: u64,
) -> MessageInputPart {
    let raw_file = clean_data_string(&segment.data, &["file"]);
    let explicit_url = clean_data_string(&segment.data, &["url"]);
    let url = explicit_url
        .as_deref()
        .and_then(safe_remote_url)
        .or_else(|| raw_file.as_deref().and_then(safe_remote_url));
    let filename = clean_data_string(&segment.data, &["name", "file_name", "filename"])
        .as_deref()
        .and_then(safe_filename)
        .or_else(|| raw_file.as_deref().and_then(safe_filename));
    let size_bytes = clean_data_u64(&segment.data, &["size", "file_size"]);
    let mime_type = clean_data_string(&segment.data, &["mime", "mime_type", "content_type"])
        .as_deref()
        .and_then(safe_mime_type)
        .or_else(|| infer_image_mime(filename.as_deref(), kind));
    let file_id = clean_data_id(&segment.data, &["file_id"])
        .as_deref()
        .and_then(safe_opaque_reference)
        .or_else(|| raw_file.as_deref().and_then(safe_opaque_reference));
    let media_id = clean_data_id(&segment.data, &["media_id", "image_id"])
        .as_deref()
        .and_then(safe_opaque_reference);
    let status = if size_bytes.is_some_and(|size| size > media_max_bytes) {
        MediaStatus::SizeExceeded
    } else if let Some(status) = explicit_media_status(&segment.data) {
        status
    } else if url.is_some() {
        MediaStatus::Available
    } else {
        MediaStatus::MissingReadableUrl
    };
    let media = MessageMedia {
        mime_type,
        filename,
        size_bytes,
        url,
        // OneBot 的 file 字段可能是客户端本机路径；一期不信任也不保存该路径。
        local_path: None,
        media_id,
        file_id,
        attachment_id: None,
        platform: Some(Platform::OneBot11.as_str().to_owned()),
        status,
    };
    match kind {
        OneBotMediaKind::Image => MessageInputPart::image(media),
        OneBotMediaKind::File => MessageInputPart::file(media),
    }
}

fn mention_identity(target_id: String, is_self: bool) -> MentionIdentity {
    let is_all = target_id == "all";
    MentionIdentity {
        raw_text: if is_self {
            Some("@当前机器人".to_owned())
        } else if is_all {
            Some("@全体成员".to_owned())
        } else {
            None
        },
        target: MessageActorContext {
            user_id: (!is_all).then_some(target_id),
            display_name: is_all.then(|| "全体成员".to_owned()),
            display_name_source: is_all.then(|| "event".to_owned()),
            is_bot: is_self.then_some(true),
            source: IdentitySource::Event,
            ..Default::default()
        },
        is_self,
        confidence: MentionConfidence::Event,
    }
}

fn event_id(event: &OneBotEvent, field: &str) -> Option<String> {
    event.extra.get(field).and_then(id_from_value)
}

fn sender(event: &OneBotEvent) -> Option<&Map<String, Value>> {
    event.extra.get("sender").and_then(Value::as_object)
}

fn sender_id(event: &OneBotEvent) -> Option<String> {
    sender(event)?.get("user_id").and_then(id_from_value)
}

fn sender_display_name(event: &OneBotEvent) -> Option<String> {
    let sender = sender(event)?;
    ["card", "nickname"]
        .into_iter()
        .filter_map(|field| sender.get(field).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

fn sender_role(event: &OneBotEvent) -> Option<GroupMemberRoleKind> {
    let role = sender(event)?.get("role")?.as_str()?.trim();
    if role.is_empty() {
        return None;
    }
    Some(match role {
        "owner" => GroupMemberRoleKind::Owner,
        "admin" => GroupMemberRoleKind::Admin,
        "member" => GroupMemberRoleKind::Member,
        _ => GroupMemberRoleKind::Unknown,
    })
}

fn id_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        }
        Value::Number(value) if value.is_i64() || value.is_u64() => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests;

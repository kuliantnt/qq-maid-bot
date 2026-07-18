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
                && !command_prefix.is_candidate(&parsed.text)
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
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use super::*;
    use crate::gateway::{
        dedupe::MessageDedupe,
        platform::{core_scope_key, render_text_for_core},
    };

    fn event(value: Value) -> OneBotEvent {
        serde_json::from_value(value).unwrap()
    }

    fn message(outcome: OneBotInboundOutcome) -> InboundMessage {
        let OneBotInboundOutcome::Message(message) = outcome else {
            panic!("expected adapted message, got {outcome:?}");
        };
        *message
    }

    fn ignored(outcome: OneBotInboundOutcome) -> OneBotIgnoreReason {
        let OneBotInboundOutcome::Ignored(reason) = outcome else {
            panic!("expected ignored event, got {outcome:?}");
        };
        reason
    }

    fn private_event(self_id: Value, user_id: Value, message_id: Value) -> OneBotEvent {
        event(json!({
            "time": 1720000000,
            "self_id": self_id,
            "post_type": "message",
            "message_type": "private",
            "user_id": user_id,
            "message_id": message_id,
            "sender": {"nickname": "测试用户"},
            "message": [
                {"type": "text", "data": {"text": "你好"}},
                {"type": "text", "data": {"text": "，世界"}}
            ]
        }))
    }

    fn group_event(message: Value) -> OneBotEvent {
        event(json!({
            "time": 1720000001,
            "self_id": "10001",
            "post_type": "message",
            "message_type": "group",
            "user_id": "20002",
            "group_id": "30003",
            "message_id": "40004",
            "sender": {"card": "群名片", "nickname": "昵称", "role": "admin"},
            "message": message
        }))
    }

    #[test]
    fn private_text_accepts_numeric_and_string_ids() {
        let cases = [
            (json!(10001), json!(20002), json!(30003)),
            (json!("10001"), json!("20002"), json!("30003")),
        ];

        for (self_id, user_id, message_id) in cases {
            let inbound = message(inbound_from_event(&private_event(
                self_id, user_id, message_id,
            )));
            assert_eq!(inbound.platform, Platform::OneBot11);
            assert_eq!(inbound.account_id.as_deref(), Some("10001"));
            assert_eq!(
                inbound.conversation,
                ConversationTarget::Private {
                    target_id: "20002".to_owned()
                }
            );
            assert_eq!(inbound.actor.sender_id.as_deref(), Some("20002"));
            assert_eq!(inbound.actor.display_name.as_deref(), Some("测试用户"));
            assert_eq!(inbound.message_id, "30003");
            assert_eq!(inbound.timestamp.as_deref(), Some("1720000000"));
            assert_eq!(inbound.text, "你好，世界");
            assert_eq!(
                inbound
                    .input_parts
                    .iter()
                    .filter_map(MessageInputPart::text_content)
                    .collect::<Vec<_>>(),
                vec!["你好，世界"]
            );
            assert_eq!(render_text_for_core(&inbound), inbound.text);
            assert_eq!(
                core_scope_key(&inbound).unwrap(),
                "platform:onebot:account:10001:private:20002"
            );
        }
    }

    #[test]
    fn group_trigger_table_distinguishes_self_at_other_at_and_self_message() {
        let cases = [
            (
                "at current bot",
                group_event(json!([
                    {"type": "at", "data": {"qq": 10001}},
                    {"type": "text", "data": {"text": " 请帮忙"}}
                ])),
                None,
            ),
            (
                "not triggered",
                group_event(json!([{"type": "text", "data": {"text": "路过"}}])),
                Some(OneBotIgnoreReason::GroupNotTriggered),
            ),
            (
                "at another member",
                group_event(json!([
                    {"type": "at", "data": {"qq": "90009"}},
                    {"type": "text", "data": {"text": " 看一下"}}
                ])),
                Some(OneBotIgnoreReason::GroupNotTriggered),
            ),
            (
                "self message",
                event(json!({
                    "self_id": "10001",
                    "post_type": "message",
                    "message_type": "group",
                    "user_id": "10001",
                    "group_id": "30003",
                    "message_id": "40004",
                    "message": [{"type": "at", "data": {"qq": "10001"}}]
                })),
                Some(OneBotIgnoreReason::SelfMessage),
            ),
        ];

        for (name, event, expected_ignored) in cases {
            let outcome = inbound_from_event(&event);
            match expected_ignored {
                Some(reason) => assert_eq!(ignored(outcome), reason, "{name}"),
                None => {
                    let inbound = message(outcome);
                    assert_eq!(
                        inbound.conversation,
                        ConversationTarget::Group {
                            target_id: "30003".to_owned()
                        },
                        "{name}"
                    );
                    assert!(inbound.mentioned_bot, "{name}");
                    assert_eq!(inbound.text, " 请帮忙", "{name}");
                    assert_eq!(
                        inbound.actor.group_member_role,
                        Some(GroupMemberRoleKind::Admin),
                        "{name}"
                    );
                }
            }
        }
    }

    #[test]
    fn direct_group_slash_candidate_without_at_preserves_core_context() {
        let inbound = message(inbound_from_event(&group_event(json!([
            {"type": "text", "data": {"text": " /memory list"}}
        ]))));

        assert_eq!(inbound.text, " /memory list");
        assert!(!inbound.mentioned_bot);
        assert_eq!(inbound.message_id, "40004");
        assert_eq!(inbound.actor.sender_id.as_deref(), Some("20002"));
        assert_eq!(
            inbound.actor.group_member_role,
            Some(GroupMemberRoleKind::Admin)
        );
        assert_eq!(
            inbound.conversation,
            ConversationTarget::Group {
                target_id: "30003".to_owned()
            }
        );
    }

    #[test]
    fn custom_prefix_controls_direct_group_command_candidates() {
        let prefix = CommandPrefix::parse("#").unwrap();

        let custom = inbound_from_event_with_media_limit(
            &group_event(json!([
                {"type": "text", "data": {"text": " #help"}}
            ])),
            crate::config::DEFAULT_MEDIA_MAX_BYTES,
            prefix,
        );
        assert_eq!(message(custom).text, " #help");

        for text in ["/help", "##help"] {
            let outcome = inbound_from_event_with_media_limit(
                &group_event(json!([
                    {"type": "text", "data": {"text": text}}
                ])),
                crate::config::DEFAULT_MEDIA_MAX_BYTES,
                prefix,
            );
            assert_eq!(ignored(outcome), OneBotIgnoreReason::GroupNotTriggered);
        }
    }

    #[test]
    fn structured_bot_at_keeps_custom_prefix_group_trigger() {
        let inbound = message(inbound_from_event_with_media_limit(
            &group_event(json!([
                {"type": "at", "data": {"qq": "10001"}},
                {"type": "text", "data": {"text": " #help"}}
            ])),
            crate::config::DEFAULT_MEDIA_MAX_BYTES,
            CommandPrefix::parse("#").unwrap(),
        ));

        assert!(inbound.mentioned_bot);
        assert_eq!(inbound.text, " #help");
    }

    #[test]
    fn removes_only_trigger_at_and_preserves_ordered_text_and_mentions() {
        let inbound = message(inbound_from_event(&group_event(json!([
            {"type": "text", "data": {"text": "请"}},
            {"type": "at", "data": {"qq": "10001"}},
            {"type": "text", "data": {"text": "帮"}},
            {"type": "at", "data": {"qq": 90009}},
            {"type": "text", "data": {"text": "看看"}}
        ]))));

        assert_eq!(inbound.text, "请帮看看");
        assert_eq!(
            inbound
                .input_parts
                .iter()
                .filter_map(MessageInputPart::text_content)
                .collect::<Vec<_>>(),
            vec!["请帮看看"]
        );
        assert_eq!(render_text_for_core(&inbound), inbound.text);
        assert_eq!(inbound.mentions.len(), 2);
        assert!(inbound.mentions[0].is_self);
        assert_eq!(inbound.mentions[0].target.user_id.as_deref(), Some("10001"));
        assert!(!inbound.mentions[1].is_self);
        assert_eq!(inbound.mentions[1].target.user_id.as_deref(), Some("90009"));
        assert_eq!(inbound.mentions[1].target.display_name, None);
        assert_eq!(inbound.mentions[1].target.is_bot, None);
    }

    #[test]
    fn sender_role_table_maps_known_values_and_marks_unknown_value() {
        let cases = [
            ("owner", GroupMemberRoleKind::Owner),
            ("admin", GroupMemberRoleKind::Admin),
            ("member", GroupMemberRoleKind::Member),
            ("future_role", GroupMemberRoleKind::Unknown),
        ];

        for (role, expected) in cases {
            let inbound = message(inbound_from_event(&event(json!({
                "self_id": "10001",
                "post_type": "message",
                "message_type": "group",
                "user_id": "20002",
                "group_id": "30003",
                "message_id": role,
                "sender": {"role": role},
                "message": [{"type": "at", "data": {"qq": "10001"}}]
            }))));
            assert_eq!(inbound.actor.group_member_role, Some(expected), "{role}");
        }
    }

    #[test]
    fn empty_text_and_unknown_segment_degrade_without_dropping_message() {
        let empty = message(inbound_from_event(&event(json!({
            "self_id": "10001",
            "post_type": "message",
            "message_type": "private",
            "user_id": "20002",
            "message_id": "empty",
            "message": [{"type": "text", "data": {"text": ""}}]
        }))));
        assert!(empty.text.is_empty());
        assert!(empty.input_parts.is_empty());

        let unknown = message(inbound_from_event(&event(json!({
            "self_id": "10001",
            "post_type": "message",
            "message_type": "private",
            "user_id": "20002",
            "message_id": "unknown",
            "message": [
                {"type": "future_segment", "data": {"anything": {"nested": true}}},
                {"type": "text", "data": {"text": "仍可处理"}}
            ]
        }))));
        assert_eq!(unknown.text, "仍可处理");
        assert_eq!(unknown.input_parts.len(), 2);
        assert!(matches!(
            unknown.input_parts[0],
            MessageInputPart::Unknown {
                media: MessageMedia {
                    status: MediaStatus::UnsupportedType,
                    ..
                },
                ..
            }
        ));
        assert_eq!(unknown.input_parts[1].text_content(), Some("仍可处理"));
    }

    #[test]
    fn reply_segment_maps_platform_message_id_without_qq_refidx_fields() {
        for reply_id in [json!(123456789), json!("123456789")] {
            let inbound = message(inbound_from_event(&event(json!({
                "self_id": "10001",
                "post_type": "message",
                "message_type": "private",
                "user_id": "20002",
                "message_id": "current-1",
                "message": [
                    {"type": "reply", "data": {
                        "id": reply_id,
                        "text": "事件自带引用正文",
                        "user_id": 30003
                    }},
                    {"type": "text", "data": {"text": "继续"}}
                ]
            }))));

            let quoted = inbound.quoted.expect("reply should create quoted context");
            assert_eq!(quoted.current_message_id.as_deref(), Some("current-1"));
            assert_eq!(quoted.reference_id.as_deref(), Some("123456789"));
            assert_eq!(quoted.current_msg_idx, None);
            assert_eq!(quoted.ref_msg_idx, None);
            assert_eq!(quoted.text_summary.as_deref(), Some("事件自带引用正文"));
            assert_eq!(
                quoted.input_parts[0].text_content(),
                Some("事件自带引用正文")
            );
            assert_eq!(
                quoted.sender.and_then(|sender| sender.user_id),
                Some("30003".to_owned())
            );
        }
    }

    #[test]
    fn text_image_and_file_segments_preserve_order_and_safe_metadata() {
        let inbound = message(inbound_from_event(&event(json!({
            "self_id": "10001",
            "post_type": "message",
            "message_type": "private",
            "user_id": "20002",
            "message_id": "media-1",
            "message": [
                {"type": "text", "data": {"text": "前"}},
                {"type": "image", "data": {
                    "file": "photo.png",
                    "url": "https://example.test/photo.png?token=secret",
                    "size": "1024",
                    "image_id": "image-1"
                }},
                {"type": "text", "data": {"text": "中"}},
                {"type": "file", "data": {
                    "file_id": 9988,
                    "name": "report.pdf",
                    "size": 2048,
                    "mime_type": "application/pdf"
                }},
                {"type": "text", "data": {"text": "后"}}
            ]
        }))));

        assert_eq!(inbound.text, "前中后");
        assert_eq!(inbound.input_parts.len(), 5);
        assert_eq!(inbound.input_parts[0].text_content(), Some("前"));
        let MessageInputPart::Image { media: image } = &inbound.input_parts[1] else {
            panic!("expected image part");
        };
        assert_eq!(image.filename.as_deref(), Some("photo.png"));
        assert_eq!(image.mime_type.as_deref(), Some("image/png"));
        assert_eq!(image.size_bytes, Some(1024));
        assert_eq!(
            image.remote_url(),
            Some("https://example.test/photo.png?token=secret")
        );
        assert_eq!(image.media_id.as_deref(), Some("image-1"));
        assert_eq!(image.status, MediaStatus::Available);
        assert_eq!(inbound.input_parts[2].text_content(), Some("中"));
        let MessageInputPart::File { media: file } = &inbound.input_parts[3] else {
            panic!("expected file part");
        };
        assert_eq!(file.filename.as_deref(), Some("report.pdf"));
        assert_eq!(file.mime_type.as_deref(), Some("application/pdf"));
        assert_eq!(file.file_id.as_deref(), Some("9988"));
        assert_eq!(file.status, MediaStatus::MissingReadableUrl);
        assert_eq!(inbound.input_parts[4].text_content(), Some("后"));
    }

    #[test]
    fn unsafe_local_base64_and_oversized_media_degrade_without_leaking_paths() {
        let inbound = message(inbound_from_event_with_media_limit(
            &event(json!({
                "self_id": "10001",
                "post_type": "message",
                "message_type": "private",
                "user_id": "20002",
                "message_id": "media-unsafe",
                "message": [
                    {"type": "image", "data": {
                        "file": "C:\\Users\\someone\\secret.png",
                        "url": "file:///C:/Users/someone/secret.png",
                        "name": "C:\\Users\\someone\\secret.png"
                    }},
                    {"type": "image", "data": {"file": "base64://abcdef"}},
                    {"type": "image", "data": {
                        "file": "large.jpg",
                        "url": "https://example.test/large.jpg",
                        "size": 11
                    }}
                ]
            })),
            10,
            CommandPrefix::default(),
        ));

        for part in &inbound.input_parts[..2] {
            let MessageInputPart::Image { media } = part else {
                panic!("expected image part");
            };
            assert_eq!(media.url, None);
            assert_eq!(media.local_path, None);
            assert_eq!(media.filename, None);
            assert_eq!(media.file_id, None);
            assert_eq!(media.status, MediaStatus::MissingReadableUrl);
            assert!(!part.fallback_text().contains("Users"));
            assert!(!part.fallback_text().contains("base64"));
        }
        let MessageInputPart::Image { media: oversized } = &inbound.input_parts[2] else {
            panic!("expected oversized image part");
        };
        assert_eq!(oversized.status, MediaStatus::SizeExceeded);
        assert_eq!(oversized.size_bytes, Some(11));
    }

    #[test]
    fn media_failure_extensions_map_to_real_fallback_statuses() {
        let inbound = message(inbound_from_event(&event(json!({
            "self_id": "10001",
            "post_type": "message",
            "message_type": "private",
            "user_id": "20002",
            "message_id": "media-status",
            "message": [
                {"type": "image", "data": {
                    "url": "https://example.test/expired.jpg",
                    "status": "expired"
                }},
                {"type": "file", "data": {
                    "name": "failed.pdf",
                    "download_status": "download_failed"
                }}
            ]
        }))));

        assert_eq!(
            inbound.input_parts[0].media().unwrap().status,
            MediaStatus::Expired
        );
        assert_eq!(
            inbound.input_parts[1].media().unwrap().status,
            MediaStatus::DownloadFailed
        );
        assert!(render_text_for_core(&inbound).contains("[图片"));
        assert!(render_text_for_core(&inbound).contains("[文件"));
    }

    #[test]
    fn unknown_events_message_sent_and_cq_strings_are_safely_ignored() {
        let cases = [
            (
                event(json!({
                    "self_id": "10001",
                    "post_type": "notice",
                    "notice_type": "group_recall"
                })),
                OneBotIgnoreReason::NonMessageEvent,
            ),
            (
                event(json!({
                    "self_id": "10001",
                    "post_type": "message_sent",
                    "message_type": "private",
                    "user_id": "20002",
                    "message_id": "sent",
                    "message": [{"type": "text", "data": {"text": "echo"}}]
                })),
                OneBotIgnoreReason::MessageSent,
            ),
            (
                event(json!({
                    "self_id": "10001",
                    "post_type": "message",
                    "message_type": "private",
                    "user_id": "20002",
                    "message_id": "cq",
                    "message": "hello[CQ:at,qq=10001]"
                })),
                OneBotIgnoreReason::UnsupportedMessageEncoding,
            ),
        ];

        for (event, reason) in cases {
            assert_eq!(ignored(inbound_from_event(&event)), reason);
        }
    }

    #[test]
    fn dedupe_key_is_stable_for_duplicates_and_isolated_by_account_and_conversation() {
        let base = message(inbound_from_event(&private_event(
            json!(10001),
            json!(20002),
            json!(30003),
        )));
        let duplicate = message(inbound_from_event(&private_event(
            json!("10001"),
            json!("20002"),
            json!("30003"),
        )));
        let other_account = message(inbound_from_event(&private_event(
            json!(10002),
            json!(20002),
            json!(30003),
        )));
        let group = message(inbound_from_event(&event(json!({
            "self_id": "10001",
            "post_type": "message",
            "message_type": "group",
            "user_id": "20002",
            "group_id": "90009",
            "message_id": "30003",
            "message": [{"type": "at", "data": {"qq": "10001"}}]
        }))));
        let other_group = message(inbound_from_event(&event(json!({
            "self_id": "10001",
            "post_type": "message",
            "message_type": "group",
            "user_id": "20002",
            "group_id": "90010",
            "message_id": "30003",
            "message": [{"type": "at", "data": {"qq": "10001"}}]
        }))));

        let base_key = base.dedupe_message_key().unwrap();
        assert_eq!(
            duplicate.dedupe_message_key().as_deref(),
            Some(base_key.as_str())
        );
        assert_ne!(
            other_account.dedupe_message_key().as_deref(),
            Some(base_key.as_str())
        );
        assert_ne!(
            group.dedupe_message_key().as_deref(),
            Some(base_key.as_str())
        );
        assert_ne!(other_group.dedupe_message_key(), group.dedupe_message_key());

        let dedupe = MessageDedupe::new(Duration::from_secs(10));
        let now = Instant::now();
        assert!(!dedupe.check_and_insert_many([base_key.clone()], now));
        assert!(dedupe.check_and_insert_many([base_key], now));
        assert!(!dedupe.check_and_insert_many([other_account.dedupe_message_key().unwrap()], now));
        assert!(!dedupe.check_and_insert_many([group.dedupe_message_key().unwrap()], now));
        assert!(!dedupe.check_and_insert_many([other_group.dedupe_message_key().unwrap()], now));
    }
}

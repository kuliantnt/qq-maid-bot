//! OneBot 11 消息事件到统一入站模型的 adapter。
//!
//! 本模块只处理一期文本、结构化 `at` 与触发语义。CQ 字符串、媒体、引用和业务回复
//! 分别由后续任务处理，不能把 OneBot 原始字段泄漏到 Core。

use qq_maid_common::{
    identity_context::{IdentitySource, MentionConfidence, MentionIdentity, MessageActorContext},
    input_part::MessageInputPart,
};
use serde_json::{Map, Value};

use crate::gateway::onebot11::protocol::{MessageSegment, OneBotEvent, OneBotMessage};

use super::model::{Actor, ConversationTarget, GroupMemberRoleKind, InboundMessage, Platform};

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
/// 一期群聊只接受明确 `at` 当前 `self_id` 的消息；当前账号自己发送的 `message` 和
/// `message_sent` 均被过滤，避免后续聊天闭环形成回声循环。
pub(crate) fn inbound_from_event(event: &OneBotEvent) -> OneBotInboundOutcome {
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

    let parsed = parse_segments(segments, event.self_id.as_str());
    let conversation = match message_type {
        MessageType::Private => ConversationTarget::Private {
            target_id: user_id.clone(),
        },
        MessageType::Group => {
            if !parsed.mentioned_bot {
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
        quoted: None,
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
}

fn parse_segments(segments: &[MessageSegment], self_id: &str) -> ParsedSegments {
    let mut text = String::new();
    let mut mentions = Vec::new();
    let mut mentioned_bot = false;

    for segment in segments {
        match segment.kind.as_str() {
            "text" => {
                let Some(value) = segment.data.get("text").and_then(Value::as_str) else {
                    continue;
                };
                text.push_str(value);
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
            _ => {
                // 未知 segment 仅降级忽略当前段，不能导致整条文本消息反序列化失败。
            }
        }
    }

    // OneBot 相邻 text segment 在协议上共同组成一段正文；合并为单一 input part，
    // 避免通用 Core renderer 将多个 part 用换行连接后改变原文。
    let input_parts = if text.is_empty() {
        Vec::new()
    } else {
        vec![MessageInputPart::text(text.clone())]
    };
    ParsedSegments {
        text,
        input_parts,
        mentions,
        mentioned_bot,
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
        assert_eq!(unknown.input_parts.len(), 1);
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

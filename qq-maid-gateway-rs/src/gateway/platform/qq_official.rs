//! QQ 官方 Gateway 协议到统一入站模型的 adapter。
//!
//! QQ 专用结构、mention 判定、附件和 reply 字段转换都收口在这里，避免污染平台无关模型。

use qq_maid_common::identity_context::{
    IdentitySource, MentionConfidence, MentionIdentity, MessageActorContext,
};

use super::model::{
    Actor, Attachment, ConversationTarget, GroupMemberRoleKind, InboundMessage, Platform,
};
use crate::gateway::event::{
    Attachment as QqAttachment, C2cMessage, GroupEventType, GroupMemberRole, GroupMention,
    GroupMessage, MessageReply,
};
use qq_maid_core::service::CoreGroupMemberRole;

pub(crate) fn inbound_from_c2c(message: &C2cMessage) -> InboundMessage {
    InboundMessage {
        platform: Platform::QqOfficial,
        account_id: None,
        conversation: ConversationTarget::Private {
            target_id: message.user_openid.clone(),
        },
        actor: Actor {
            sender_id: Some(message.user_openid.clone()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            source: IdentitySource::Event,
        },
        message_id: message.message_id.clone(),
        current_msg_idx: message.current_msg_idx.clone(),
        timestamp: message.timestamp.clone(),
        text: message.content.clone(),
        input_parts: message.input_parts.clone(),
        attachments: message.attachments.iter().map(attachment_from_qq).collect(),
        quoted: message.reply.as_ref().map(|reply| {
            quoted_from_qq(
                reply,
                &message.message_id,
                message.current_msg_idx.as_deref(),
            )
        }),
        visible_entity_snapshot: None,
        mentions: Vec::new(),
        mentioned_bot: false,
    }
}

pub(crate) fn inbound_from_group(message: &GroupMessage) -> InboundMessage {
    InboundMessage {
        platform: Platform::QqOfficial,
        account_id: None,
        conversation: ConversationTarget::Group {
            target_id: message.group_openid.clone(),
        },
        actor: Actor {
            sender_id: message.member_openid.clone(),
            union_id: None,
            display_name: None,
            group_member_role: message.member_role.map(GroupMemberRoleKind::from),
            is_bot: message.author_is_bot || message.author_is_self,
            source: IdentitySource::Event,
        },
        message_id: message.message_id.clone(),
        current_msg_idx: message.current_msg_idx.clone(),
        timestamp: message.timestamp.clone(),
        text: message.content.clone(),
        input_parts: message.input_parts.clone(),
        attachments: message.attachments.iter().map(attachment_from_qq).collect(),
        quoted: message.reply.as_ref().map(|reply| {
            quoted_from_qq(
                reply,
                &message.message_id,
                message.current_msg_idx.as_deref(),
            )
        }),
        visible_entity_snapshot: None,
        mentions: mentions_from_group_event(message),
        mentioned_bot: message.event_type == GroupEventType::GroupAtMessage
            || message.mentions.iter().any(|mention| mention.is_you),
    }
}

/// 把群事件的结构化 mentions 映射为平台无关 `MentionIdentity`。
///
/// - `mentions[]` 中每个条目按事件顺序映射：`is_you` -> `is_self`，`target_id` -> 稳定 ID，
///   `member_role` -> 角色字符串，`confidence = Event`。
/// - `is_self` 只来自平台结构化 `is_you` 字段，不因 `GROUP_AT_MESSAGE_CREATE` 事件类型
///   把普通 mention 误标为机器人。
/// - `GROUP_AT_MESSAGE_CREATE` 事件整体即代表 @ 当前机器人；若遍历完 mentions 仍未看到
///   任何 `is_you=true` 条目，才追加一条 synthetic self mention，避免与结构化普通 mention 混淆。
/// - 非 `is_you` 的 mention `is_bot` 保持 None（事件不提供），不伪造。
/// - 文本 `@昵称` 弱候选由 `text_weak_mentions_from_content` 补充，采用保守去重策略。
fn mentions_from_group_event(message: &GroupMessage) -> Vec<MentionIdentity> {
    let mut result = Vec::new();
    let has_self_event = message.event_type == GroupEventType::GroupAtMessage;
    let mut saw_self = false;
    for mention in &message.mentions {
        // is_self 只来自平台结构化 is_you；不因事件类型把普通 mention 强制标为 self。
        let is_self = mention.is_you;
        if is_self {
            saw_self = true;
        }
        result.push(mention_identity_from_group_mention(mention, is_self));
    }
    // GROUP_AT_MESSAGE_CREATE 整体即 @ 当前机器人；仅当结构化 mentions 中无任何 is_you=true 时
    // 才追加 synthetic self mention，不与普通 mention 混淆。
    if has_self_event && !saw_self {
        result.push(self_mention());
    }
    // 文本 @ 昵称弱候选：保守去重，避免与结构化 mention 拆成两个独立对象。
    result.extend(text_weak_mentions_from_content(&message.content, &result));
    result
}

fn mention_identity_from_group_mention(mention: &GroupMention, is_self: bool) -> MentionIdentity {
    MentionIdentity {
        raw_text: is_self.then(|| "@当前机器人".to_owned()),
        target: MessageActorContext {
            user_id: mention.target_id.clone(),
            union_id: None,
            display_name: None,
            display_name_source: None,
            group_member_role: mention.member_role.map(|role| {
                CoreGroupMemberRole::from(GroupMemberRoleKind::from(role))
                    .as_str()
                    .to_owned()
            }),
            // 仅 @ 当前机器人时可确定 is_bot=true；其余 mention 事件不提供，保持 None。
            is_bot: is_self.then_some(true),
            source: IdentitySource::Event,
        },
        is_self,
        confidence: MentionConfidence::Event,
    }
}

fn self_mention() -> MentionIdentity {
    MentionIdentity {
        raw_text: Some("@当前机器人".to_owned()),
        target: MessageActorContext {
            is_bot: Some(true),
            source: IdentitySource::Event,
            ..Default::default()
        },
        is_self: true,
        confidence: MentionConfidence::Event,
    }
}

/// 从消息文本中提取未被结构化 mention 覆盖的 `@昵称` 弱候选。
///
/// # 保守去重策略
///
/// QQ 结构化 `mentions[]` 不携带昵称文本，因此无法安全判断正文中的 `@昵称`
/// 是否与某条结构化 mention 指向同一人。为避免把同一个被 @ 对象拆成
/// “一个 Event 稳定身份 + 一个 TextWeak 昵称身份”两条独立 mention（会让 LLM 误以为是两个人），
/// 采用保守策略：
///
/// - 当存在任意结构化普通 mention（非 self）时，不再从文本生成 TextWeak 候选。
///   未被结构化覆盖的真正独立 @对象作为弱信号丢失，可接受；待 Phase 3 接入 #229
///   成员详情后由 display_name 补全。
/// - 仅当没有结构化普通 mention（只有 self 或完全无结构化 mention）时，才为正文里
///   未被 self 的 `@当前机器人` 占用的 `@昵称` 生成 TextWeak 候选。
///
/// 生成的弱候选只有 `display_name`，不伪造稳定 ID，`confidence = TextWeak`。
fn text_weak_mentions_from_content(
    content: &str,
    structured: &[MentionIdentity],
) -> Vec<MentionIdentity> {
    // 存在结构化普通 mention 时直接抑制 TextWeak，避免同对象被拆成两条独立 mention。
    let has_structured_non_self = structured.iter().any(|mention| !mention.is_self);
    if has_structured_non_self {
        return Vec::new();
    }
    let occupied: Vec<String> = structured
        .iter()
        .filter_map(|mention| mention.raw_text.clone())
        .collect();
    let mut seen: Vec<String> = Vec::new();
    let mut result = Vec::new();
    let chars: Vec<char> = content.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let is_at = chars[index] == '@';
        let at_boundary = index == 0 || is_text_weak_boundary(chars[index - 1]);
        if !(is_at && at_boundary) {
            index += 1;
            continue;
        }
        // 收集 @ 后的昵称 token：遇空白或终止标点即停。
        let mut end = index + 1;
        while end < chars.len() && !is_text_weak_terminator(chars[end]) {
            end += 1;
        }
        let token: String = chars[index + 1..end].iter().collect();
        let raw = format!("@{token}");
        index = end;
        if token.trim().is_empty() || token.chars().count() > 24 {
            continue;
        }
        if occupied.iter().any(|item| item == &raw) || seen.iter().any(|item| item == &raw) {
            continue;
        }
        seen.push(raw.clone());
        result.push(MentionIdentity {
            raw_text: Some(raw),
            target: MessageActorContext {
                display_name: Some(token),
                source: IdentitySource::TextWeak,
                ..Default::default()
            },
            is_self: false,
            confidence: MentionConfidence::TextWeak,
        });
    }
    result
}

/// `@` 前一个字符是否构成边界（空白或常见中英文标点）。
fn is_text_weak_boundary(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '，' | '。' | '！' | '？' | '、' | '；' | '：' | ',' | '.' | '!' | '?'
        )
}

/// 昵称 token 的终止字符（空白或常见中英文标点 / 括号 / CQ 码分隔符）。
fn is_text_weak_terminator(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '，' | '。'
                | '！'
                | '？'
                | '、'
                | '；'
                | '：'
                | '['
                | ']'
                | '【'
                | '】'
                | '('
                | ')'
                | '（'
                | '）'
                | '<'
                | '>'
                | '\n'
                | '\r'
        )
}

fn attachment_from_qq(value: &QqAttachment) -> Attachment {
    Attachment {
        content_type: value.content_type.clone(),
        filename: value.filename.clone(),
        url: value.url.clone(),
        size_bytes: value.size_bytes,
        media_id: value.media_id.clone(),
        file_id: value.file_id.clone(),
        attachment_id: value.attachment_id.clone(),
        placeholder: None,
    }
}

fn quoted_from_qq(
    value: &MessageReply,
    current_message_id: &str,
    current_msg_idx: Option<&str>,
) -> qq_maid_common::input_part::QuotedMessageContext {
    qq_maid_common::input_part::QuotedMessageContext {
        current_message_id: Some(current_message_id.to_owned()),
        current_msg_idx: current_msg_idx.map(str::to_owned),
        reference_id: Some(value.message_id.clone()),
        // QQ 引用恢复只能使用官方下发的 ref_msg_idx/REFIDX；reply.message_id
        // 保留为原始引用字段，不伪造成 ref_index lookup key。
        ref_msg_idx: value.ref_msg_idx.clone(),
        text_summary: value.content.clone(),
        media_summaries: value.media_summaries.clone(),
        input_parts: value.input_parts.clone(),
        lookup_found: value.content.is_some()
            || !value.media_summaries.is_empty()
            || !value.input_parts.is_empty(),
        fallback_reason: (value.content.is_none()
            && value.media_summaries.is_empty()
            && value.input_parts.is_empty())
        .then(|| "pending_ref_index_lookup".to_owned()),
        ..Default::default()
    }
}

impl From<GroupMemberRole> for GroupMemberRoleKind {
    fn from(value: GroupMemberRole) -> Self {
        match value {
            GroupMemberRole::Owner => Self::Owner,
            GroupMemberRole::Admin => Self::Admin,
            GroupMemberRole::Member => Self::Member,
            GroupMemberRole::Unknown => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::event::{GroupMention, MessageReply};
    use qq_maid_core::service::{CoreConversation, CoreGroupMemberRole, Platform as CorePlatform};

    fn c2c_message() -> C2cMessage {
        C2cMessage {
            message_id: "msg-1".to_owned(),
            current_msg_idx: None,
            event_id: Some("event-1".to_owned()),
            source_message_ids: vec!["msg-1".to_owned()],
            source_event_ids: vec!["event-1".to_owned()],
            user_openid: "user-1".to_owned(),
            content: "你好".to_owned(),
            reply: None,
            timestamp: Some("2026-07-04T20:00:00+08:00".to_owned()),
            first_message_timestamp: Some("2026-07-04T20:00:00+08:00".to_owned()),
            last_message_timestamp: Some("2026-07-04T20:00:00+08:00".to_owned()),
            input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("你好")],
            attachments: Vec::new(),
        }
    }

    fn group_message() -> GroupMessage {
        GroupMessage {
            message_id: "group-msg-1".to_owned(),
            current_msg_idx: None,
            group_openid: "group-1".to_owned(),
            member_openid: Some("member-1".to_owned()),
            member_role: Some(GroupMemberRole::Admin),
            content: "/rss".to_owned(),
            mentions: vec![GroupMention {
                is_you: true,
                member_role: Some(GroupMemberRole::Admin),
                target_id: None,
            }],
            reply: None,
            timestamp: None,
            input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("/rss")],
            attachments: Vec::new(),
            event_type: GroupEventType::GroupMessage,
            author_is_bot: false,
            author_is_self: false,
        }
    }

    #[test]
    fn qq_c2c_maps_to_private_inbound_and_core_request() {
        let inbound = inbound_from_c2c(&c2c_message());
        let request = super::super::to_core_request(&inbound, inbound.text.clone()).unwrap();

        assert_eq!(inbound.platform, Platform::QqOfficial);
        assert_eq!(inbound.actor.sender_id.as_deref(), Some("user-1"));
        assert_eq!(
            super::super::core_scope_key(&inbound).unwrap(),
            "platform:qq_official:account:-:private:user-1"
        );
        assert_eq!(request.platform, CorePlatform::QqOfficial);
        assert_eq!(
            request.conversation,
            CoreConversation::Private {
                peer_id: "user-1".to_owned()
            }
        );
    }

    #[test]
    fn qq_group_maps_to_group_inbound_without_member_scope_split() {
        let inbound = inbound_from_group(&group_message());
        let request = super::super::to_core_request(&inbound, inbound.text.clone()).unwrap();

        assert_eq!(inbound.actor.sender_id.as_deref(), Some("member-1"));
        assert_eq!(
            inbound.actor.group_member_role,
            Some(GroupMemberRoleKind::Admin)
        );
        assert!(inbound.mentioned_bot);
        assert_eq!(inbound.mentions.len(), 1);
        assert!(inbound.mentions[0].is_self);
        assert_eq!(inbound.mentions[0].target.is_bot, Some(true));
        assert_eq!(
            super::super::core_scope_key(&inbound).unwrap(),
            "platform:qq_official:account:-:group:group-1"
        );
        assert_eq!(
            request.actor.group_member_role,
            Some(CoreGroupMemberRole::Admin)
        );
        let context = request.message_context();
        assert_eq!(
            context
                .actor
                .as_ref()
                .and_then(|actor| actor.user_id.as_deref()),
            Some("member-1")
        );
        assert_eq!(context.mentions.len(), 1);
        assert!(context.mentions[0].is_self);
        assert_eq!(
            request.conversation,
            CoreConversation::Group {
                group_id: "group-1".to_owned()
            }
        );
    }

    #[test]
    fn qq_adapter_converts_reply_and_attachment_metadata() {
        let mut message = c2c_message();
        message.reply = Some(MessageReply {
            message_id: "quoted-1".to_owned(),
            ref_msg_idx: None,
            content: Some("上一条".to_owned()),
            input_parts: Vec::new(),
            media_summaries: Vec::new(),
        });
        message.attachments = vec![QqAttachment {
            content_type: Some("image/jpeg".to_owned()),
            filename: Some("a.jpg".to_owned()),
            url: Some("https://example.test/a.jpg".to_owned()),
            size_bytes: None,
            media_id: None,
            file_id: None,
            attachment_id: None,
        }];
        message
            .input_parts
            .push(message.attachments[0].to_input_part("qq_official"));

        let inbound = inbound_from_c2c(&message);
        let rendered = super::super::render_text_for_core(&inbound);

        assert_eq!(
            inbound
                .quoted
                .as_ref()
                .and_then(|quote| quote.reference_id.as_deref()),
            Some("quoted-1")
        );
        assert_eq!(
            inbound
                .quoted
                .as_ref()
                .and_then(|quote| quote.ref_msg_idx.as_deref()),
            None
        );
        assert_eq!(inbound.attachments[0].filename.as_deref(), Some("a.jpg"));
        assert!(rendered.starts_with("你好"));
        assert!(rendered.contains("[图片 image/jpeg: a.jpg]"));
    }

    #[test]
    fn qq_group_multi_mention_maps_to_structured_identity_in_order() {
        // 一条消息同时 @ 当前机器人（is_you）和 @ 普通成员（结构化），顺序与事件一致。
        let mut message = group_message();
        message.event_type = GroupEventType::GroupMessage;
        message.content = "@当前机器人 帮我问一下".to_owned();
        message.mentions = vec![
            GroupMention {
                is_you: true,
                member_role: Some(GroupMemberRole::Admin),
                target_id: Some("bot-appid".to_owned()),
            },
            GroupMention {
                is_you: false,
                member_role: Some(GroupMemberRole::Member),
                target_id: Some("member-2".to_owned()),
            },
        ];

        let inbound = inbound_from_group(&message);
        let request = super::super::to_core_request(&inbound, inbound.text.clone()).unwrap();
        let context = request.message_context();

        // 结构化 mention 按事件顺序进入上下文，第一个是 self，第二个是普通成员。
        assert_eq!(context.mentions.len(), 2);
        assert!(context.mentions[0].is_self);
        assert_eq!(context.mentions[0].target.is_bot, Some(true));
        assert_eq!(
            context.mentions[0].target.user_id.as_deref(),
            Some("bot-appid")
        );
        assert_eq!(context.mentions[0].confidence, MentionConfidence::Event);
        assert!(!context.mentions[1].is_self);
        // 非 self mention 的 is_bot 事件不提供，保持 None，不伪造。
        assert_eq!(context.mentions[1].target.is_bot, None);
        assert_eq!(
            context.mentions[1].target.user_id.as_deref(),
            Some("member-2")
        );
        assert_eq!(
            context.mentions[1].target.group_member_role.as_deref(),
            Some("member")
        );
        assert_eq!(context.mentions[1].confidence, MentionConfidence::Event);
    }

    #[test]
    fn qq_group_text_mention_without_structured_id_is_text_weak() {
        // 文本 @昵称 且无对应结构化 mention 时，作为 TextWeak 弱候选，不伪造稳定 ID。
        let mut message = group_message();
        message.event_type = GroupEventType::GroupMessage;
        message.mentions = Vec::new();
        message.content = "@小明 你觉得呢".to_owned();

        let inbound = inbound_from_group(&message);
        let context = inbound.mentions.clone();

        assert_eq!(context.len(), 1);
        assert_eq!(context[0].confidence, MentionConfidence::TextWeak);
        assert_eq!(context[0].target.user_id, None);
        assert_eq!(context[0].target.display_name.as_deref(), Some("小明"));
        assert_eq!(context[0].raw_text.as_deref(), Some("@小明"));
        assert!(!context[0].is_self);
    }

    #[test]
    fn qq_group_at_message_event_always_emits_self_mention() {
        // GROUP_AT_MESSAGE_CREATE 事件即便 mentions 为空，也代表 @ 当前机器人。
        let mut message = group_message();
        message.event_type = GroupEventType::GroupAtMessage;
        message.mentions = Vec::new();
        message.content = "/help".to_owned();

        let inbound = inbound_from_group(&message);
        assert_eq!(inbound.mentions.len(), 1);
        assert!(inbound.mentions[0].is_self);
        assert_eq!(inbound.mentions[0].confidence, MentionConfidence::Event);
    }

    #[test]
    fn qq_group_at_message_keeps_plain_mention_order_and_appends_synthetic_self() {
        // GROUP_AT_MESSAGE_CREATE + mentions 全部 is_you=false：保留普通 mentions，
        // 并额外追加一条 synthetic self mention，不把首条普通 mention 误标为 self。
        let mut message = group_message();
        message.event_type = GroupEventType::GroupAtMessage;
        message.mentions = vec![
            GroupMention {
                is_you: false,
                member_role: Some(GroupMemberRole::Member),
                target_id: Some("member-plain".to_owned()),
            },
            GroupMention {
                is_you: false,
                member_role: Some(GroupMemberRole::Admin),
                target_id: Some("member-admin".to_owned()),
            },
        ];
        message.content = "/help".to_owned();

        let inbound = inbound_from_group(&message);
        // 两条普通 mention 保留原序 + 一条 synthetic self mention。
        assert_eq!(inbound.mentions.len(), 3);
        assert!(!inbound.mentions[0].is_self);
        assert_eq!(
            inbound.mentions[0].target.user_id.as_deref(),
            Some("member-plain")
        );
        assert_eq!(inbound.mentions[0].target.is_bot, None);
        assert!(!inbound.mentions[1].is_self);
        assert_eq!(
            inbound.mentions[1].target.user_id.as_deref(),
            Some("member-admin")
        );
        assert!(inbound.mentions[2].is_self);
        assert_eq!(inbound.mentions[2].target.is_bot, Some(true));
        assert_eq!(inbound.mentions[2].confidence, MentionConfidence::Event);
    }

    #[test]
    fn qq_group_at_message_with_plain_then_self_keeps_order() {
        // GROUP_AT_MESSAGE_CREATE + 首条普通 mention、其后 is_you=true 的 self：
        // 首条保持 is_self=false，第二条 is_self=true，不再追加 synthetic self。
        let mut message = group_message();
        message.event_type = GroupEventType::GroupAtMessage;
        message.mentions = vec![
            GroupMention {
                is_you: false,
                member_role: Some(GroupMemberRole::Member),
                target_id: Some("member-plain".to_owned()),
            },
            GroupMention {
                is_you: true,
                member_role: Some(GroupMemberRole::Admin),
                target_id: Some("bot-appid".to_owned()),
            },
        ];
        message.content = "/help".to_owned();

        let inbound = inbound_from_group(&message);
        assert_eq!(inbound.mentions.len(), 2);
        assert!(!inbound.mentions[0].is_self);
        assert_eq!(inbound.mentions[0].target.is_bot, None);
        assert!(inbound.mentions[1].is_self);
        assert_eq!(inbound.mentions[1].target.is_bot, Some(true));
        assert_eq!(
            inbound.mentions[1].target.user_id.as_deref(),
            Some("bot-appid")
        );
    }

    #[test]
    fn qq_group_text_weak_suppressed_when_structured_plain_mention_present() {
        // 存在结构化普通 mention 时，正文中的 @昵称 不再生成独立 TextWeak，
        // 避免同对象被拆成 Event + TextWeak 两条独立 mention。
        let mut message = group_message();
        message.event_type = GroupEventType::GroupMessage;
        message.mentions = vec![GroupMention {
            is_you: false,
            member_role: Some(GroupMemberRole::Member),
            target_id: Some("member-2".to_owned()),
        }];
        message.content = "@小明 你觉得呢".to_owned();

        let inbound = inbound_from_group(&message);
        // 仅保留结构化普通 mention，不额外生成 @小明 的 TextWeak。
        assert_eq!(inbound.mentions.len(), 1);
        assert_eq!(inbound.mentions[0].confidence, MentionConfidence::Event);
        assert_eq!(
            inbound.mentions[0].target.user_id.as_deref(),
            Some("member-2")
        );
    }

    #[test]
    fn qq_group_text_weak_generated_when_only_self_structured_mention() {
        // 仅 self 结构化 mention 时，正文里独立的 @昵称 仍生成 TextWeak 弱候选
        // （@当前机器人 已被 self raw_text 占用，去重后不重复）。
        let mut message = group_message();
        message.event_type = GroupEventType::GroupMessage;
        message.mentions = vec![GroupMention {
            is_you: true,
            member_role: Some(GroupMemberRole::Admin),
            target_id: None,
        }];
        message.content = "@当前机器人 帮我叫 @小明".to_owned();

        let inbound = inbound_from_group(&message);
        // 第一条是 self，其后是 @小明 的 TextWeak 弱候选。
        assert_eq!(inbound.mentions.len(), 2);
        assert!(inbound.mentions[0].is_self);
        assert_eq!(inbound.mentions[1].confidence, MentionConfidence::TextWeak);
        assert_eq!(
            inbound.mentions[1].target.display_name.as_deref(),
            Some("小明")
        );
        assert_eq!(inbound.mentions[1].target.user_id, None);
    }
}

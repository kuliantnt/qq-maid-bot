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
    Attachment as QqAttachment, C2cMessage, GroupEventType, GroupMemberRole, GroupMessage,
    MessageReply,
};

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
        tools_visible_snapshot: None,
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
        tools_visible_snapshot: None,
        mentions: self_mentions_from_group_event(message),
        mentioned_bot: message.event_type == GroupEventType::GroupAtMessage
            || message.mentions.iter().any(|mention| mention.is_you),
    }
}

fn self_mentions_from_group_event(message: &GroupMessage) -> Vec<MentionIdentity> {
    if message.event_type != GroupEventType::GroupAtMessage
        && !message.mentions.iter().any(|mention| mention.is_you)
    {
        return Vec::new();
    }
    vec![MentionIdentity {
        raw_text: Some("@当前机器人".to_owned()),
        target: MessageActorContext {
            is_bot: Some(true),
            source: IdentitySource::Event,
            ..Default::default()
        },
        is_self: true,
        confidence: MentionConfidence::Event,
    }]
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
        let context = request.message_context.as_ref().unwrap();
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
}

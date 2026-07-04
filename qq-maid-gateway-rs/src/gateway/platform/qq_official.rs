//! QQ 官方 Gateway 协议到统一入站模型的 adapter。
//!
//! QQ 专用结构、mention 判定、附件和 reply 字段转换都收口在这里，避免污染平台无关模型。

use super::model::{
    Actor, Attachment, ConversationTarget, GroupMemberRoleKind, InboundMessage, Platform,
    ReplyReference,
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
            display_name: None,
            group_member_role: None,
        },
        message_id: message.message_id.clone(),
        timestamp: message.timestamp.clone(),
        text: message.content.clone(),
        attachments: message.attachments.iter().map(attachment_from_qq).collect(),
        reply: message.reply.as_ref().map(reply_from_qq),
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
            display_name: None,
            group_member_role: message.member_role.map(GroupMemberRoleKind::from),
        },
        message_id: message.message_id.clone(),
        timestamp: message.timestamp.clone(),
        text: message.content.clone(),
        attachments: message.attachments.iter().map(attachment_from_qq).collect(),
        reply: message.reply.as_ref().map(reply_from_qq),
        mentioned_bot: message.event_type == GroupEventType::GroupAtMessage
            || message.mentions.iter().any(|mention| mention.is_you),
    }
}

fn attachment_from_qq(value: &QqAttachment) -> Attachment {
    Attachment {
        content_type: value.content_type.clone(),
        filename: value.filename.clone(),
        url: value.url.clone(),
        placeholder: None,
    }
}

fn reply_from_qq(value: &MessageReply) -> ReplyReference {
    ReplyReference {
        message_id: value.message_id.clone(),
        content: value.content.clone(),
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
            event_id: Some("event-1".to_owned()),
            source_message_ids: vec!["msg-1".to_owned()],
            source_event_ids: vec!["event-1".to_owned()],
            user_openid: "user-1".to_owned(),
            content: "你好".to_owned(),
            reply: None,
            timestamp: Some("2026-07-04T20:00:00+08:00".to_owned()),
            first_message_timestamp: Some("2026-07-04T20:00:00+08:00".to_owned()),
            last_message_timestamp: Some("2026-07-04T20:00:00+08:00".to_owned()),
            attachments: Vec::new(),
        }
    }

    fn group_message() -> GroupMessage {
        GroupMessage {
            message_id: "group-msg-1".to_owned(),
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
        assert_eq!(
            super::super::core_scope_key(&inbound).unwrap(),
            "platform:qq_official:account:-:group:group-1"
        );
        assert_eq!(
            request.actor.group_member_role,
            Some(CoreGroupMemberRole::Admin)
        );
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
            content: Some("上一条".to_owned()),
        });
        message.attachments = vec![QqAttachment {
            content_type: Some("image/jpeg".to_owned()),
            filename: Some("a.jpg".to_owned()),
            url: Some("https://example.test/a.jpg".to_owned()),
        }];

        let inbound = inbound_from_c2c(&message);
        let rendered = super::super::render_text_for_core(&inbound);

        assert_eq!(
            inbound
                .reply
                .as_ref()
                .map(|reply| reply.message_id.as_str()),
            Some("quoted-1")
        );
        assert_eq!(inbound.attachments[0].filename.as_deref(), Some("a.jpg"));
        assert!(rendered.starts_with("[reply message_id=quoted-1]\n上一条\n[/reply]\n你好"));
        assert!(rendered.contains("[附件 image/jpeg: a.jpg https://example.test/a.jpg]"));
    }
}

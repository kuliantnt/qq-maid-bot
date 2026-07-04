//! Gateway 内部的平台无关入站消息模型。
//!
//! 各平台 adapter 负责把 QQ 官方、OneBot、微信等原始协议转换到这里；后续进入
//! CoreService 前只依赖这个模型，避免平台字段继续向 Core / LLM 扩散。

use qq_maid_core::service::{
    CoreActor, CoreConversation, CoreGroupMemberRole, CoreRequest, Platform as CorePlatform,
};

use super::event::{
    Attachment as QqAttachment, C2cMessage, GroupEventType, GroupMemberRole, GroupMessage,
    MessageReply,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    QqOfficial,
    // 后续 OneBot 11 adapter 接入后会由对应协议转换层构造。
    #[allow(dead_code)]
    OneBot11,
    // 后续微信服务号 adapter 接入后会由 XML 回调转换层构造。
    #[allow(dead_code)]
    WechatService,
}

impl Platform {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::QqOfficial => "qq_official",
            Self::OneBot11 => "onebot11",
            Self::WechatService => "wechat_service",
        }
    }

    fn core_platform(self) -> Option<CorePlatform> {
        match self {
            Self::QqOfficial => Some(CorePlatform::QqOfficial),
            Self::OneBot11 => Some(CorePlatform::OneBot),
            // 微信还没有 Core 侧平台枚举，本任务只建立 Gateway 边界。
            Self::WechatService => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InboundMessage {
    pub(crate) platform: Platform,
    pub(crate) account_id: Option<String>,
    pub(crate) conversation: ConversationTarget,
    pub(crate) actor: Actor,
    pub(crate) message_id: String,
    pub(crate) timestamp: Option<String>,
    pub(crate) text: String,
    pub(crate) attachments: Vec<Attachment>,
    pub(crate) reply: Option<ReplyReference>,
    pub(crate) mentioned_bot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConversationTarget {
    Private {
        target_id: String,
    },
    Group {
        target_id: String,
    },
    // 预留给频道类平台；当前 QQ 官方入口不构造该会话类型。
    #[allow(dead_code)]
    Channel {
        target_id: String,
    },
    // 预留给微信服务号这类公众号会话；当前还不映射到 Core。
    #[allow(dead_code)]
    ServiceAccount {
        target_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Actor {
    pub(crate) sender_id: Option<String>,
    pub(crate) display_name: Option<String>,
    pub(crate) group_member_role: Option<GroupMemberRoleKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GroupMemberRoleKind {
    Owner,
    Admin,
    Member,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attachment {
    pub(crate) content_type: Option<String>,
    pub(crate) filename: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) placeholder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplyReference {
    pub(crate) message_id: String,
    pub(crate) content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum InboundCoreMappingError {
    #[error("unsupported platform for core respond: {0}")]
    UnsupportedPlatform(&'static str),
    #[error("unsupported conversation for core respond")]
    UnsupportedConversation,
}

impl InboundMessage {
    pub(crate) fn from_qq_c2c(message: &C2cMessage) -> Self {
        Self {
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
            attachments: message.attachments.iter().map(Attachment::from).collect(),
            reply: message.reply.as_ref().map(ReplyReference::from),
            mentioned_bot: false,
        }
    }

    pub(crate) fn from_qq_group(message: &GroupMessage) -> Self {
        Self {
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
            attachments: message.attachments.iter().map(Attachment::from).collect(),
            reply: message.reply.as_ref().map(ReplyReference::from),
            mentioned_bot: message.event_type == GroupEventType::GroupAtMessage
                || message.mentions.iter().any(|mention| mention.is_you),
        }
    }

    pub(crate) fn to_core_request(
        &self,
        text: String,
    ) -> Result<CoreRequest, InboundCoreMappingError> {
        let platform =
            self.platform
                .core_platform()
                .ok_or(InboundCoreMappingError::UnsupportedPlatform(
                    self.platform.as_str(),
                ))?;
        let conversation = match &self.conversation {
            ConversationTarget::Private { target_id } => CoreConversation::Private {
                peer_id: target_id.clone(),
            },
            ConversationTarget::Group { target_id } => CoreConversation::Group {
                group_id: target_id.clone(),
            },
            ConversationTarget::Channel { .. } | ConversationTarget::ServiceAccount { .. } => {
                return Err(InboundCoreMappingError::UnsupportedConversation);
            }
        };

        Ok(CoreRequest {
            text,
            platform,
            actor: CoreActor {
                user_id: self.actor.sender_id.clone(),
                group_member_role: self.actor.group_member_role.map(CoreGroupMemberRole::from),
            },
            conversation,
        })
    }

    pub(crate) fn core_scope_key(&self) -> Result<String, InboundCoreMappingError> {
        self.to_core_request(String::new())
            .map(|request| request.scope_key())
    }

    pub(crate) fn render_text_for_core(&self) -> String {
        let mut content = String::new();
        if let Some(reply) = &self.reply {
            content.push_str(&format!("[reply message_id={}]\n", reply.message_id));
            if let Some(reply_content) = reply.content.as_deref() {
                content.push_str(reply_content);
            }
            content.push_str("\n[/reply]\n");
        }
        content.push_str(&self.text);
        for attachment in &self.attachments {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&attachment.note());
        }
        content
    }

    // 当前 QQ C2C 聚合仍使用既有 message/event reservation；该 key 先作为统一入站模型
    // 的跨平台去重语义，供后续 OneBot/微信 adapter 接入时复用。
    #[allow(dead_code)]
    pub(crate) fn dedupe_message_key(&self) -> Option<String> {
        let message_id = self.message_id.trim();
        if message_id.is_empty() {
            return None;
        }
        let account = self
            .account_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("-");
        Some(format!(
            "{}:{account}:message:{message_id}",
            self.platform.as_str()
        ))
    }
}

impl Attachment {
    fn note(&self) -> String {
        if let Some(placeholder) = self.placeholder.as_deref() {
            return placeholder.to_owned();
        }
        let content_type = self.content_type.as_deref().unwrap_or("unknown");
        let filename = self.filename.as_deref().unwrap_or("unnamed");
        let url = self.url.as_deref().unwrap_or("no-url");
        format!("[附件 {content_type}: {filename} {url}]")
    }
}

impl From<&QqAttachment> for Attachment {
    fn from(value: &QqAttachment) -> Self {
        Self {
            content_type: value.content_type.clone(),
            filename: value.filename.clone(),
            url: value.url.clone(),
            placeholder: None,
        }
    }
}

impl From<&MessageReply> for ReplyReference {
    fn from(value: &MessageReply) -> Self {
        Self {
            message_id: value.message_id.clone(),
            content: value.content.clone(),
        }
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

impl From<GroupMemberRoleKind> for CoreGroupMemberRole {
    fn from(value: GroupMemberRoleKind) -> Self {
        match value {
            GroupMemberRoleKind::Owner => Self::Owner,
            GroupMemberRoleKind::Admin => Self::Admin,
            GroupMemberRoleKind::Member => Self::Member,
            GroupMemberRoleKind::Unknown => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::event::{GroupMention, MessageReply};

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
        let inbound = InboundMessage::from_qq_c2c(&c2c_message());
        let request = inbound.to_core_request(inbound.text.clone()).unwrap();

        assert_eq!(inbound.platform, Platform::QqOfficial);
        assert_eq!(inbound.actor.sender_id.as_deref(), Some("user-1"));
        assert_eq!(inbound.core_scope_key().unwrap(), "private:user-1");
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
        let inbound = InboundMessage::from_qq_group(&group_message());
        let request = inbound.to_core_request(inbound.text.clone()).unwrap();

        assert_eq!(inbound.actor.sender_id.as_deref(), Some("member-1"));
        assert_eq!(
            inbound.actor.group_member_role,
            Some(GroupMemberRoleKind::Admin)
        );
        assert!(inbound.mentioned_bot);
        assert_eq!(inbound.core_scope_key().unwrap(), "group:group-1");
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
    fn render_text_for_core_keeps_reply_and_attachment_notes() {
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

        let rendered = InboundMessage::from_qq_c2c(&message).render_text_for_core();

        assert!(rendered.starts_with("[reply message_id=quoted-1]\n上一条\n[/reply]\n你好"));
        assert!(rendered.contains("[附件 image/jpeg: a.jpg https://example.test/a.jpg]"));
    }

    #[test]
    fn dedupe_message_key_is_platform_and_account_scoped() {
        let mut inbound = InboundMessage::from_qq_c2c(&c2c_message());
        inbound.account_id = Some("bot-a".to_owned());

        assert_eq!(
            inbound.dedupe_message_key().as_deref(),
            Some("qq_official:bot-a:message:msg-1")
        );

        inbound.account_id = Some("bot-b".to_owned());
        assert_eq!(
            inbound.dedupe_message_key().as_deref(),
            Some("qq_official:bot-b:message:msg-1")
        );
    }
}

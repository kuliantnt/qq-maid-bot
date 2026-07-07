//! 平台无关的 Gateway 入站消息模型。
//!
//! 本文件不依赖 QQ 官方、OneBot 或微信协议类型；所有协议字段都必须先在 adapter
//! 层转换为这些通用结构，再进入 Core 映射和回复编排。

use qq_maid_common::{
    identity_context::{IdentitySource, MentionIdentity},
    input_part::{MessageInputPart, MessageMedia, QuotedMessageContext},
};
use qq_maid_core::service::VisibleEntitySnapshot;

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InboundMessage {
    pub(crate) platform: Platform,
    pub(crate) account_id: Option<String>,
    pub(crate) conversation: ConversationTarget,
    pub(crate) actor: Actor,
    pub(crate) message_id: String,
    pub(crate) current_msg_idx: Option<String>,
    pub(crate) timestamp: Option<String>,
    pub(crate) text: String,
    pub(crate) input_parts: Vec<MessageInputPart>,
    pub(crate) attachments: Vec<Attachment>,
    pub(crate) quoted: Option<QuotedMessageContext>,
    /// 引用消息关联的工具可见实体快照。Gateway 只透传，不解析具体 domain。
    pub(crate) visible_entity_snapshot: Option<VisibleEntitySnapshot>,
    /// 平台事件提供的结构化 mention 目标。文本 @昵称 不应伪造成稳定身份。
    pub(crate) mentions: Vec<MentionIdentity>,
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
    pub(crate) union_id: Option<String>,
    pub(crate) display_name: Option<String>,
    pub(crate) group_member_role: Option<GroupMemberRoleKind>,
    pub(crate) is_bot: bool,
    pub(crate) source: IdentitySource,
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
    pub(crate) size_bytes: Option<u64>,
    pub(crate) media_id: Option<String>,
    pub(crate) file_id: Option<String>,
    pub(crate) attachment_id: Option<String>,
    pub(crate) placeholder: Option<String>,
}

impl Attachment {
    pub(crate) fn to_input_part(&self, platform: Platform) -> MessageInputPart {
        if let Some(placeholder) = self.placeholder.as_deref() {
            return MessageInputPart::text(placeholder.to_owned());
        }
        let mut media = MessageMedia {
            mime_type: self.content_type.clone(),
            filename: self.filename.clone(),
            size_bytes: self.size_bytes,
            url: self.url.clone(),
            local_path: None,
            media_id: self.media_id.clone(),
            file_id: self.file_id.clone(),
            attachment_id: self.attachment_id.clone(),
            platform: Some(platform.as_str().to_owned()),
            status: Default::default(),
        };
        media.status = media.inferred_readability_status();
        match attachment_kind(self.content_type.as_deref(), self.filename.as_deref()) {
            AttachmentKind::Image => MessageInputPart::image(media),
            AttachmentKind::File => MessageInputPart::file(media),
            AttachmentKind::Unknown => MessageInputPart::unknown(media, "unsupported_media_type"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachmentKind {
    Image,
    File,
    Unknown,
}

fn attachment_kind(content_type: Option<&str>, filename: Option<&str>) -> AttachmentKind {
    let content_type = content_type.unwrap_or("").trim().to_ascii_lowercase();
    if content_type.starts_with("image/") || content_type == "image" {
        return AttachmentKind::Image;
    }
    if !content_type.is_empty() {
        return AttachmentKind::File;
    }
    let filename = filename.unwrap_or("").trim().to_ascii_lowercase();
    if matches!(
        filename.rsplit('.').next(),
        Some("jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp")
    ) {
        return AttachmentKind::Image;
    }
    if filename.is_empty() {
        AttachmentKind::Unknown
    } else {
        AttachmentKind::File
    }
}

impl InboundMessage {
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
        let conversation_kind = self.conversation.kind();
        let conversation_target = self.conversation.target_id();
        Some(format!(
            "{}:{account}:{conversation_kind}:{conversation_target}:message:{message_id}",
            self.platform.as_str()
        ))
    }
}

impl ConversationTarget {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Private { .. } => "private",
            Self::Group { .. } => "group",
            Self::Channel { .. } => "channel",
            Self::ServiceAccount { .. } => "service_account",
        }
    }

    pub(crate) fn target_id(&self) -> &str {
        match self {
            Self::Private { target_id }
            | Self::Group { target_id }
            | Self::Channel { target_id }
            | Self::ServiceAccount { target_id } => target_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(sender_id: &str) -> Actor {
        Actor {
            sender_id: Some(sender_id.to_owned()),
            union_id: None,
            display_name: Some("测试用户".to_owned()),
            group_member_role: None,
            is_bot: false,
            source: IdentitySource::Event,
        }
    }

    #[test]
    fn pure_model_expresses_private_conversation_and_dedupe_key() {
        let inbound = InboundMessage {
            platform: Platform::OneBot11,
            account_id: Some("bot-10000".to_owned()),
            conversation: ConversationTarget::Private {
                target_id: "user-1".to_owned(),
            },
            actor: actor("user-1"),
            message_id: "msg-1".to_owned(),
            current_msg_idx: None,
            timestamp: Some("2026-07-04T20:00:00+08:00".to_owned()),
            text: "你好".to_owned(),
            input_parts: vec![MessageInputPart::text("你好")],
            attachments: Vec::new(),
            quoted: None,
            mentions: Vec::new(),
            mentioned_bot: false,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            inbound.conversation,
            ConversationTarget::Private {
                target_id: "user-1".to_owned()
            }
        );
        assert_eq!(
            inbound.dedupe_message_key().as_deref(),
            Some("onebot11:bot-10000:private:user-1:message:msg-1")
        );
    }

    #[test]
    fn pure_model_expresses_group_conversation_and_attachment_placeholder() {
        let inbound = InboundMessage {
            platform: Platform::WechatService,
            account_id: Some("wx-app".to_owned()),
            conversation: ConversationTarget::Group {
                target_id: "group-1".to_owned(),
            },
            actor: Actor {
                sender_id: Some("member-1".to_owned()),
                union_id: None,
                display_name: None,
                group_member_role: Some(GroupMemberRoleKind::Member),
                is_bot: false,
                source: IdentitySource::Event,
            },
            message_id: "msg-2".to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: "看图".to_owned(),
            input_parts: vec![
                MessageInputPart::text("看图"),
                MessageInputPart::image(MessageMedia {
                    mime_type: Some("image/png".to_owned()),
                    ..Default::default()
                }),
            ],
            attachments: vec![Attachment {
                content_type: Some("image/png".to_owned()),
                filename: None,
                url: None,
                size_bytes: None,
                media_id: None,
                file_id: None,
                attachment_id: None,
                placeholder: Some("[图片]".to_owned()),
            }],
            quoted: Some(QuotedMessageContext {
                reference_id: Some("quoted-1".to_owned()),
                ..Default::default()
            }),
            mentions: Vec::new(),
            mentioned_bot: true,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            inbound.conversation,
            ConversationTarget::Group {
                target_id: "group-1".to_owned()
            }
        );
        assert_eq!(
            inbound.attachments[0].placeholder.as_deref(),
            Some("[图片]")
        );
        assert!(inbound.mentioned_bot);
        assert_eq!(
            inbound.dedupe_message_key().as_deref(),
            Some("wechat_service:wx-app:group:group-1:message:msg-2")
        );
    }

    #[test]
    fn dedupe_key_includes_conversation_to_avoid_cross_scope_collisions() {
        let private = InboundMessage {
            platform: Platform::QqOfficial,
            account_id: None,
            conversation: ConversationTarget::Private {
                target_id: "user-1".to_owned(),
            },
            actor: actor("user-1"),
            message_id: "same-msg".to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: "私聊".to_owned(),
            input_parts: vec![MessageInputPart::text("私聊")],
            attachments: Vec::new(),
            quoted: None,
            mentions: Vec::new(),
            mentioned_bot: false,
            visible_entity_snapshot: None,
        };
        let group = InboundMessage {
            platform: Platform::QqOfficial,
            account_id: None,
            conversation: ConversationTarget::Group {
                target_id: "group-1".to_owned(),
            },
            actor: actor("member-1"),
            message_id: "same-msg".to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: "群聊".to_owned(),
            input_parts: vec![MessageInputPart::text("群聊")],
            attachments: Vec::new(),
            quoted: None,
            mentions: Vec::new(),
            mentioned_bot: true,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            private.dedupe_message_key().as_deref(),
            Some("qq_official:-:private:user-1:message:same-msg")
        );
        assert_eq!(
            group.dedupe_message_key().as_deref(),
            Some("qq_official:-:group:group-1:message:same-msg")
        );
        assert_ne!(private.dedupe_message_key(), group.dedupe_message_key());
    }

    #[test]
    fn dedupe_key_is_none_for_empty_message_id() {
        let inbound = InboundMessage {
            platform: Platform::QqOfficial,
            account_id: None,
            conversation: ConversationTarget::Private {
                target_id: "user-1".to_owned(),
            },
            actor: actor("user-1"),
            message_id: "  ".to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: "空 id".to_owned(),
            input_parts: vec![MessageInputPart::text("空 id")],
            attachments: Vec::new(),
            quoted: None,
            mentions: Vec::new(),
            mentioned_bot: false,
            visible_entity_snapshot: None,
        };

        assert_eq!(inbound.dedupe_message_key(), None);
    }
}

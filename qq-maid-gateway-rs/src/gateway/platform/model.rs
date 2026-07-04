//! 平台无关的 Gateway 入站消息模型。
//!
//! 本文件不依赖 QQ 官方、OneBot 或微信协议类型；所有协议字段都必须先在 adapter
//! 层转换为这些通用结构，再进入 Core 映射和回复编排。

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
        Some(format!(
            "{}:{account}:message:{message_id}",
            self.platform.as_str()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(sender_id: &str) -> Actor {
        Actor {
            sender_id: Some(sender_id.to_owned()),
            display_name: Some("测试用户".to_owned()),
            group_member_role: None,
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
            timestamp: Some("2026-07-04T20:00:00+08:00".to_owned()),
            text: "你好".to_owned(),
            attachments: Vec::new(),
            reply: None,
            mentioned_bot: false,
        };

        assert_eq!(
            inbound.conversation,
            ConversationTarget::Private {
                target_id: "user-1".to_owned()
            }
        );
        assert_eq!(
            inbound.dedupe_message_key().as_deref(),
            Some("onebot11:bot-10000:message:msg-1")
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
                display_name: None,
                group_member_role: Some(GroupMemberRoleKind::Member),
            },
            message_id: "msg-2".to_owned(),
            timestamp: None,
            text: "看图".to_owned(),
            attachments: vec![Attachment {
                content_type: Some("image".to_owned()),
                filename: None,
                url: None,
                placeholder: Some("[图片]".to_owned()),
            }],
            reply: Some(ReplyReference {
                message_id: "quoted-1".to_owned(),
                content: None,
            }),
            mentioned_bot: true,
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
    }
}

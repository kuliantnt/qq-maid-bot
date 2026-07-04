//! 统一入站模型到 CoreService 的映射和 Core 文本协议渲染。
//!
//! 这里仍属于 Gateway 边界：Core 不理解平台原始协议，也不接收附件/reply 的结构化字段。

use qq_maid_core::service::{
    CoreActor, CoreConversation, CoreGroupMemberRole, CoreRequest, Platform as CorePlatform,
};

use super::model::{Attachment, ConversationTarget, GroupMemberRoleKind, InboundMessage, Platform};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum InboundCoreMappingError {
    #[error("unsupported platform for core respond: {0}")]
    UnsupportedPlatform(&'static str),
    #[error("unsupported conversation for core respond")]
    UnsupportedConversation,
}

pub(crate) fn to_core_request(
    inbound: &InboundMessage,
    text: String,
) -> Result<CoreRequest, InboundCoreMappingError> {
    let platform = core_platform(inbound.platform).ok_or(
        InboundCoreMappingError::UnsupportedPlatform(inbound.platform.as_str()),
    )?;
    let conversation = match &inbound.conversation {
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
            user_id: inbound.actor.sender_id.clone(),
            group_member_role: inbound
                .actor
                .group_member_role
                .map(CoreGroupMemberRole::from),
        },
        conversation,
    })
}

pub(crate) fn core_scope_key(inbound: &InboundMessage) -> Result<String, InboundCoreMappingError> {
    to_core_request(inbound, String::new()).map(|request| request.scope_key())
}

pub(crate) fn render_text_for_core(inbound: &InboundMessage) -> String {
    let mut content = String::new();
    if let Some(reply) = &inbound.reply {
        content.push_str(&format!("[reply message_id={}]\n", reply.message_id));
        if let Some(reply_content) = reply.content.as_deref() {
            content.push_str(reply_content);
        }
        content.push_str("\n[/reply]\n");
    }
    content.push_str(&inbound.text);
    for attachment in &inbound.attachments {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&attachment_note(attachment));
    }
    content
}

fn core_platform(platform: Platform) -> Option<CorePlatform> {
    match platform {
        Platform::QqOfficial => Some(CorePlatform::QqOfficial),
        Platform::OneBot11 => Some(CorePlatform::OneBot),
        // 微信还没有 Core 侧平台枚举，本任务只建立 Gateway 边界。
        Platform::WechatService => None,
    }
}

fn attachment_note(attachment: &Attachment) -> String {
    if let Some(placeholder) = attachment.placeholder.as_deref() {
        return placeholder.to_owned();
    }
    let content_type = attachment.content_type.as_deref().unwrap_or("unknown");
    let filename = attachment.filename.as_deref().unwrap_or("unnamed");
    let url = attachment.url.as_deref().unwrap_or("no-url");
    format!("[附件 {content_type}: {filename} {url}]")
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
    use super::super::model::{
        Actor, Attachment, ConversationTarget, InboundMessage, Platform, ReplyReference,
    };
    use super::*;

    #[test]
    fn core_render_uses_attachment_placeholder_without_platform_protocol() {
        let inbound = InboundMessage {
            platform: Platform::OneBot11,
            account_id: Some("bot-1".to_owned()),
            conversation: ConversationTarget::Private {
                target_id: "user-1".to_owned(),
            },
            actor: Actor {
                sender_id: Some("user-1".to_owned()),
                display_name: None,
                group_member_role: None,
            },
            message_id: "msg-1".to_owned(),
            timestamp: None,
            text: "看一下".to_owned(),
            attachments: vec![Attachment {
                content_type: None,
                filename: None,
                url: None,
                placeholder: Some("[图片]".to_owned()),
            }],
            reply: Some(ReplyReference {
                message_id: "quoted-1".to_owned(),
                content: Some("上一条".to_owned()),
            }),
            mentioned_bot: false,
        };

        assert_eq!(
            render_text_for_core(&inbound),
            "[reply message_id=quoted-1]\n上一条\n[/reply]\n看一下\n[图片]"
        );
    }
}

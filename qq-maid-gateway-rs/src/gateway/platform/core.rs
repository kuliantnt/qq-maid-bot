//! 统一入站模型到 CoreService 的映射和 Core 文本协议渲染。
//!
//! 这里仍属于 Gateway 边界：Core 不理解平台原始协议，只接收平台无关的有序 input parts。

use qq_maid_common::input_part::MessageInputPart;
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
        ConversationTarget::ServiceAccount { target_id } => CoreConversation::ServiceAccount {
            account_id: inbound.account_id.clone(),
            peer_id: target_id.clone(),
        },
        ConversationTarget::Channel { .. } => {
            return Err(InboundCoreMappingError::UnsupportedConversation);
        }
    };

    Ok(CoreRequest {
        text,
        input_parts: effective_input_parts(inbound),
        quoted: inbound.quoted.clone(),
        platform,
        account_id: inbound.account_id.clone(),
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
    let parts = body_input_parts(inbound);
    if parts.is_empty() {
        content.push_str(&inbound.text);
    } else {
        content.push_str(
            &parts
                .iter()
                .map(MessageInputPart::fallback_text)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if parts.is_empty() {
        for attachment in &inbound.attachments {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&attachment_note(attachment));
        }
    }
    content
}

fn effective_input_parts(inbound: &InboundMessage) -> Vec<MessageInputPart> {
    body_input_parts(inbound)
}

fn body_input_parts(inbound: &InboundMessage) -> Vec<MessageInputPart> {
    if !inbound.input_parts.is_empty() {
        return inbound.input_parts.clone();
    }
    let mut parts = Vec::new();
    if !inbound.text.trim().is_empty() {
        parts.push(MessageInputPart::text(inbound.text.clone()));
    }
    parts.extend(
        inbound
            .attachments
            .iter()
            .map(|attachment| attachment.to_input_part(inbound.platform)),
    );
    parts
}

fn core_platform(platform: Platform) -> Option<CorePlatform> {
    match platform {
        Platform::QqOfficial => Some(CorePlatform::QqOfficial),
        Platform::OneBot11 => Some(CorePlatform::OneBot),
        Platform::WechatService => Some(CorePlatform::WechatService),
    }
}

fn attachment_note(attachment: &Attachment) -> String {
    if let Some(placeholder) = attachment.placeholder.as_deref() {
        return placeholder.to_owned();
    }
    let content_type = attachment.content_type.as_deref().unwrap_or("unknown");
    let filename = attachment.filename.as_deref().unwrap_or("unnamed");
    format!("[附件 {content_type}: {filename}]")
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
    use super::super::model::{Actor, Attachment, ConversationTarget, InboundMessage, Platform};
    use super::*;
    use qq_maid_common::input_part::{MessageMedia, QuotedMessageContext};

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
                is_bot: false,
            },
            message_id: "msg-1".to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: "看一下".to_owned(),
            input_parts: Vec::new(),
            attachments: vec![Attachment {
                content_type: None,
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
                text_summary: Some("上一条".to_owned()),
                lookup_found: true,
                ..Default::default()
            }),
            mentioned_bot: false,
        };

        assert_eq!(render_text_for_core(&inbound), "看一下\n[图片]");
    }

    #[test]
    fn core_mapping_preserves_structured_quote_and_ordered_body_parts() {
        let inbound = InboundMessage {
            platform: Platform::QqOfficial,
            account_id: None,
            conversation: ConversationTarget::Private {
                target_id: "user-1".to_owned(),
            },
            actor: Actor {
                sender_id: Some("user-1".to_owned()),
                display_name: None,
                group_member_role: None,
                is_bot: false,
            },
            message_id: "msg-1".to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: "看一下".to_owned(),
            input_parts: vec![
                MessageInputPart::text("看一下"),
                MessageInputPart::image(MessageMedia {
                    mime_type: Some("image/png".to_owned()),
                    filename: Some("a.png".to_owned()),
                    url: Some("https://example.test/a.png".to_owned()),
                    ..Default::default()
                }),
            ],
            attachments: Vec::new(),
            quoted: Some(QuotedMessageContext {
                reference_id: Some("quoted-1".to_owned()),
                text_summary: Some("上一条".to_owned()),
                lookup_found: true,
                ..Default::default()
            }),
            mentioned_bot: false,
        };

        let rendered = render_text_for_core(&inbound);
        let request = to_core_request(&inbound, rendered.clone()).unwrap();

        assert_eq!(rendered, "看一下\n[图片 image/png: a.png]");
        assert_eq!(request.text, rendered);
        assert_eq!(
            request.quoted.as_ref().unwrap().text_summary.as_deref(),
            Some("上一条")
        );
        assert_eq!(request.input_parts.len(), 2);
        assert_eq!(request.input_parts[0].text_content(), Some("看一下"));
        assert!(matches!(
            request.input_parts[1],
            MessageInputPart::Image { .. }
        ));
    }
}

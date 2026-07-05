use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::warn;

use crate::gateway::logging::mask_openid;
use qq_maid_common::input_part::{MessageInputPart, MessageMedia};

pub const EVENT_C2C_MESSAGE_CREATE: &str = "C2C_MESSAGE_CREATE";
pub const EVENT_GROUP_AT_MESSAGE_CREATE: &str = "GROUP_AT_MESSAGE_CREATE";
pub const EVENT_GROUP_MESSAGE_CREATE: &str = "GROUP_MESSAGE_CREATE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupEventType {
    GroupAtMessage,
    GroupMessage,
}

impl GroupEventType {
    pub fn as_respond_event_type(self) -> &'static str {
        match self {
            Self::GroupAtMessage => "group_at_message",
            Self::GroupMessage => "group_message",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct GatewayEnvelope {
    pub op: u64,
    #[serde(default)]
    pub d: Value,
    #[serde(default)]
    pub s: Option<u64>,
    #[serde(default)]
    pub t: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct C2cMessage {
    pub message_id: String,
    pub event_id: Option<String>,
    pub source_message_ids: Vec<String>,
    pub source_event_ids: Vec<String>,
    pub user_openid: String,
    pub content: String,
    pub reply: Option<MessageReply>,
    pub timestamp: Option<String>,
    pub first_message_timestamp: Option<String>,
    pub last_message_timestamp: Option<String>,
    pub input_parts: Vec<MessageInputPart>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMessage {
    pub message_id: String,
    pub group_openid: String,
    pub member_openid: Option<String>,
    pub member_role: Option<GroupMemberRole>,
    pub content: String,
    pub mentions: Vec<GroupMention>,
    pub reply: Option<MessageReply>,
    pub timestamp: Option<String>,
    pub input_parts: Vec<MessageInputPart>,
    pub attachments: Vec<Attachment>,
    pub event_type: GroupEventType,
    pub author_is_bot: bool,
    pub author_is_self: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMention {
    pub is_you: bool,
    pub member_role: Option<GroupMemberRole>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupMemberRole {
    Owner,
    Admin,
    Member,
    Unknown,
}

impl GroupMemberRole {
    fn from_raw(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "owner" => Self::Owner,
            "admin" => Self::Admin,
            "member" => Self::Member,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageReply {
    pub message_id: String,
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Attachment {
    #[serde(default, alias = "content_type", alias = "mime_type")]
    pub content_type: Option<String>,
    #[serde(default, alias = "filename", alias = "file_name", alias = "name")]
    pub filename: Option<String>,
    #[serde(default, alias = "url", alias = "file_url", alias = "image_url")]
    pub url: Option<String>,
    #[serde(default, alias = "size", alias = "file_size")]
    pub size_bytes: Option<u64>,
    #[serde(default, alias = "media_id")]
    pub media_id: Option<String>,
    #[serde(default, alias = "file_id")]
    pub file_id: Option<String>,
    #[serde(default, alias = "attachment_id", alias = "id")]
    pub attachment_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawC2cMessage {
    #[serde(default, alias = "message_id")]
    id: Option<String>,
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    author: Option<RawAuthor>,
    #[serde(default)]
    user_openid: Option<String>,
    #[serde(default)]
    openid: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reply: Option<RawMessageReply>,
    #[serde(default)]
    quote: Option<RawMessageReply>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    attachments: Vec<Attachment>,
}

#[derive(Debug, Deserialize)]
struct RawGroupMessage {
    #[serde(default, alias = "message_id")]
    id: Option<String>,
    #[serde(default)]
    event_id: Option<String>,
    group_openid: Option<String>,
    #[serde(default)]
    group_id: Option<String>,
    #[serde(default)]
    author: Option<RawAuthor>,
    #[serde(default)]
    user_openid: Option<String>,
    #[serde(default)]
    member_openid: Option<String>,
    #[serde(default)]
    member_role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    mentions: Vec<RawMention>,
    #[serde(default)]
    reply: Option<RawMessageReply>,
    #[serde(default)]
    quote: Option<RawMessageReply>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    attachments: Vec<Attachment>,
    #[serde(default)]
    bot: Option<bool>,
    #[serde(default)]
    is_bot: Option<bool>,
    #[serde(default)]
    self_sent: Option<bool>,
    #[serde(default)]
    is_self: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RawAuthor {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    openid: Option<String>,
    #[serde(default)]
    user_openid: Option<String>,
    #[serde(default)]
    member_openid: Option<String>,
    #[serde(default)]
    member_role: Option<String>,
    #[serde(default)]
    bot: Option<bool>,
    #[serde(default)]
    is_bot: Option<bool>,
    #[serde(default)]
    self_sent: Option<bool>,
    #[serde(default)]
    is_self: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RawMention {
    #[serde(default)]
    is_you: Option<bool>,
    #[serde(default)]
    member_role: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawMessageReply {
    #[serde(default, alias = "id")]
    message_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum EventError {
    #[error("invalid C2C message event: {0}")]
    InvalidC2c(#[from] serde_json::Error),
    #[error("invalid group message event: {0}")]
    InvalidGroup(serde_json::Error),
    #[error("C2C message missing message id")]
    MissingMessageId,
    #[error("C2C message missing user_openid")]
    MissingUserOpenid,
    #[error("group message missing group_openid")]
    MissingGroupOpenid,
}

pub fn parse_c2c_message(envelope: &GatewayEnvelope) -> Result<Option<C2cMessage>, EventError> {
    if envelope.t.as_deref() != Some(EVENT_C2C_MESSAGE_CREATE) {
        return Ok(None);
    }

    let raw = serde_json::from_value::<RawC2cMessage>(envelope.d.clone())?;
    let event_id = raw.event_id.or_else(|| envelope.id.clone());
    let message_id = raw
        .id
        .or_else(|| event_id.clone())
        .filter(|value| !value.trim().is_empty())
        .ok_or(EventError::MissingMessageId)?;
    let user_openid = resolve_c2c_user_openid(
        envelope.t.as_deref().unwrap_or(EVENT_C2C_MESSAGE_CREATE),
        raw.author.as_ref(),
        raw.user_openid.as_deref(),
        raw.openid.as_deref(),
    )
    .ok_or(EventError::MissingUserOpenid)?;
    let base_content = raw.content.unwrap_or_default().trim().to_owned();
    let reply = extract_message_reply(&base_content, raw.reply.as_ref(), raw.quote.as_ref());
    let timestamp = raw.timestamp;
    let input_parts =
        input_parts_from_content_and_attachments(&base_content, &raw.attachments, "qq_official");
    Ok(Some(C2cMessage {
        source_message_ids: vec![message_id.clone()],
        source_event_ids: event_id.iter().cloned().collect(),
        message_id,
        event_id,
        user_openid,
        content: base_content,
        reply,
        first_message_timestamp: timestamp.clone(),
        last_message_timestamp: timestamp.clone(),
        timestamp,
        input_parts,
        attachments: raw.attachments,
    }))
}

pub fn parse_group_message(envelope: &GatewayEnvelope) -> Result<Option<GroupMessage>, EventError> {
    let event_type = match envelope.t.as_deref() {
        Some(EVENT_GROUP_AT_MESSAGE_CREATE) => GroupEventType::GroupAtMessage,
        Some(EVENT_GROUP_MESSAGE_CREATE) => GroupEventType::GroupMessage,
        _ => return Ok(None),
    };

    let raw = serde_json::from_value::<RawGroupMessage>(envelope.d.clone())
        .map_err(EventError::InvalidGroup)?;
    let message_id = raw
        .id
        .or(raw.event_id)
        .filter(|value| !value.trim().is_empty())
        .ok_or(EventError::MissingMessageId)?;
    // QQ 群事件在不同阶段可能同时携带 `group_openid` 和旧字段 `group_id`；
    // 这里手动合并，避免直接用 serde alias 时命中 duplicate field 报错。
    let group_openid = raw
        .group_openid
        .or(raw.group_id)
        .filter(|value| !value.trim().is_empty())
        .ok_or(EventError::MissingGroupOpenid)?;
    let author = raw.author;
    let member_openid = resolve_group_member_openid(
        envelope.t.as_deref().unwrap_or(EVENT_GROUP_MESSAGE_CREATE),
        author.as_ref(),
        raw.member_openid.as_deref(),
        raw.user_openid.as_deref(),
    );
    let member_role =
        resolve_group_member_role(raw.member_role.as_deref(), author.as_ref(), &raw.mentions);
    let author_is_bot = raw.bot.or(raw.is_bot).unwrap_or(false)
        || author
            .as_ref()
            .and_then(|author| author.bot.or(author.is_bot))
            .unwrap_or(false);
    let author_is_self = raw.self_sent.or(raw.is_self).unwrap_or(false)
        || author
            .as_ref()
            .and_then(|author| author.self_sent.or(author.is_self))
            .unwrap_or(false);
    let base_content = raw.content.unwrap_or_default().trim().to_owned();
    let reply = extract_message_reply(&base_content, raw.reply.as_ref(), raw.quote.as_ref());
    let input_parts =
        input_parts_from_content_and_attachments(&base_content, &raw.attachments, "qq_official");
    Ok(Some(GroupMessage {
        message_id,
        group_openid,
        member_openid,
        member_role,
        content: base_content,
        mentions: raw
            .mentions
            .iter()
            .map(raw_group_mention)
            .collect::<Vec<_>>(),
        reply,
        timestamp: raw.timestamp,
        input_parts,
        attachments: raw.attachments,
        event_type,
        author_is_bot,
        author_is_self,
    }))
}

fn resolve_group_member_role(
    top_member_role: Option<&str>,
    author: Option<&RawAuthor>,
    mentions: &[RawMention],
) -> Option<GroupMemberRole> {
    first_non_empty([
        top_member_role,
        author.and_then(|author| author.member_role.as_deref()),
        mentions.iter().find_map(|mention| {
            mention
                .is_you
                .unwrap_or(false)
                .then_some(mention.member_role.as_deref())
                .flatten()
        }),
    ])
    .map(|value| GroupMemberRole::from_raw(&value))
}

fn raw_group_mention(mention: &RawMention) -> GroupMention {
    GroupMention {
        // 官方结构化 mention 里的 is_you 是普通群消息判断“是否 @ 当前机器人”的可信来源。
        is_you: mention.is_you.unwrap_or(false),
        member_role: mention
            .member_role
            .as_deref()
            .map(GroupMemberRole::from_raw),
    }
}

// reply 只提取一层 message_id，不递归解析引用消息正文或其它扩展字段。
fn extract_message_reply(
    content: &str,
    reply: Option<&RawMessageReply>,
    quote: Option<&RawMessageReply>,
) -> Option<MessageReply> {
    reply
        .and_then(|item| item.message_id.as_deref())
        .or_else(|| quote.and_then(|item| item.message_id.as_deref()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| extract_cq_reply_message_id(content))
        .map(|message_id| MessageReply {
            message_id: message_id.to_owned(),
            content: None,
        })
}

fn extract_cq_reply_message_id(content: &str) -> Option<&str> {
    let marker = "CQ:reply,";
    let start = content.find(marker)?;
    let rest = &content[start + marker.len()..];
    for field in rest.split([',', ']']) {
        if let Some(message_id) = field.strip_prefix("id=") {
            let message_id = message_id.trim();
            if !message_id.is_empty() {
                return Some(message_id);
            }
        }
    }
    None
}

fn resolve_c2c_user_openid(
    event_type: &str,
    author: Option<&RawAuthor>,
    top_user_openid: Option<&str>,
    top_openid: Option<&str>,
) -> Option<String> {
    first_non_empty([
        author.and_then(|author| author.user_openid.as_deref()),
        author.and_then(|author| author.openid.as_deref()),
        author.and_then(|author| author.member_openid.as_deref()),
        top_user_openid,
        top_openid,
    ])
    .or_else(|| legacy_author_id_fallback(event_type, author))
}

fn resolve_group_member_openid(
    event_type: &str,
    author: Option<&RawAuthor>,
    top_member_openid: Option<&str>,
    top_user_openid: Option<&str>,
) -> Option<String> {
    first_non_empty([
        author.and_then(|author| author.member_openid.as_deref()),
        author.and_then(|author| author.user_openid.as_deref()),
        author.and_then(|author| author.openid.as_deref()),
        top_member_openid,
        top_user_openid,
    ])
    .or_else(|| legacy_author_id_fallback(event_type, author))
}

fn first_non_empty<const N: usize>(values: [Option<&str>; N]) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

fn legacy_author_id_fallback(event_type: &str, author: Option<&RawAuthor>) -> Option<String> {
    let value = author
        .and_then(|author| author.id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    // author.id 仅作旧事件兼容兜底；没有证据保证它长期等价于 OpenID，日志必须脱敏。
    warn!(
        event_type = %event_type,
        identity = %mask_openid(value),
        "QQ identity resolved through untrusted author.id fallback"
    );
    Some(value.to_owned())
}

impl Attachment {
    pub fn note(&self) -> String {
        let content_type = self.content_type.as_deref().unwrap_or("unknown");
        let filename = self.filename.as_deref().unwrap_or("unnamed");
        format!("[附件 {content_type}: {filename}]")
    }

    pub fn to_input_part(&self, platform: &str) -> MessageInputPart {
        let media = MessageMedia {
            mime_type: self.content_type.clone(),
            filename: self.filename.clone(),
            size_bytes: self.size_bytes,
            url: self.url.clone(),
            media_id: self.media_id.clone(),
            file_id: self.file_id.clone(),
            attachment_id: self.attachment_id.clone(),
            platform: Some(platform.to_owned()),
            status: Default::default(),
        };
        match attachment_kind(self.content_type.as_deref(), self.filename.as_deref()) {
            AttachmentKind::Image => MessageInputPart::image(media),
            AttachmentKind::File => MessageInputPart::file(media),
            AttachmentKind::Unknown => MessageInputPart::unknown(media, "unsupported_media_type"),
        }
    }
}

fn input_parts_from_content_and_attachments(
    content: &str,
    attachments: &[Attachment],
    platform: &str,
) -> Vec<MessageInputPart> {
    let mut parts = Vec::new();
    if !content.trim().is_empty() {
        parts.push(MessageInputPart::text(content.to_owned()));
    }
    parts.extend(
        attachments
            .iter()
            .map(|attachment| attachment.to_input_part(platform)),
    );
    parts
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
        AttachmentKind::Image
    } else if filename.is_empty() {
        AttachmentKind::Unknown
    } else {
        AttachmentKind::File
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_c2c_message_create() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-1",
                "author": {"user_openid": "user-1"},
                "content": "你好",
                "timestamp": "2026-06-10T12:00:00+08:00",
                "attachments": [{
                    "content_type": "image/jpeg",
                    "filename": "a.jpg",
                    "url": "https://example.test/a.jpg"
                }]
            }),
        };

        let message = parse_c2c_message(&envelope).unwrap().unwrap();

        assert_eq!(message.message_id, "msg-1");
        assert_eq!(message.user_openid, "user-1");
        assert_eq!(message.content, "你好");
        assert_eq!(message.reply, None);
        assert_eq!(
            message.timestamp.as_deref(),
            Some("2026-06-10T12:00:00+08:00")
        );
        assert_eq!(
            message.first_message_timestamp.as_deref(),
            Some("2026-06-10T12:00:00+08:00")
        );
        assert_eq!(
            message.last_message_timestamp.as_deref(),
            Some("2026-06-10T12:00:00+08:00")
        );
        assert_eq!(message.attachments.len(), 1);
    }

    #[test]
    fn ignores_other_events() {
        let envelope = GatewayEnvelope {
            op: 0,
            d: json!({}),
            s: None,
            t: Some("READY".to_owned()),
            id: None,
        };

        assert!(parse_c2c_message(&envelope).unwrap().is_none());
    }

    #[test]
    fn parses_group_at_message_create() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-1",
                "group_openid": "group-1",
                "author": {"member_openid": "member-1"},
                "content": "/rss"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.message_id, "msg-1");
        assert_eq!(message.group_openid, "group-1");
        assert_eq!(message.member_openid.as_deref(), Some("member-1"));
        assert_eq!(message.content, "/rss");
        assert_eq!(message.event_type, GroupEventType::GroupAtMessage);
    }

    #[test]
    fn parses_group_message_member_openid_from_top_level() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-top-member",
                "group_openid": "group-1",
                "member_openid": "member-2",
                "content": "hello"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.member_openid.as_deref(), Some("member-2"));
    }

    #[test]
    fn parses_group_message_with_top_member_and_user_openid() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-top-both",
                "group_openid": "group-1",
                "member_openid": "member-top",
                "user_openid": "user-top",
                "content": "hello"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.member_openid.as_deref(), Some("member-top"));
    }

    #[test]
    fn prefers_author_member_openid_over_top_level_group_identity() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-author-priority",
                "group_openid": "group-1",
                "member_openid": "member-top",
                "user_openid": "user-top",
                "author": {"member_openid": "member-author"},
                "content": "hello"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.member_openid.as_deref(), Some("member-author"));
    }

    #[test]
    fn parses_group_message_with_legacy_author_id_fallback() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-legacy-author-id",
                "group_openid": "group-1",
                "author": {"id": "legacy-author-id"},
                "content": "hello"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.member_openid.as_deref(), Some("legacy-author-id"));
    }

    #[test]
    fn group_message_allows_missing_member_identity() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-no-member",
                "group_openid": "group-1",
                "content": "hello"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.member_openid, None);
    }

    #[test]
    fn parses_plain_group_message_create_with_bot_flags() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-2",
                "group_openid": "group-1",
                "author": {"member_openid": "member-2", "is_bot": true},
                "content": "hello"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.message_id, "msg-2");
        assert_eq!(message.member_openid.as_deref(), Some("member-2"));
        assert_eq!(message.event_type, GroupEventType::GroupMessage);
        assert!(message.author_is_bot);
        assert!(!message.author_is_self);
    }

    #[test]
    fn parses_group_message_structured_mentions() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-mentions",
                "group_openid": "group-1",
                "author": {"member_openid": "member-2", "member_role": "owner"},
                "content": " /help ",
                "mentions": [
                    {"id": "owner-id", "is_you": false, "member_role": "owner"},
                    {"id": "appid", "is_you": true, "member_role": "admin"},
                    {"user_openid": "user-openid", "is_you": false, "member_role": "member"},
                    {"member_openid": "member-openid", "member_role": "future-role"}
                ]
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.content, "/help");
        assert_eq!(message.member_role, Some(GroupMemberRole::Owner));
        assert_eq!(
            message.mentions,
            vec![
                GroupMention {
                    is_you: false,
                    member_role: Some(GroupMemberRole::Owner)
                },
                GroupMention {
                    is_you: true,
                    member_role: Some(GroupMemberRole::Admin)
                },
                GroupMention {
                    is_you: false,
                    member_role: Some(GroupMemberRole::Member)
                },
                GroupMention {
                    is_you: false,
                    member_role: Some(GroupMemberRole::Unknown)
                }
            ]
        );
    }

    #[test]
    fn parses_group_message_self_flag_from_top_level() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-3",
                "group_openid": "group-1",
                "author": {"member_openid": "member-3"},
                "content": "hello",
                "is_self": true
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert!(message.author_is_self);
    }

    #[test]
    fn parses_group_at_message_with_duplicate_openid_fields() {
        // QQ API 有时同时发送 group_openid 和 openid，openid 不应被当作 group_openid 的别名
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-dup",
                "group_openid": "group-1",
                "openid": "group-1",
                "author": {"member_openid": "member-1"},
                "content": "hello"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.group_openid, "group-1");
        assert_eq!(message.member_openid.as_deref(), Some("member-1"));
    }

    #[test]
    fn parses_group_message_from_legacy_group_id_field() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-legacy",
                "group_id": "group-legacy",
                "author": {"member_openid": "member-1"},
                "content": "hello"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.group_openid, "group-legacy");
        assert_eq!(message.member_openid.as_deref(), Some("member-1"));
    }

    #[test]
    fn prefers_group_openid_when_group_id_is_also_present() {
        // QQ API 兼容期内可能同时下发新旧群字段，主字段应优先使用 group_openid。
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-both-group-fields",
                "group_openid": "group-new",
                "group_id": "group-old",
                "author": {"member_openid": "member-1"},
                "content": "hello"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.group_openid, "group-new");
        assert_eq!(message.member_openid.as_deref(), Some("member-1"));
    }

    #[test]
    fn parses_reply_message_id_from_cq_code() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-1",
                "author": {"user_openid": "user-1"},
                "content": "[CQ:reply,id=quoted-1]你好"
            }),
        };

        let message = parse_c2c_message(&envelope).unwrap().unwrap();

        assert_eq!(
            message.reply,
            Some(MessageReply {
                message_id: "quoted-1".to_owned(),
                content: None,
            })
        );
    }

    #[test]
    fn parses_reply_message_id_from_explicit_reply_field() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-1",
                "author": {"user_openid": "user-1"},
                "content": "你好",
                "reply": {
                    "message_id": "quoted-2"
                }
            }),
        };

        let message = parse_c2c_message(&envelope).unwrap().unwrap();

        assert_eq!(
            message.reply,
            Some(MessageReply {
                message_id: "quoted-2".to_owned(),
                content: None,
            })
        );
    }

    #[test]
    fn parses_reply_message_id_from_quote_field() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-1",
                "author": {"user_openid": "user-1"},
                "content": "你好",
                "quote": {
                    "message_id": "quoted-3"
                }
            }),
        };

        let message = parse_c2c_message(&envelope).unwrap().unwrap();

        assert_eq!(
            message.reply,
            Some(MessageReply {
                message_id: "quoted-3".to_owned(),
                content: None,
            })
        );
    }
}

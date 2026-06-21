use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const EVENT_C2C_MESSAGE_CREATE: &str = "C2C_MESSAGE_CREATE";
pub const EVENT_GROUP_AT_MESSAGE_CREATE: &str = "GROUP_AT_MESSAGE_CREATE";
pub const EVENT_GROUP_MESSAGE_CREATE: &str = "GROUP_MESSAGE_CREATE";

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
    pub user_openid: String,
    pub content: String,
    pub reply: Option<MessageReply>,
    pub timestamp: Option<String>,
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMessage {
    pub message_id: String,
    pub group_openid: String,
    pub member_openid: Option<String>,
    pub content: String,
    pub reply: Option<MessageReply>,
    pub timestamp: Option<String>,
    pub attachments: Vec<Attachment>,
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
    #[serde(default)]
    group_openid: Option<String>,
    #[serde(default)]
    group_id: Option<String>,
    #[serde(default)]
    openid: Option<String>,
    #[serde(default)]
    author: Option<RawAuthor>,
    #[serde(default, alias = "member_openid")]
    user_openid: Option<String>,
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
struct RawAuthor {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    openid: Option<String>,
    #[serde(default)]
    user_openid: Option<String>,
    #[serde(default)]
    member_openid: Option<String>,
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
    let message_id = raw
        .id
        .or(raw.event_id)
        .filter(|value| !value.trim().is_empty())
        .ok_or(EventError::MissingMessageId)?;
    let user_openid = raw
        .author
        .and_then(|author| {
            author
                .user_openid
                .or(author.openid)
                .or(author.member_openid)
                .or(author.id)
        })
        .or(raw.user_openid)
        .or(raw.openid)
        .filter(|value| !value.trim().is_empty())
        .ok_or(EventError::MissingUserOpenid)?;
    let base_content = raw.content.unwrap_or_default().trim().to_owned();
    let reply = extract_message_reply(&base_content, raw.reply.as_ref(), raw.quote.as_ref());
    Ok(Some(C2cMessage {
        message_id,
        user_openid,
        content: base_content,
        reply,
        timestamp: raw.timestamp,
        attachments: raw.attachments,
    }))
}

pub fn parse_group_message(envelope: &GatewayEnvelope) -> Result<Option<GroupMessage>, EventError> {
    if !matches!(
        envelope.t.as_deref(),
        Some(EVENT_GROUP_AT_MESSAGE_CREATE | EVENT_GROUP_MESSAGE_CREATE)
    ) {
        return Ok(None);
    }

    let raw = serde_json::from_value::<RawGroupMessage>(envelope.d.clone())?;
    let message_id = raw
        .id
        .or(raw.event_id)
        .filter(|value| !value.trim().is_empty())
        .ok_or(EventError::MissingMessageId)?;
    let group_openid = raw
        .group_openid
        .or(raw.group_id)
        .or(raw.openid)
        .filter(|value| !value.trim().is_empty())
        .ok_or(EventError::MissingGroupOpenid)?;
    let member_openid = raw
        .author
        .and_then(|author| {
            author
                .member_openid
                .or(author.user_openid)
                .or(author.openid)
                .or(author.id)
        })
        .or(raw.user_openid)
        .filter(|value| !value.trim().is_empty());
    let base_content = raw.content.unwrap_or_default().trim().to_owned();
    let reply = extract_message_reply(&base_content, raw.reply.as_ref(), raw.quote.as_ref());
    Ok(Some(GroupMessage {
        message_id,
        group_openid,
        member_openid,
        content: base_content,
        reply,
        timestamp: raw.timestamp,
        attachments: raw.attachments,
    }))
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

impl Attachment {
    pub fn note(&self) -> String {
        let content_type = self.content_type.as_deref().unwrap_or("unknown");
        let filename = self.filename.as_deref().unwrap_or("unnamed");
        let url = self.url.as_deref().unwrap_or("no-url");
        format!("[附件 {content_type}: {filename} {url}]")
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
    }

    #[test]
    fn parses_group_message_create() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-1",
                "group_openid": "group-1",
                "author": {"member_openid": "member-1"},
                "content": "早上好"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.message_id, "msg-1");
        assert_eq!(message.group_openid, "group-1");
        assert_eq!(message.member_openid.as_deref(), Some("member-1"));
        assert_eq!(message.content, "早上好");
    }

    #[test]
    fn parses_group_message_with_openid_fields_without_duplicate_error() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-1",
                "group_openid": "group-1",
                "openid": "group-1",
                "author": {"member_openid": "member-1", "openid": "member-1"},
                "content": "早上好"
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.group_openid, "group-1");
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

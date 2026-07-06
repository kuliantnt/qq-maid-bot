use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::warn;

use crate::gateway::logging::mask_openid;
use crate::gateway::ref_index::qq::{
    MSG_TYPE_QUOTE, RawMessageScene, RawMsgElement, parse_ref_indices,
};
use qq_maid_common::input_part::{MediaStatus, MessageInputPart, MessageMedia, QuotedMediaSummary};

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
    pub current_msg_idx: Option<String>,
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
    pub current_msg_idx: Option<String>,
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
    /// 被提及者的平台结构化 ID（群场景优先 member openid，其次 user openid / id）。
    /// 仅当平台事件未提供任何稳定 ID 时为 None。
    pub target_id: Option<String>,
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
    pub ref_msg_idx: Option<String>,
    pub content: Option<String>,
    pub input_parts: Vec<MessageInputPart>,
    pub media_summaries: Vec<QuotedMediaSummary>,
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
    message_scene: Option<RawMessageScene>,
    #[serde(default)]
    message_type: Option<u64>,
    #[serde(default)]
    msg_elements: Vec<RawMsgElement>,
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
    message_scene: Option<RawMessageScene>,
    #[serde(default)]
    message_type: Option<u64>,
    #[serde(default)]
    msg_elements: Vec<RawMsgElement>,
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
    // QQ mention 对象可能以下任一字段携带被提及者稳定 ID。
    #[serde(default)]
    member_openid: Option<String>,
    #[serde(default)]
    user_openid: Option<String>,
    #[serde(default)]
    openid: Option<String>,
    #[serde(default, alias = "id")]
    mention_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawMessageReply {
    #[serde(default, alias = "id")]
    message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct QuotedPayloadFallback {
    content: Option<String>,
    input_parts: Vec<MessageInputPart>,
    media_summaries: Vec<QuotedMediaSummary>,
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
    let parsed_content = parse_safe_content_parts(&raw.content.unwrap_or_default(), "qq_official");
    let base_content = parsed_content.text.trim().to_owned();
    let ref_indices = parse_ref_indices(
        raw.message_scene.as_ref(),
        raw.message_type,
        &raw.msg_elements,
    );
    let reply = extract_message_reply(
        &base_content,
        raw.reply.as_ref(),
        raw.quote.as_ref(),
        ref_indices.ref_msg_idx.clone(),
        quoted_payload_fallback(raw.message_type, &raw.msg_elements),
    );
    let timestamp = raw.timestamp;
    let input_parts = input_parts_from_content_and_attachments(
        &base_content,
        parsed_content.input_parts,
        &raw.attachments,
        "qq_official",
    );
    Ok(Some(C2cMessage {
        source_message_ids: vec![message_id.clone()],
        source_event_ids: event_id.iter().cloned().collect(),
        message_id,
        current_msg_idx: ref_indices.msg_idx,
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
    let parsed_content = parse_safe_content_parts(&raw.content.unwrap_or_default(), "qq_official");
    let base_content = parsed_content.text.trim().to_owned();
    let ref_indices = parse_ref_indices(
        raw.message_scene.as_ref(),
        raw.message_type,
        &raw.msg_elements,
    );
    let reply = extract_message_reply(
        &base_content,
        raw.reply.as_ref(),
        raw.quote.as_ref(),
        ref_indices.ref_msg_idx.clone(),
        quoted_payload_fallback(raw.message_type, &raw.msg_elements),
    );
    let input_parts = input_parts_from_content_and_attachments(
        &base_content,
        parsed_content.input_parts,
        &raw.attachments,
        "qq_official",
    );
    Ok(Some(GroupMessage {
        message_id,
        current_msg_idx: ref_indices.msg_idx,
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
        // 群场景优先 member openid，其次 user openid / openid / id；
        // 都缺失时返回 None，上游据此降级为 TextWeak 弱候选或丢弃。
        target_id: first_non_empty([
            mention.member_openid.as_deref(),
            mention.user_openid.as_deref(),
            mention.openid.as_deref(),
            mention.mention_id.as_deref(),
        ]),
    }
}

// reply 只提取一层 message_id，不递归解析引用消息正文或其它扩展字段。
fn extract_message_reply(
    content: &str,
    reply: Option<&RawMessageReply>,
    quote: Option<&RawMessageReply>,
    ref_msg_idx: Option<String>,
    fallback: QuotedPayloadFallback,
) -> Option<MessageReply> {
    let reference_id = reply
        .and_then(|item| item.message_id.as_deref())
        .or_else(|| quote.and_then(|item| item.message_id.as_deref()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| extract_cq_reply_message_id(content))
        .map(str::to_owned)
        .or_else(|| ref_msg_idx.clone());
    reference_id.map(|message_id| MessageReply {
        message_id,
        ref_msg_idx,
        content: fallback.content,
        input_parts: fallback.input_parts,
        media_summaries: fallback.media_summaries,
    })
}

fn quoted_payload_fallback(
    message_type: Option<u64>,
    msg_elements: &[RawMsgElement],
) -> QuotedPayloadFallback {
    if message_type != Some(MSG_TYPE_QUOTE) {
        return QuotedPayloadFallback::default();
    }
    let Some(element) = msg_elements.first() else {
        return QuotedPayloadFallback::default();
    };
    let raw_content = element.content.as_deref().unwrap_or_default();
    let parsed = parse_safe_content_parts(raw_content, "qq_official");
    let content = parsed.text.trim().to_owned();
    let input_parts = input_parts_from_content_and_attachments(
        &content,
        parsed.input_parts,
        &element.attachments,
        "qq_official",
    );
    let media_summaries = input_parts
        .iter()
        .filter_map(QuotedMediaSummary::from_input_part)
        .collect::<Vec<_>>();

    QuotedPayloadFallback {
        content: (!content.is_empty()).then_some(content),
        input_parts,
        media_summaries,
    }
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
        let mut media = MessageMedia {
            mime_type: self.content_type.clone(),
            filename: self.filename.clone(),
            size_bytes: self.size_bytes,
            url: self.url.clone(),
            local_path: None,
            media_id: self.media_id.clone(),
            file_id: self.file_id.clone(),
            attachment_id: self.attachment_id.clone(),
            platform: Some(platform.to_owned()),
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

fn input_parts_from_content_and_attachments(
    content: &str,
    parsed_parts: Vec<MessageInputPart>,
    attachments: &[Attachment],
    platform: &str,
) -> Vec<MessageInputPart> {
    let mut parts = Vec::new();
    if parsed_parts.is_empty() && !content.trim().is_empty() {
        parts.push(MessageInputPart::text(content.to_owned()));
    }
    let mut image_attachments = attachments
        .iter()
        .filter(|attachment| {
            attachment_kind(
                attachment.content_type.as_deref(),
                attachment.filename.as_deref(),
            ) == AttachmentKind::Image
        })
        .cloned();
    let mut trailing_parts = Vec::new();

    for part in parsed_parts {
        match part {
            MessageInputPart::Image { .. } => {
                if let Some(attachment) = image_attachments.next() {
                    parts.push(attachment.to_input_part(platform));
                } else {
                    parts.push(part);
                }
            }
            other => parts.push(other),
        }
    }

    trailing_parts.extend(image_attachments.map(|attachment| attachment.to_input_part(platform)));
    trailing_parts.extend(
        attachments
            .iter()
            .filter(|attachment| {
                attachment_kind(
                    attachment.content_type.as_deref(),
                    attachment.filename.as_deref(),
                ) != AttachmentKind::Image
            })
            .map(|attachment| attachment.to_input_part(platform)),
    );
    parts.extend(trailing_parts);
    parts
}

struct ParsedContentParts {
    text: String,
    input_parts: Vec<MessageInputPart>,
}

fn parse_safe_content_parts(content: &str, platform: &str) -> ParsedContentParts {
    let mut text = String::new();
    let mut input_parts = Vec::new();
    let mut rest = content;

    while let Some(start) = find_img_tag_start(rest) {
        text.push_str(&rest[..start]);
        push_text_part(&mut input_parts, &rest[..start]);
        let tag_rest = &rest[start..];
        let Some(end) = tag_rest.find('>') else {
            text.push_str(tag_rest);
            push_text_part(&mut input_parts, tag_rest);
            rest = "";
            break;
        };
        let tag = &tag_rest[..=end];
        if let Some(src) = extract_img_src(tag) {
            let filename =
                safe_filename_from_reference(src).unwrap_or_else(|| "unnamed".to_owned());
            let mut media = MessageMedia {
                mime_type: infer_image_mime_type(&filename),
                filename: Some(filename),
                url: Some(src.trim().to_owned()),
                platform: Some(platform.to_owned()),
                ..Default::default()
            };
            media.status = media.inferred_readability_status();
            text.push_str(&MessageInputPart::image(media.clone()).fallback_text());
            input_parts.push(MessageInputPart::image(media));
        } else {
            text.push_str("[图片 unknown: unnamed]");
            input_parts.push(MessageInputPart::image(MessageMedia {
                mime_type: Some("unknown".to_owned()),
                filename: Some("unnamed".to_owned()),
                platform: Some(platform.to_owned()),
                status: MediaStatus::MissingReadableUrl,
                ..Default::default()
            }));
        }
        rest = &tag_rest[end + 1..];
    }
    text.push_str(rest);
    push_text_part(&mut input_parts, rest);

    ParsedContentParts { text, input_parts }
}

fn push_text_part(parts: &mut Vec<MessageInputPart>, text: &str) {
    if !text.trim().is_empty() {
        parts.push(MessageInputPart::text(text.to_owned()));
    }
}

fn find_img_tag_start(text: &str) -> Option<usize> {
    text.as_bytes()
        .windows(4)
        .position(|window| window.eq_ignore_ascii_case(b"<img"))
}

fn extract_img_src(tag: &str) -> Option<&str> {
    let bytes = tag.as_bytes();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        if bytes[index..index + 3].eq_ignore_ascii_case(b"src") && is_attr_boundary(bytes, index) {
            let mut cursor = index + 3;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() || bytes[cursor] != b'=' {
                index += 3;
                continue;
            }
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() {
                return None;
            }
            let quote = bytes[cursor];
            if matches!(quote, b'"' | b'\'') {
                cursor += 1;
                let start = cursor;
                while cursor < bytes.len() && bytes[cursor] != quote {
                    cursor += 1;
                }
                return tag.get(start..cursor);
            }
            let start = cursor;
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && !matches!(bytes[cursor], b'>' | b'/')
            {
                cursor += 1;
            }
            return tag.get(start..cursor);
        }
        index += 1;
    }
    None
}

fn is_attr_boundary(bytes: &[u8], index: usize) -> bool {
    (index == 0 || bytes[index - 1].is_ascii_whitespace())
        && bytes[index..index + 3].eq_ignore_ascii_case(b"src")
        && bytes
            .get(index + 3)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'=')
}

fn safe_filename_from_reference(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches(['/', '\\']);
    if value.is_empty() {
        return None;
    }
    let path = value.split(['?', '#']).next().unwrap_or(value);
    path.rsplit(['/', '\\'])
        .next()
        .map(str::trim)
        .filter(|filename| !filename.is_empty())
        .map(str::to_owned)
}

fn infer_image_mime_type(filename: &str) -> Option<String> {
    let extension = filename.rsplit('.').next()?.to_ascii_lowercase();
    let mime = match extension.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => "image/unknown",
    };
    Some(mime.to_owned())
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
    fn c2c_img_file_url_content_is_sanitized_and_kept_unreadable() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-file-image",
                "author": {"user_openid": "user-1"},
                "content": r#"<img src="file://C:\Users\ThinkPad\Documents\Tencent Files\123\Image\a.jpg" />抱抱你"#
            }),
        };

        let message = parse_c2c_message(&envelope).unwrap().unwrap();
        let fallback = message
            .input_parts
            .iter()
            .map(MessageInputPart::fallback_text)
            .collect::<Vec<_>>()
            .join("");

        assert_eq!(message.content, "[图片 image/jpeg: a.jpg]抱抱你");
        assert_eq!(fallback, "[图片 image/jpeg: a.jpg]抱抱你");
        assert!(!message.content.contains("C:\\Users"));
        assert!(!fallback.contains("Tencent Files"));
        assert!(matches!(
            &message.input_parts[0],
            MessageInputPart::Image { media }
                if media.filename.as_deref() == Some("a.jpg")
                    && media.remote_url().is_none()
                    && media.status == MediaStatus::MissingReadableUrl
        ));
        assert_eq!(message.input_parts[1].text_content(), Some("抱抱你"));
    }

    #[test]
    fn c2c_img_placeholders_reuse_attachment_slots_without_duplicates() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_C2C_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-mixed-images",
                "author": {"user_openid": "user-1"},
                "content": r#"前<img src="file://C:\Images\a.png" />中<img src="file://C:\Images\b.webp" />后"#,
                "attachments": [
                    {
                        "content_type": "image",
                        "filename": "a.png",
                        "url": "https://example.test/a.png"
                    },
                    {
                        "content_type": "image/webp",
                        "filename": "b.webp",
                        "url": "https://example.test/b.webp"
                    }
                ]
            }),
        };

        let message = parse_c2c_message(&envelope).unwrap().unwrap();

        assert_eq!(message.input_parts.len(), 5);
        assert_eq!(message.input_parts[0].text_content(), Some("前"));
        assert_eq!(message.input_parts[2].text_content(), Some("中"));
        assert_eq!(message.input_parts[4].text_content(), Some("后"));
        assert_eq!(
            message
                .input_parts
                .iter()
                .filter(|part| matches!(part, MessageInputPart::Image { .. }))
                .count(),
            2
        );
        let MessageInputPart::Image { media: first } = &message.input_parts[1] else {
            panic!("expected first image part");
        };
        let MessageInputPart::Image { media: second } = &message.input_parts[3] else {
            panic!("expected second image part");
        };
        assert_eq!(first.remote_url(), Some("https://example.test/a.png"));
        assert_eq!(first.mime_type.as_deref(), Some("image"));
        assert_eq!(second.remote_url(), Some("https://example.test/b.webp"));
        assert_eq!(second.mime_type.as_deref(), Some("image/webp"));
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
                    member_role: Some(GroupMemberRole::Owner),
                    target_id: Some("owner-id".to_owned())
                },
                GroupMention {
                    is_you: true,
                    member_role: Some(GroupMemberRole::Admin),
                    target_id: Some("appid".to_owned())
                },
                GroupMention {
                    is_you: false,
                    member_role: Some(GroupMemberRole::Member),
                    target_id: Some("user-openid".to_owned())
                },
                GroupMention {
                    is_you: false,
                    member_role: Some(GroupMemberRole::Unknown),
                    target_id: Some("member-openid".to_owned())
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
                ref_msg_idx: None,
                content: None,
                input_parts: Vec::new(),
                media_summaries: Vec::new(),
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
                ref_msg_idx: None,
                content: None,
                input_parts: Vec::new(),
                media_summaries: Vec::new(),
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
                ref_msg_idx: None,
                content: None,
                input_parts: Vec::new(),
                media_summaries: Vec::new(),
            })
        );
    }

    #[test]
    fn parses_group_refidx_from_message_scene_ext() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-current",
                "group_openid": "group-1",
                "author": {"member_openid": "member-1"},
                "content": "这条是什么意思",
                "message_scene": {
                    "ext": [
                        "msg_idx=REFIDX_current",
                        "ref_msg_idx=REFIDX_quoted"
                    ]
                }
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();

        assert_eq!(message.current_msg_idx.as_deref(), Some("REFIDX_current"));
        assert_eq!(
            message.reply,
            Some(MessageReply {
                message_id: "REFIDX_quoted".to_owned(),
                ref_msg_idx: Some("REFIDX_quoted".to_owned()),
                content: None,
                input_parts: Vec::new(),
                media_summaries: Vec::new(),
            })
        );
    }

    #[test]
    fn parses_qq_quote_msg_element_as_payload_fallback() {
        let envelope = GatewayEnvelope {
            op: 0,
            s: Some(42),
            t: Some(EVENT_GROUP_AT_MESSAGE_CREATE.to_owned()),
            id: None,
            d: json!({
                "id": "msg-current",
                "group_openid": "group-1",
                "author": {"member_openid": "member-1"},
                "content": "查看这条",
                "message_type": 103,
                "message_scene": {
                    "ext": ["msg_idx=REFIDX_current"]
                },
                "msg_elements": [{
                    "msg_idx": "REFIDX_quoted",
                    "content": "被引用原文",
                    "attachments": [{
                        "content_type": "image/png",
                        "filename": "quoted.png",
                        "url": "https://example.test/quoted.png"
                    }]
                }]
            }),
        };

        let message = parse_group_message(&envelope).unwrap().unwrap();
        let reply = message.reply.unwrap();

        assert_eq!(message.current_msg_idx.as_deref(), Some("REFIDX_current"));
        assert_eq!(reply.ref_msg_idx.as_deref(), Some("REFIDX_quoted"));
        assert_eq!(reply.message_id, "REFIDX_quoted");
        assert_eq!(reply.content.as_deref(), Some("被引用原文"));
        assert_eq!(reply.input_parts.len(), 2);
        assert_eq!(reply.media_summaries.len(), 1);
        assert!(matches!(
            reply.input_parts[1],
            MessageInputPart::Image { .. }
        ));
    }
}

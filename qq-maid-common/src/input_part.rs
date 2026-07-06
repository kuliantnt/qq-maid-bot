//! 平台无关的入站消息内容块。
//!
//! 一次用户输入可能由多段文字、图片或文件组成。本模块只描述顺序和元信息，
//! 不承载 OCR、票据识别或任何业务语义，便于 gateway、core 和 LLM 层复用。

use serde::{Deserialize, Serialize};

use crate::identity_context::MessageActorContext;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageInputPart {
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<TextSource>,
    },
    Image {
        media: MessageMedia,
    },
    File {
        media: MessageMedia,
    },
    Unknown {
        media: MessageMedia,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TextSource {
    Body,
    Caption,
    Quote,
    /// 系统提供的当前消息上下文，不属于用户原文。
    Context,
    Supplement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MessageMedia {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default)]
    pub status: MediaStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct QuotedMessageContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_msg_idx: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_msg_idx: Option<String>,
    #[serde(default)]
    pub lookup_found: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_summaries: Vec<QuotedMediaSummary>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_parts: Vec<MessageInputPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_bot: Option<bool>,
    /// 引用消息发送者身份摘要；ref_index 命中时回填，缺失时为 None。
    /// 与 `from_bot` 并存：`from_bot` 仅区分 bot/user/unknown，`sender` 携带稳定 ID 等更多信息。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<MessageActorContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuotedMediaSummary {
    pub kind: QuotedMediaKind,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MessageMedia>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuotedMediaKind {
    Image,
    File,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MediaStatus {
    #[default]
    Available,
    MissingReadableUrl,
    SizeExceeded,
    UnsupportedType,
    DownloadFailed,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaUrlScheme {
    Http,
    Https,
    File,
    LocalPath,
    Empty,
    Other,
    Missing,
}

impl MediaUrlScheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::File => "file",
            Self::LocalPath => "local_path",
            Self::Empty => "empty",
            Self::Other => "other",
            Self::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    File,
    Unknown,
}

impl MessageInputPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            source: Some(TextSource::Body),
        }
    }

    pub fn image(media: MessageMedia) -> Self {
        Self::Image { media }
    }

    pub fn file(media: MessageMedia) -> Self {
        Self::File { media }
    }

    pub fn unknown(media: MessageMedia, reason: impl Into<String>) -> Self {
        Self::Unknown {
            media,
            reason: Some(reason.into()),
        }
    }

    pub fn text_content(&self) -> Option<&str> {
        match self {
            Self::Text { text, .. } => Some(text),
            Self::Image { .. } | Self::File { .. } | Self::Unknown { .. } => None,
        }
    }

    pub fn media_kind(&self) -> Option<MediaKind> {
        match self {
            Self::Text { .. } => None,
            Self::Image { .. } => Some(MediaKind::Image),
            Self::File { .. } => Some(MediaKind::File),
            Self::Unknown { .. } => Some(MediaKind::Unknown),
        }
    }

    pub fn media(&self) -> Option<&MessageMedia> {
        match self {
            Self::Text { .. } => None,
            Self::Image { media } | Self::File { media } | Self::Unknown { media, .. } => {
                Some(media)
            }
        }
    }

    pub fn is_non_text(&self) -> bool {
        !matches!(self, Self::Text { .. })
    }

    pub fn fallback_text(&self) -> String {
        match self {
            Self::Text { text, .. } => text.clone(),
            Self::Image { media } => format_media_note("图片", media),
            Self::File { media } => format_media_note("文件", media),
            Self::Unknown { media, .. } => format_media_note("附件", media),
        }
    }
}

impl QuotedMessageContext {
    pub fn fallback_text(&self) -> String {
        let mut lines = Vec::new();
        let from = match self.from_bot {
            Some(true) => "bot",
            Some(false) => "user",
            None => "unknown",
        };
        let reference = self
            .ref_msg_idx
            .as_deref()
            .or(self.reference_id.as_deref())
            .unwrap_or("unknown");
        // 若 ref_index 回填了 sender，优先展示稳定身份摘要；否则回退到 from_bot 摘要。
        let sender_summary = self.sender.as_ref().map(|sender| {
            let display = sender
                .display_name
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("unknown");
            let uid = sender
                .user_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("unknown");
            let is_bot = sender
                .is_bot
                .map(|value| if value { "true" } else { "false" })
                .unwrap_or("unknown");
            format!(
                "昵称={display}，稳定ID={uid}，是否机器人={is_bot}，身份来源={}",
                sender.source.as_str()
            )
        });
        lines.push(format!(
            "Quoted Context: reference={reference}, from={from}"
        ));
        if let Some(summary) = sender_summary {
            lines.push(format!("引用发送者：{summary}"));
        }
        if self.lookup_found {
            if let Some(text) = self
                .text_summary
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            {
                lines.push(format!("引用文本：{text}"));
            }
            for media in &self.media_summaries {
                if !media.summary.trim().is_empty() {
                    lines.push(format!("引用媒体：{}", media.summary));
                }
            }
            if lines.len() == 1 {
                lines.push("引用消息为空或只有暂不可读内容。".to_owned());
            }
        } else {
            let reason = self
                .fallback_reason
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("not_found");
            lines.push(format!("引用内容不可用：{reason}"));
        }
        lines.join("\n")
    }
}

impl QuotedMediaSummary {
    pub fn from_input_part(part: &MessageInputPart) -> Option<Self> {
        let kind = match part {
            MessageInputPart::Text { .. } => return None,
            MessageInputPart::Image { .. } => QuotedMediaKind::Image,
            MessageInputPart::File { .. } => QuotedMediaKind::File,
            MessageInputPart::Unknown { .. } => QuotedMediaKind::Unknown,
        };
        Some(Self {
            kind,
            summary: part.fallback_text(),
            media: part.media().cloned(),
        })
    }
}

impl MessageMedia {
    pub fn remote_url(&self) -> Option<&str> {
        self.url.as_deref().map(str::trim).filter(|value| {
            matches!(
                media_url_scheme(Some(value)),
                MediaUrlScheme::Http | MediaUrlScheme::Https
            )
        })
    }

    pub fn has_fetchable_reference(&self) -> bool {
        self.local_path
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || self.remote_url().is_some()
            || self
                .media_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || self
                .file_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || self
                .attachment_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
    }

    pub fn url_scheme(&self) -> MediaUrlScheme {
        media_url_scheme(self.url.as_deref())
    }

    pub fn inferred_readability_status(&self) -> MediaStatus {
        if self
            .local_path
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || self.remote_url().is_some()
        {
            MediaStatus::Available
        } else {
            MediaStatus::MissingReadableUrl
        }
    }
}

fn format_media_note(label: &str, media: &MessageMedia) -> String {
    let mime = media.mime_type.as_deref().unwrap_or("unknown");
    let filename = media.filename.as_deref().unwrap_or("unnamed");
    format!("[{label} {mime}: {filename}]")
}

fn media_url_scheme(url: Option<&str>) -> MediaUrlScheme {
    let Some(value) = url.map(str::trim) else {
        return MediaUrlScheme::Missing;
    };
    if value.is_empty() {
        return MediaUrlScheme::Empty;
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return MediaUrlScheme::Https;
    }
    if lower.starts_with("http://") {
        return MediaUrlScheme::Http;
    }
    if lower.starts_with("file://") {
        return MediaUrlScheme::File;
    }
    if looks_like_windows_local_path(value) {
        return MediaUrlScheme::LocalPath;
    }
    MediaUrlScheme::Other
}

fn looks_like_windows_local_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 3 {
        return false;
    }
    bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/') && bytes[0].is_ascii_alphabetic()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_text_omits_sensitive_url() {
        let media = MessageMedia {
            mime_type: Some("image/jpeg".to_owned()),
            filename: Some("ticket.jpg".to_owned()),
            url: Some("https://example.test/secret-token".to_owned()),
            ..Default::default()
        };

        assert_eq!(
            MessageInputPart::image(media).fallback_text(),
            "[图片 image/jpeg: ticket.jpg]"
        );
    }

    #[test]
    fn remote_url_only_allows_http_and_https() {
        let mut media = MessageMedia {
            url: Some(" https://example.test/a.jpg ".to_owned()),
            ..Default::default()
        };
        assert_eq!(media.remote_url(), Some("https://example.test/a.jpg"));
        assert_eq!(media.url_scheme(), MediaUrlScheme::Https);

        media.url = Some("http://example.test/a.jpg".to_owned());
        assert_eq!(media.remote_url(), Some("http://example.test/a.jpg"));
        assert_eq!(media.url_scheme(), MediaUrlScheme::Http);

        for value in [
            "",
            "file://C:\\Users\\ThinkPad\\Documents\\Tencent Files\\a.jpg",
            "C:\\Users\\ThinkPad\\Pictures\\a.jpg",
            "ftp://example.test/a.jpg",
        ] {
            media.url = Some(value.to_owned());
            assert_eq!(media.remote_url(), None, "{value}");
        }
    }
}

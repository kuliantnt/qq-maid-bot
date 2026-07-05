//! 平台无关的入站消息内容块。
//!
//! 一次用户输入可能由多段文字、图片或文件组成。本模块只描述顺序和元信息，
//! 不承载 OCR、票据识别或任何业务语义，便于 gateway、core 和 LLM 层复用。

use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MediaStatus {
    #[default]
    Available,
    SizeExceeded,
    UnsupportedType,
    DownloadFailed,
    Expired,
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

impl MessageMedia {
    pub fn remote_url(&self) -> Option<&str> {
        self.url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub fn has_fetchable_reference(&self) -> bool {
        self.remote_url().is_some()
            || self
                .media_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || self
                .file_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
    }
}

fn format_media_note(label: &str, media: &MessageMedia) -> String {
    let mime = media.mime_type.as_deref().unwrap_or("unknown");
    let filename = media.filename.as_deref().unwrap_or("unnamed");
    format!("[{label} {mime}: {filename}]")
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
}

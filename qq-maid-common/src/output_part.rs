//! 平台无关的出站内容模型。
//!
//! 与入站 [`crate::input_part::MessageInputPart`] 对称，这里只描述助手回复的
//! 顺序化内容块和纯展示元信息，不承载平台发送能力判断、fallback 策略或业务
//! 语义，便于 gateway、core 和 LLM 层复用。
//!
//! `text_fallback` 是所有平台都可降级发送的纯文本；`markdown` 保留结构化排版
//! 通道；`parts` 为图片、文件、卡片等顺序化出站内容载体。

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantOutput {
    pub text_fallback: String,
    pub markdown: Option<String>,
    pub parts: Vec<OutputPart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputPart {
    Text { text: String },
    Markdown { markdown: String },
    Image { media: OutputMedia },
    File { media: OutputMedia },
}

/// 出站媒体占位信息。
///
/// 作为结构化输出契约存在，平台能力判断和发送由 Gateway render 层负责，
/// 本结构不要求 Gateway 立即接入发送。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputMedia {
    pub mime_type: Option<String>,
    pub filename: Option<String>,
    pub size_bytes: Option<u64>,
    pub url: Option<String>,
    pub media_id: Option<String>,
    pub file_id: Option<String>,
    pub platform: Option<String>,
    pub fallback_text: Option<String>,
}

impl AssistantOutput {
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            text_fallback: text.clone(),
            markdown: None,
            parts: non_empty_output_parts([OutputPart::Text { text }]),
        }
    }

    pub fn markdown(text_fallback: impl Into<String>, markdown: impl Into<String>) -> Self {
        let text_fallback = text_fallback.into();
        let markdown = markdown.into();
        let mut parts = Vec::new();
        if !markdown.trim().is_empty() {
            parts.push(OutputPart::Markdown {
                markdown: markdown.clone(),
            });
        }
        if parts.is_empty() && !text_fallback.trim().is_empty() {
            parts.push(OutputPart::Text {
                text: text_fallback.clone(),
            });
        }
        Self {
            text_fallback,
            markdown: Some(markdown),
            parts,
        }
    }
}

impl OutputPart {
    fn is_empty(&self) -> bool {
        match self {
            Self::Text { text } => text.trim().is_empty(),
            Self::Markdown { markdown } => markdown.trim().is_empty(),
            Self::Image { .. } | Self::File { .. } => false,
        }
    }
}

fn non_empty_output_parts(parts: impl IntoIterator<Item = OutputPart>) -> Vec<OutputPart> {
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

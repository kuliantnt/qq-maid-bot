use crate::{markdown::MarkdownPayload, media::ImagePayload, respond::RespondResponse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundMessage {
    Text {
        text: String,
    },
    Markdown {
        markdown: MarkdownPayload,
        fallback_text: String,
    },
    Image {
        image: ImagePayload,
        fallback_text: String,
    },
}

impl OutboundMessage {
    pub fn fallback_text(&self) -> &str {
        match self {
            Self::Text { text } => text,
            Self::Markdown { fallback_text, .. } | Self::Image { fallback_text, .. } => {
                fallback_text
            }
        }
    }
}

pub fn render_respond_response(
    response: &RespondResponse,
    enable_markdown: bool,
    _enable_image: bool,
) -> Option<OutboundMessage> {
    let text = response.text.as_ref()?;
    if text.trim().is_empty() {
        return None;
    }
    if enable_markdown {
        return Some(OutboundMessage::Markdown {
            markdown: MarkdownPayload::new(text.clone()),
            fallback_text: text.clone(),
        });
    }
    Some(OutboundMessage::Text { text: text.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_with_text(text: Option<&str>) -> RespondResponse {
        RespondResponse {
            ok: true,
            text: text.map(str::to_owned),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            error: None,
        }
    }

    /// 合并 2 个 render_respond_response 测试为表驱动测试。
    #[test]
    fn respond_text_renders_to_appropriate_message_kind() {
        struct Case {
            name: &'static str,
            text: Option<&'static str>,
            enable_markdown: bool,
            expected: OutboundMessage,
        }

        let cases = [
            Case {
                name: "respond_text_renders_to_text_message_when_markdown_disabled",
                text: Some("hello"),
                enable_markdown: false,
                expected: OutboundMessage::Text {
                    text: "hello".to_owned(),
                },
            },
            Case {
                name: "respond_text_renders_to_markdown_message_when_markdown_enabled",
                text: Some("  hello **qq**\n"),
                enable_markdown: true,
                expected: OutboundMessage::Markdown {
                    markdown: MarkdownPayload::new("  hello **qq**\n"),
                    fallback_text: "  hello **qq**\n".to_owned(),
                },
            },
        ];

        for case in &cases {
            let response = response_with_text(case.text);
            let actual = render_respond_response(&response, case.enable_markdown, true);
            assert_eq!(
                actual,
                Some(case.expected.clone()),
                "case '{}' failed: rendered message mismatch",
                case.name
            );
        }
    }

    #[test]
    fn empty_respond_text_renders_to_none() {
        assert_eq!(
            render_respond_response(&response_with_text(Some(" \n\t")), true, true),
            None
        );
        assert_eq!(
            render_respond_response(&response_with_text(None), true, true),
            None
        );
    }
}

use crate::{
    gateway::outbound::RenderProfile, markdown::MarkdownPayload, media::ImagePayload,
    respond::RespondResponse,
};

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
    ImagePlaceholder {
        fallback_text: String,
    },
    AttachmentPlaceholder {
        fallback_text: String,
    },
}

impl OutboundMessage {
    pub fn fallback_text(&self) -> &str {
        match self {
            Self::Text { text } => text,
            Self::Markdown { fallback_text, .. }
            | Self::Image { fallback_text, .. }
            | Self::ImagePlaceholder { fallback_text }
            | Self::AttachmentPlaceholder { fallback_text } => fallback_text,
        }
    }

    /// 群 at 回复需要在 QQ 出站边界补充平台提及语法；富媒体和 fallback 文本保持一致。
    pub fn prefix_text(self, prefix: &str) -> Self {
        fn join(prefix: &str, text: String) -> String {
            if text.trim().is_empty() {
                prefix.to_owned()
            } else {
                format!("{prefix}\n{text}")
            }
        }

        match self {
            Self::Text { text } => Self::Text {
                text: join(prefix, text),
            },
            Self::Markdown {
                markdown,
                fallback_text,
            } => Self::Markdown {
                markdown: MarkdownPayload::new(join(prefix, markdown.content)),
                fallback_text: join(prefix, fallback_text),
            },
            Self::Image {
                image,
                fallback_text,
            } => Self::Image {
                image,
                fallback_text: join(prefix, fallback_text),
            },
            Self::ImagePlaceholder { fallback_text } => Self::ImagePlaceholder {
                fallback_text: join(prefix, fallback_text),
            },
            Self::AttachmentPlaceholder { fallback_text } => Self::AttachmentPlaceholder {
                fallback_text: join(prefix, fallback_text),
            },
        }
    }
}

pub fn render_respond_response(
    response: &RespondResponse,
    enable_markdown: bool,
    enable_image: bool,
) -> Option<OutboundMessage> {
    let profile = RenderProfile {
        supports_text: true,
        supports_markdown: enable_markdown,
        supports_image: enable_image,
        supports_attachment: false,
        unsupported_fallback: crate::gateway::outbound::UnsupportedCapabilityFallback::UseText,
    };
    render_respond_response_for_profile(response, &profile)
}

pub(crate) fn render_respond_response_for_profile(
    response: &RespondResponse,
    profile: &RenderProfile,
) -> Option<OutboundMessage> {
    let text = response.text.as_ref()?;
    if text.trim().is_empty() {
        return None;
    }
    if profile.supports_markdown
        && let Some(markdown) = response.markdown.as_ref()
        && !markdown.trim().is_empty()
    {
        return Some(OutboundMessage::Markdown {
            markdown: MarkdownPayload::new(markdown.clone()),
            fallback_text: text.clone(),
        });
    }
    profile
        .supports_text
        .then(|| OutboundMessage::Text { text: text.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_with_body(text: Option<&str>, markdown: Option<&str>) -> RespondResponse {
        RespondResponse {
            text: text.map(str::to_owned),
            markdown: markdown.map(str::to_owned),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        }
    }

    /// 合并 2 个 render_respond_response 测试为表驱动测试。
    #[test]
    fn respond_text_renders_to_appropriate_message_kind() {
        struct Case {
            name: &'static str,
            text: Option<&'static str>,
            markdown: Option<&'static str>,
            enable_markdown: bool,
            expected: OutboundMessage,
        }

        let cases = [
            Case {
                name: "respond_text_renders_to_text_message_when_markdown_disabled",
                text: Some("hello"),
                markdown: Some("# hello"),
                enable_markdown: false,
                expected: OutboundMessage::Text {
                    text: "hello".to_owned(),
                },
            },
            Case {
                name: "respond_markdown_renders_to_markdown_message_when_markdown_enabled",
                text: Some("hello qq"),
                markdown: Some("  hello **qq**\n"),
                enable_markdown: true,
                expected: OutboundMessage::Markdown {
                    markdown: MarkdownPayload::new("  hello **qq**\n"),
                    fallback_text: "hello qq".to_owned(),
                },
            },
            Case {
                name: "respond_without_markdown_falls_back_to_text_when_markdown_enabled",
                text: Some("hello"),
                markdown: None,
                enable_markdown: true,
                expected: OutboundMessage::Text {
                    text: "hello".to_owned(),
                },
            },
            Case {
                name: "blank_markdown_falls_back_to_text_when_markdown_enabled",
                text: Some("hello"),
                markdown: Some("  \n\t"),
                enable_markdown: true,
                expected: OutboundMessage::Text {
                    text: "hello".to_owned(),
                },
            },
        ];

        for case in &cases {
            let response = response_with_body(case.text, case.markdown);
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
    fn profile_without_markdown_degrades_to_text() {
        let profile = RenderProfile::text_only_sync();
        let response = response_with_body(Some("hello"), Some("**hello**"));

        assert_eq!(
            render_respond_response_for_profile(&response, &profile),
            Some(OutboundMessage::Text {
                text: "hello".to_owned()
            })
        );
    }

    #[test]
    fn empty_respond_text_renders_to_none() {
        assert_eq!(
            render_respond_response(&response_with_body(Some(" \n\t"), Some("# hi")), true, true),
            None
        );
        assert_eq!(
            render_respond_response(&response_with_body(None, Some("# hi")), true, true),
            None
        );
    }

    #[test]
    fn prefix_text_updates_markdown_and_fallback() {
        let outbound = OutboundMessage::Markdown {
            markdown: MarkdownPayload::new("**正文**"),
            fallback_text: "正文".to_owned(),
        }
        .prefix_text("<@member-1>");

        assert_eq!(
            outbound,
            OutboundMessage::Markdown {
                markdown: MarkdownPayload::new("<@member-1>\n**正文**"),
                fallback_text: "<@member-1>\n正文".to_owned(),
            }
        );
    }
}

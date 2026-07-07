use crate::{
    gateway::outbound::RenderProfile, markdown::MarkdownPayload, media::ImagePayload,
    respond::RespondResponse,
};
use qq_maid_common::markdown_strip::strip_markdown_for_chat;
use qq_maid_core::service::{AssistantOutput, OutputMedia, OutputPart};

const UNSUPPORTED_IMAGE_FALLBACK_TEXT: &str = "当前平台暂不支持发送这类图片内容。";
const UNSUPPORTED_FILE_FALLBACK_TEXT: &str = "当前平台暂不支持发送这类文件内容。";

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
    if let Some(output) = response.output.as_ref()
        && !output.parts.is_empty()
    {
        return render_assistant_output_for_profile(output, profile);
    }

    let text = response
        .text
        .as_ref()
        .or_else(|| response.output.as_ref().map(|output| &output.text_fallback))?;
    if text.trim().is_empty() {
        return None;
    }
    if profile.supports_markdown
        && let Some(markdown) = response.markdown.as_ref().or_else(|| {
            response
                .output
                .as_ref()
                .and_then(|output| output.markdown.as_ref())
        })
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

fn render_assistant_output_for_profile(
    output: &AssistantOutput,
    profile: &RenderProfile,
) -> Option<OutboundMessage> {
    let fallback_text = normalized_output_fallback(output)?;

    if profile.supports_markdown
        && output
            .parts
            .iter()
            .any(|part| matches!(part, OutputPart::Markdown { .. }))
    {
        let markdown = render_parts_as_markdown(output);
        if !markdown.trim().is_empty() {
            return Some(OutboundMessage::Markdown {
                markdown: MarkdownPayload::new(markdown),
                fallback_text,
            });
        }
    }

    profile.supports_text.then(|| OutboundMessage::Text {
        text: render_parts_as_text(output),
    })
}

fn normalized_output_fallback(output: &AssistantOutput) -> Option<String> {
    let text = output.text_fallback.trim();
    if !text.is_empty() {
        return Some(output.text_fallback.clone());
    }

    let text = output
        .parts
        .iter()
        .filter_map(part_text_fallback)
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.trim().is_empty()).then_some(text)
}

fn render_parts_as_text(output: &AssistantOutput) -> String {
    normalized_output_fallback(output).unwrap_or_default()
}

fn render_parts_as_markdown(output: &AssistantOutput) -> String {
    let markdown = output
        .parts
        .iter()
        .filter_map(part_markdown_render)
        .collect::<Vec<_>>()
        .join("\n\n");
    if markdown.trim().is_empty() {
        output
            .markdown
            .clone()
            .unwrap_or_else(|| output.text_fallback.clone())
    } else {
        markdown
    }
}

fn part_text_fallback(part: &OutputPart) -> Option<String> {
    let text = match part {
        OutputPart::Text { text } => text.clone(),
        OutputPart::Markdown { markdown } => strip_markdown_for_chat(markdown),
        OutputPart::Image { media } => media_fallback_text(media, UNSUPPORTED_IMAGE_FALLBACK_TEXT),
        OutputPart::File { media } => media_fallback_text(media, UNSUPPORTED_FILE_FALLBACK_TEXT),
    };
    (!text.trim().is_empty()).then_some(text)
}

fn part_markdown_render(part: &OutputPart) -> Option<String> {
    let text = match part {
        OutputPart::Text { text } => text.clone(),
        OutputPart::Markdown { markdown } => markdown.clone(),
        OutputPart::Image { media } => media_fallback_text(media, UNSUPPORTED_IMAGE_FALLBACK_TEXT),
        OutputPart::File { media } => media_fallback_text(media, UNSUPPORTED_FILE_FALLBACK_TEXT),
    };
    (!text.trim().is_empty()).then_some(text)
}

fn media_fallback_text(media: &OutputMedia, default_text: &'static str) -> String {
    media
        .fallback_text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(default_text)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response_with_body(text: Option<&str>, markdown: Option<&str>) -> RespondResponse {
        RespondResponse {
            output: None,
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

    #[test]
    fn structured_output_parts_render_markdown_when_supported() {
        let response = RespondResponse {
            output: Some(AssistantOutput {
                text_fallback: "plain fallback".to_owned(),
                markdown: None,
                parts: vec![
                    OutputPart::Text {
                        text: "hello *plain*".to_owned(),
                    },
                    OutputPart::Markdown {
                        markdown: "## title\n- item".to_owned(),
                    },
                ],
            }),
            text: Some("legacy text ignored".to_owned()),
            markdown: Some("legacy markdown ignored".to_owned()),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            render_respond_response(&response, true, true),
            Some(OutboundMessage::Markdown {
                markdown: MarkdownPayload::new("hello *plain*\n\n## title\n- item"),
                fallback_text: "plain fallback".to_owned(),
            })
        );
    }

    #[test]
    fn structured_output_degrades_to_text_for_text_only_profile() {
        let response = RespondResponse {
            output: Some(AssistantOutput {
                text_fallback: String::new(),
                markdown: None,
                parts: vec![
                    OutputPart::Markdown {
                        markdown: "# 标题\n- item".to_owned(),
                    },
                    OutputPart::Image {
                        media: OutputMedia {
                            fallback_text: Some("图片：天气雷达".to_owned()),
                            ..OutputMedia::default()
                        },
                    },
                ],
            }),
            text: None,
            markdown: None,
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            render_respond_response_for_profile(&response, &RenderProfile::text_only_sync()),
            Some(OutboundMessage::Text {
                text: "标题\n· item\n\n图片：天气雷达".to_owned(),
            })
        );
    }

    #[test]
    fn unsupported_structured_part_uses_explicit_fallback_text() {
        let response = RespondResponse {
            output: Some(AssistantOutput {
                text_fallback: String::new(),
                markdown: None,
                parts: vec![OutputPart::File {
                    media: OutputMedia::default(),
                }],
            }),
            text: None,
            markdown: None,
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            render_respond_response(&response, true, true),
            Some(OutboundMessage::Text {
                text: UNSUPPORTED_FILE_FALLBACK_TEXT.to_owned(),
            })
        );
    }

    #[test]
    fn output_with_empty_parts_uses_legacy_compat_fields() {
        let response = RespondResponse {
            output: Some(AssistantOutput {
                text_fallback: "output fallback".to_owned(),
                markdown: Some("**output markdown**".to_owned()),
                parts: Vec::new(),
            }),
            text: None,
            markdown: None,
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            render_respond_response(&response, true, true),
            Some(OutboundMessage::Markdown {
                markdown: MarkdownPayload::new("**output markdown**"),
                fallback_text: "output fallback".to_owned(),
            })
        );
    }
}

use crate::{
    gateway::outbound::RenderProfile, markdown::MarkdownPayload, media::ImagePayload,
    respond::RespondResponse,
};
use qq_maid_core::service::{AssistantOutput, OutputPart};

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

pub(crate) fn render_respond_response_for_profile(
    response: &RespondResponse,
    profile: &RenderProfile,
) -> Option<OutboundMessage> {
    let output = response.output.as_ref()?;
    render_assistant_output_for_profile(output, profile)
}

fn render_assistant_output_for_profile(
    output: &AssistantOutput,
    profile: &RenderProfile,
) -> Option<OutboundMessage> {
    // 用户可见纯文本 fallback（媒体缺文案时使用平台默认文案），全空时整体不渲染。
    let fallback_text = output.render_text_fallback(
        UNSUPPORTED_IMAGE_FALLBACK_TEXT,
        UNSUPPORTED_FILE_FALLBACK_TEXT,
    )?;

    if profile.supports_markdown && output_has_markdown_channel(output) {
        // 按 parts 拼接 Markdown（媒体 fallback 同样使用平台默认文案）；非空才出 Markdown。
        let markdown = output.render_markdown(
            UNSUPPORTED_IMAGE_FALLBACK_TEXT,
            UNSUPPORTED_FILE_FALLBACK_TEXT,
        );
        if !markdown.trim().is_empty() {
            return Some(OutboundMessage::Markdown {
                markdown: MarkdownPayload::new(markdown),
                fallback_text,
            });
        }
    }

    profile.supports_text.then_some(OutboundMessage::Text {
        text: fallback_text,
    })
}

fn output_has_markdown_channel(output: &AssistantOutput) -> bool {
    output
        .markdown
        .as_deref()
        .is_some_and(|markdown| !markdown.trim().is_empty())
        || output.parts.iter().any(
            |part| matches!(part, OutputPart::Markdown { markdown } if !markdown.trim().is_empty()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use qq_maid_core::service::OutputMedia;

    fn render_profile(enable_markdown: bool, enable_image: bool) -> RenderProfile {
        RenderProfile {
            supports_text: true,
            supports_markdown: enable_markdown,
            supports_image: enable_image,
            supports_attachment: false,
            unsupported_fallback: crate::gateway::outbound::UnsupportedCapabilityFallback::UseText,
        }
    }

    fn response_with_body(text: Option<&str>, markdown: Option<&str>) -> RespondResponse {
        // 测试直接构造 Core->Gateway 的结构化 output，不再绕旧 text/markdown 字段。
        let output = match (text, markdown) {
            (Some(text), Some(markdown)) => Some(qq_maid_core::service::AssistantOutput::markdown(
                text, markdown,
            )),
            (Some(text), None) => Some(qq_maid_core::service::AssistantOutput::text(text)),
            _ => None,
        };
        RespondResponse {
            output,
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        }
    }

    fn response_with_empty_output() -> RespondResponse {
        // 渲染层在 output 缺失时返回 None，对应旧空正文路径。
        RespondResponse {
            output: None,
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
            let actual = render_respond_response_for_profile(
                &response,
                &render_profile(case.enable_markdown, true),
            );
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
    fn empty_respond_output_renders_to_none() {
        // output 缺失时渲染层返回 None，不再依赖旧 text/markdown 兼容字段。
        assert_eq!(
            render_respond_response_for_profile(
                &response_with_empty_output(),
                &render_profile(true, true),
            ),
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
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            render_respond_response_for_profile(&response, &render_profile(true, true)),
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
    fn structured_image_part_uses_fallback_text_even_when_image_supported() {
        let response = RespondResponse {
            output: Some(AssistantOutput {
                text_fallback: String::new(),
                markdown: None,
                parts: vec![OutputPart::Image {
                    media: OutputMedia {
                        media_id: Some("image-media-id".to_owned()),
                        fallback_text: Some("图片：天气雷达".to_owned()),
                        ..OutputMedia::default()
                    },
                }],
            }),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            render_respond_response_for_profile(&response, &render_profile(false, true)),
            Some(OutboundMessage::Text {
                text: "图片：天气雷达".to_owned(),
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
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            render_respond_response_for_profile(&response, &render_profile(true, true)),
            Some(OutboundMessage::Text {
                text: UNSUPPORTED_FILE_FALLBACK_TEXT.to_owned(),
            })
        );
    }

    #[test]
    fn output_with_empty_parts_falls_back_to_text_fallback_and_markdown() {
        let response = RespondResponse {
            output: Some(AssistantOutput {
                text_fallback: "output fallback".to_owned(),
                markdown: Some("**output markdown**".to_owned()),
                parts: Vec::new(),
            }),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };

        assert_eq!(
            render_respond_response_for_profile(&response, &render_profile(true, true)),
            Some(OutboundMessage::Markdown {
                markdown: MarkdownPayload::new("**output markdown**"),
                fallback_text: "output fallback".to_owned(),
            })
        );
    }
}

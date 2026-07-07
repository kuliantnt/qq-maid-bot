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

    // 图片结构化出站（Issue #284 第一批）：仅在平台声明支持图片、且输出由单张可发送
    // 图片构成时，直接渲染为 `OutboundMessage::Image`，交由发送层走真实图片链路。
    // 其余情况（混合文本+图片、缺少 QQ 可用的 media_id 或平台不支持）继续走下方
    // markdown / 文本降级路径，把图片转为 fallback 文本，避免吞掉内容。
    if profile.supports_image
        && let Some(image) = single_sendable_image(output)
    {
        return Some(OutboundMessage::Image {
            image,
            fallback_text,
        });
    }

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

/// 当输出仅由单张“可发送”图片构成时，返回对应的 QQ 图片载荷。
///
/// “可发送”指图片媒体携带可上传的资源 `url`：发送层据此调用 QQ 富媒体上传
/// 接口换取 `file_info` 后再用 `msg_type=7` 发送。缺少 `url` 时返回 `None`，
/// 调用方应降级为 fallback 文本，不能凭空构造图片消息导致平台报错。
///
/// 注意：不使用 `media_id` / `file_id` 构造发送用 `file_info`——入站媒体标识
/// 不等于上传接口返回的 `file_info`，必须经上传接口获取。
///
/// 当前只处理“单张图片、无其他文本/Markdown part”的最小形态；混合内容仍走文本
/// 降级，后续批次再接入多消息出站。
fn single_sendable_image(output: &AssistantOutput) -> Option<ImagePayload> {
    if output.parts.len() != 1 {
        return None;
    }
    match &output.parts[0] {
        OutputPart::Image { media } => {
            let url = qq_image_url(media)?;
            Some(ImagePayload::new(url))
        }
        _ => None,
    }
}

/// 取图片的可上传资源 URL。
///
/// QQ 富媒体出站要求先上传图片资源（`url` 字段）换取 `file_info`；`OutputMedia::url`
/// 即平台无关的媒体资源地址，是上传接口所需来源。空 URL 视为不可发送。
fn qq_image_url(media: &OutputMedia) -> Option<String> {
    media
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
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
        let mut response = response_with_legacy_body(text, markdown);
        response.output =
            qq_maid_core::service::AssistantOutput::from_compat_fields(text, markdown);
        response
    }

    fn response_with_legacy_body(text: Option<&str>, markdown: Option<&str>) -> RespondResponse {
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
    fn empty_legacy_respond_text_renders_to_none() {
        assert_eq!(
            render_respond_response(
                &response_with_legacy_body(Some(" \n\t"), Some("# hi")),
                true,
                true
            ),
            None
        );
        assert_eq!(
            render_respond_response(&response_with_legacy_body(None, Some("# hi")), true, true),
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

    fn image_support_profile() -> RenderProfile {
        RenderProfile {
            supports_text: true,
            supports_markdown: true,
            supports_image: true,
            supports_attachment: false,
            unsupported_fallback: crate::gateway::outbound::UnsupportedCapabilityFallback::UseText,
        }
    }

    fn response_with_single_image(media: OutputMedia, fallback_text: &str) -> RespondResponse {
        RespondResponse {
            output: Some(AssistantOutput {
                text_fallback: fallback_text.to_owned(),
                markdown: None,
                parts: vec![OutputPart::Image { media }],
            }),
            text: None,
            markdown: None,
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        }
    }

    #[test]
    fn single_image_with_url_renders_to_image_message_when_supported() {
        let media = OutputMedia {
            url: Some("https://example.test/radar.png".to_owned()),
            fallback_text: Some("天气雷达图".to_owned()),
            ..OutputMedia::default()
        };
        let response = response_with_single_image(media, "天气雷达图");

        assert_eq!(
            render_respond_response_for_profile(&response, &image_support_profile()),
            Some(OutboundMessage::Image {
                image: ImagePayload::new("https://example.test/radar.png".to_owned()),
                fallback_text: "天气雷达图".to_owned(),
            })
        );
    }

    #[test]
    fn single_image_without_url_degrades_to_text_when_supported() {
        // 平台声明支持图片，但媒体缺少可上传的资源 URL，
        // 不能凭空发送图片，必须降级为 fallback 文本。
        let media = OutputMedia {
            fallback_text: Some("图片：天气雷达".to_owned()),
            ..OutputMedia::default()
        };
        let response = response_with_single_image(media, "图片：天气雷达");

        assert_eq!(
            render_respond_response_for_profile(&response, &image_support_profile()),
            Some(OutboundMessage::Text {
                text: "图片：天气雷达".to_owned(),
            })
        );
    }

    #[test]
    fn single_image_degrades_to_text_when_platform_does_not_support_image() {
        let media = OutputMedia {
            url: Some("https://example.test/radar.png".to_owned()),
            fallback_text: Some("图片：天气雷达".to_owned()),
            ..OutputMedia::default()
        };
        let response = response_with_single_image(media, "图片：天气雷达");

        assert_eq!(
            render_respond_response_for_profile(&response, &RenderProfile::text_only_sync()),
            Some(OutboundMessage::Text {
                text: "图片：天气雷达".to_owned(),
            })
        );
    }

    #[test]
    fn mixed_image_and_text_parts_degrade_to_text_when_supported() {
        // 混合内容暂不接入多消息图片出站，仍走文本降级。
        let media = OutputMedia {
            url: Some("https://example.test/radar.png".to_owned()),
            fallback_text: Some("图片：天气雷达".to_owned()),
            ..OutputMedia::default()
        };
        let response = RespondResponse {
            output: Some(AssistantOutput {
                text_fallback: "请看这张图\n\n图片：天气雷达".to_owned(),
                markdown: None,
                parts: vec![
                    OutputPart::Text {
                        text: "请看这张图".to_owned(),
                    },
                    OutputPart::Image { media },
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
            render_respond_response_for_profile(&response, &image_support_profile()),
            Some(OutboundMessage::Text {
                text: "请看这张图\n\n图片：天气雷达".to_owned(),
            })
        );
    }
}

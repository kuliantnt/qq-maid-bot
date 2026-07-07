//! 平台无关的出站内容模型。
//!
//! 与入站 [`crate::input_part::MessageInputPart`] 对称，这里只描述助手回复的
//! 顺序化内容块和纯展示元信息，不承载平台发送能力判断、fallback 策略或业务
//! 语义，便于 gateway、core 和 LLM 层复用。
//!
//! `text_fallback` 是所有平台都可降级发送的纯文本；`markdown` 保留结构化排版
//! 通道；`parts` 为图片、文件、卡片等顺序化出站内容载体。
//!
//! 本模块同时提供 parts 到纯文本 / Markdown 的纯渲染 helper，避免 gateway、
//! core、未来 LLM / tool / push 路径各自重复实现 strip / 拼接逻辑。**平台相关
//! 的默认文案（如"当前平台暂不支持发送图片"）由调用方作为参数传入**，common
//! 不绑定任何具体平台文案。

use crate::markdown_strip::strip_markdown_for_chat;

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

    /// 用户可见纯文本 fallback（直读 `text_fallback` 字段，零分配）。
    ///
    /// 适合 ref_index 回填、日志等只需读取已组装 fallback 字段的场景。若需要按
    /// `parts` 重新拼接（含媒体 fallback 文案），使用 [`Self::render_text_fallback`]。
    pub fn text_content(&self) -> &str {
        &self.text_fallback
    }

    /// 用户可见 Markdown 正文（直读 `markdown` 字段，零分配）。
    ///
    /// 与 [`Self::text_content`] 对应；纯文本回复时返回 `None`。若需要按 `parts`
    /// 重新拼接（含媒体 fallback 文案），使用 [`Self::render_markdown`]。
    pub fn markdown_content(&self) -> Option<&str> {
        self.markdown.as_deref()
    }

    /// 按 `parts` 重新拼接用户可见纯文本 fallback。
    ///
    /// 优先使用已组装的 `text_fallback`；为空时按 `parts` 顺序拼接各段纯文本
    /// （Markdown 段会被剥离为纯文本，媒体段在缺少 `fallback_text` 时使用调用方
    /// 传入的默认文案）。全空时返回 `None`，便于上层判断"无可见正文"。
    pub fn render_text_fallback(
        &self,
        image_fallback: &str,
        file_fallback: &str,
    ) -> Option<String> {
        if !self.text_fallback.trim().is_empty() {
            return Some(self.text_fallback.clone());
        }
        let text = self
            .parts
            .iter()
            .filter_map(|part| part.as_text_segment(image_fallback, file_fallback))
            .collect::<Vec<_>>()
            .join("\n\n");
        (!text.trim().is_empty()).then_some(text)
    }

    /// 按 `parts` 重新拼接用户可见 Markdown 正文。
    ///
    /// 优先按 `parts` 中各段拼接（Markdown 段保留原排版，媒体段使用调用方传入的
    /// 默认文案）；结果为空时回退到 `markdown` 字段，再回退到 `text_fallback`，
    /// 保证非空 output 总能产出可见正文。
    pub fn render_markdown(&self, image_fallback: &str, file_fallback: &str) -> String {
        let markdown = self
            .parts
            .iter()
            .filter_map(|part| part.as_markdown_segment(image_fallback, file_fallback))
            .collect::<Vec<_>>()
            .join("\n\n");
        if markdown.trim().is_empty() {
            self.markdown
                .clone()
                .unwrap_or_else(|| self.text_fallback.clone())
        } else {
            markdown
        }
    }

    /// 按是否偏好 Markdown 取最终单条用户可见正文。
    ///
    /// 偏好 Markdown 时返回非空 `markdown` 字段；否则返回非空 `text_fallback`
    /// 字段。适用于 ref_index 回填、日志、流式收尾等只需一条正文摘要的场景；
    /// 本方法只读字段不重新拼装 parts，零分配优先。
    pub fn preferred_text(&self, prefer_markdown: bool) -> Option<String> {
        if prefer_markdown {
            self.markdown
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        } else {
            (!self.text_fallback.trim().is_empty()).then(|| self.text_fallback.clone())
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

    /// 转为纯文本段（Markdown 段会被剥离为纯文本）；空段返回 `None`。
    ///
    /// 媒体段在缺少 `fallback_text` 时使用调用方传入的默认文案，避免 common
    /// 绑定任何具体平台文案。
    pub fn as_text_segment(&self, image_fallback: &str, file_fallback: &str) -> Option<String> {
        let text = match self {
            Self::Text { text } => text.clone(),
            Self::Markdown { markdown } => strip_markdown_for_chat(markdown),
            Self::Image { media } => media.fallback_text_or(image_fallback),
            Self::File { media } => media.fallback_text_or(file_fallback),
        };
        (!text.trim().is_empty()).then_some(text)
    }

    /// 转为 Markdown 段（保留原排版）；空段返回 `None`。
    ///
    /// 媒体段 fallback 与 [`Self::as_text_segment`] 一致。
    pub fn as_markdown_segment(&self, image_fallback: &str, file_fallback: &str) -> Option<String> {
        let text = match self {
            Self::Text { text } => text.clone(),
            Self::Markdown { markdown } => markdown.clone(),
            Self::Image { media } => media.fallback_text_or(image_fallback),
            Self::File { media } => media.fallback_text_or(file_fallback),
        };
        (!text.trim().is_empty()).then_some(text)
    }
}

impl OutputMedia {
    /// 媒体出站的用户可见 fallback 文本。
    ///
    /// 优先使用 `fallback_text`（去掉首尾空白后非空）；否则返回调用方传入的默认
    /// 文案。默认文案由调用方提供，common 不内置平台相关文案。
    pub fn fallback_text_or(&self, default_text: &str) -> String {
        self.fallback_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(default_text)
            .to_owned()
    }
}

fn non_empty_output_parts(parts: impl IntoIterator<Item = OutputPart>) -> Vec<OutputPart> {
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_output_carries_single_text_part() {
        let output = AssistantOutput::text("hello");
        assert_eq!(output.text_content(), "hello");
        assert_eq!(output.markdown_content(), None);
        assert_eq!(
            output.parts,
            vec![OutputPart::Text {
                text: "hello".to_owned()
            }]
        );
    }

    #[test]
    fn markdown_output_carries_markdown_part_and_field() {
        let output = AssistantOutput::markdown("plain", "# title");
        assert_eq!(output.text_content(), "plain");
        assert_eq!(output.markdown_content(), Some("# title"));
        assert_eq!(
            output.parts,
            vec![OutputPart::Markdown {
                markdown: "# title".to_owned()
            }]
        );
    }

    #[test]
    fn render_text_fallback_prefers_text_fallback_field() {
        let output = AssistantOutput::markdown("plain", "# title");
        assert_eq!(
            output.render_text_fallback("no-img", "no-file"),
            Some("plain".to_owned())
        );
    }

    #[test]
    fn render_text_fallback_joins_parts_when_field_empty() {
        let output = AssistantOutput {
            text_fallback: String::new(),
            markdown: None,
            parts: vec![
                OutputPart::Text {
                    text: "hello".to_owned(),
                },
                OutputPart::Markdown {
                    markdown: "# title".to_owned(),
                },
            ],
        };
        // text_fallback 为空时按 parts 拼接；markdown 段会被 strip。
        assert_eq!(
            output.render_text_fallback("no-img", "no-file"),
            Some("hello\n\ntitle".to_owned())
        );
    }

    #[test]
    fn render_text_fallback_uses_media_default_when_missing() {
        let output = AssistantOutput {
            text_fallback: String::new(),
            markdown: None,
            parts: vec![OutputPart::Image {
                media: OutputMedia::default(),
            }],
        };
        assert_eq!(
            output.render_text_fallback("图片不支持", "文件不支持"),
            Some("图片不支持".to_owned())
        );
    }

    #[test]
    fn render_text_fallback_returns_none_when_all_empty() {
        let output = AssistantOutput {
            text_fallback: String::new(),
            markdown: None,
            parts: Vec::new(),
        };
        assert_eq!(output.render_text_fallback("no-img", "no-file"), None);
    }

    #[test]
    fn render_markdown_prefers_parts_markdown_segments() {
        let output = AssistantOutput {
            text_fallback: "plain".to_owned(),
            markdown: Some("# legacy".to_owned()),
            parts: vec![
                OutputPart::Text {
                    text: "hello".to_owned(),
                },
                OutputPart::Markdown {
                    markdown: "## title".to_owned(),
                },
            ],
        };
        assert_eq!(
            output.render_markdown("no-img", "no-file"),
            "hello\n\n## title".to_owned()
        );
    }

    #[test]
    fn render_markdown_falls_back_to_markdown_field_when_parts_empty() {
        let output = AssistantOutput {
            text_fallback: "plain".to_owned(),
            markdown: Some("# legacy".to_owned()),
            parts: Vec::new(),
        };
        assert_eq!(output.render_markdown("no-img", "no-file"), "# legacy");
    }

    #[test]
    fn render_markdown_falls_back_to_text_fallback_when_markdown_absent() {
        let output = AssistantOutput::text("plain");
        assert_eq!(output.render_markdown("no-img", "no-file"), "plain");
    }

    #[test]
    fn media_fallback_text_prefers_explicit_value() {
        let media = OutputMedia {
            fallback_text: Some("  图片：天气雷达  ".to_owned()),
            ..OutputMedia::default()
        };
        assert_eq!(media.fallback_text_or("default"), "图片：天气雷达");
    }

    #[test]
    fn media_fallback_text_uses_default_when_blank_or_missing() {
        assert_eq!(OutputMedia::default().fallback_text_or("默认"), "默认");
        let media = OutputMedia {
            fallback_text: Some("   ".to_owned()),
            ..OutputMedia::default()
        };
        assert_eq!(media.fallback_text_or("默认"), "默认");
    }

    #[test]
    fn preferred_text_returns_markdown_when_preferred_and_present() {
        let output = AssistantOutput::markdown("plain", "# title");
        assert_eq!(output.preferred_text(true), Some("# title".to_owned()));
    }

    #[test]
    fn preferred_text_returns_text_fallback_when_markdown_absent() {
        let output = AssistantOutput::text("plain");
        assert_eq!(output.preferred_text(true), None);
        assert_eq!(output.preferred_text(false), Some("plain".to_owned()));
    }

    #[test]
    fn preferred_text_ignores_blank_markdown_field() {
        let output = AssistantOutput {
            text_fallback: "plain".to_owned(),
            markdown: Some("   ".to_owned()),
            parts: Vec::new(),
        };
        assert_eq!(output.preferred_text(true), None);
    }
}

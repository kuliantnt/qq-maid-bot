//! 基于 Markdown AST 的完整纯文本渲染。

use super::{ensure_line_break, ensure_paragraph_break, push_paragraph_break};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// 完整解析 Markdown 后渲染为纯文本，适合不稳定支持 Markdown 的平台通道。
///
/// 与轻量 [`crate::markdown::to_chat_text`] 分工：该函数完整解析引用式链接和合法
/// 反斜杠转义，链接目标用全角括号保留，引用定义本身不会作为正文输出。
pub fn to_plain_text(markdown: &str) -> String {
    // 不启用 smart punctuation，避免普通 RSS 文本里的半角引号等字符被擅自改写。
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(markdown, options);
    let mut output = String::new();
    let mut link_destinations = Vec::new();

    for event in parser {
        match event {
            Event::Start(Tag::Paragraph | Tag::Heading { .. }) => {
                ensure_paragraph_break(&mut output)
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) => push_paragraph_break(&mut output),
            Event::Start(Tag::Item) => {
                ensure_line_break(&mut output);
                output.push_str("• ");
            }
            Event::End(TagEnd::Item) => ensure_line_break(&mut output),
            Event::Start(Tag::Link { dest_url, .. }) => {
                link_destinations.push(dest_url.into_string());
            }
            Event::End(TagEnd::Link) => {
                if let Some(destination) = link_destinations.pop()
                    && !destination.trim().is_empty()
                {
                    output.push('（');
                    output.push_str(destination.trim());
                    output.push('）');
                }
            }
            Event::Text(text) | Event::Code(text) => output.push_str(&text),
            Event::SoftBreak | Event::HardBreak => ensure_line_break(&mut output),
            Event::Rule => push_paragraph_break(&mut output),
            Event::TaskListMarker(checked) => {
                output.push_str(if checked {
                    "[已完成] "
                } else {
                    "[未完成] "
                });
            }
            Event::Html(_) | Event::InlineHtml(_) | Event::FootnoteReference(_) => {}
            Event::Start(_) | Event::End(_) | Event::InlineMath(_) | Event::DisplayMath(_) => {}
        }
    }

    output.trim().to_owned()
}

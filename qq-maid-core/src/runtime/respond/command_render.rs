//! 轻量命令渲染 helper。
//!
//! 这里不引入通用 DSL，只提供少量命令输出常用块，避免业务层长期手写
//! Markdown / 纯文本两套模板后逐步漂移。

use qq_maid_common::markdown::{escape_inline, escape_text};

use super::common::CommandBody;

/// 同时维护 Markdown 与纯文本缓冲区的轻量 builder。
#[derive(Debug, Default)]
pub(crate) struct CommandRender {
    text_lines: Vec<String>,
    markdown_lines: Vec<String>,
}

impl CommandRender {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn title(&mut self, text: &str) {
        self.push_pair(text.to_owned(), format!("# {}", escape_inline(text)));
    }

    pub(crate) fn subtitle(&mut self, text: &str) {
        self.push_pair(text.to_owned(), format!("## {}", escape_inline(text)));
    }

    pub(crate) fn paragraph(&mut self, text: &str) {
        let text = text.trim().to_owned();
        self.push_pair(text.clone(), escape_text(&text));
    }

    pub(crate) fn bullet(&mut self, text: &str) {
        let text = text.trim().to_owned();
        self.push_pair(format!("· {text}"), format!("- {}", escape_text(&text)));
    }

    pub(crate) fn blank(&mut self) {
        if self
            .text_lines
            .last()
            .is_some_and(|line| !line.is_empty() || self.markdown_lines.last().is_some())
        {
            self.text_lines.push(String::new());
            self.markdown_lines.push(String::new());
        }
    }

    pub(crate) fn push_pair(&mut self, text: String, markdown: String) {
        self.text_lines.push(text);
        self.markdown_lines.push(markdown);
    }

    pub(crate) fn build(self) -> CommandBody {
        CommandBody::dual(self.text_lines.join("\n"), self.markdown_lines.join("\n"))
    }
}

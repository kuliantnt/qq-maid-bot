//! 轻量命令渲染 helper。
//!
//! 这里不引入通用 DSL，只提供少量命令输出常用块，避免业务层长期手写
//! Markdown / 纯文本两套模板后逐步漂移。

use super::common::CommandBody;

/// 同时维护 Markdown 与纯文本缓冲区的轻量 builder。
#[derive(Debug, Default)]
pub(super) struct CommandRender {
    text_lines: Vec<String>,
    markdown_lines: Vec<String>,
}

impl CommandRender {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn title(&mut self, text: &str) {
        self.push_pair(
            text.to_owned(),
            format!("# {}", escape_markdown_inline(text)),
        );
    }

    pub(super) fn subtitle(&mut self, text: &str) {
        self.push_pair(
            text.to_owned(),
            format!("## {}", escape_markdown_inline(text)),
        );
    }

    pub(super) fn paragraph(&mut self, text: &str) {
        let text = text.trim().to_owned();
        self.push_pair(text.clone(), escape_markdown_text(&text));
    }

    pub(super) fn bullet(&mut self, text: &str) {
        let text = text.trim().to_owned();
        self.push_pair(
            format!("· {text}"),
            format!("- {}", escape_markdown_text(&text)),
        );
    }

    pub(super) fn blank(&mut self) {
        if self
            .text_lines
            .last()
            .is_some_and(|line| !line.is_empty() || self.markdown_lines.last().is_some())
        {
            self.text_lines.push(String::new());
            self.markdown_lines.push(String::new());
        }
    }

    pub(super) fn push_pair(&mut self, text: String, markdown: String) {
        self.text_lines.push(text);
        self.markdown_lines.push(markdown);
    }

    pub(super) fn build(self) -> CommandBody {
        CommandBody::dual(self.text_lines.join("\n"), self.markdown_lines.join("\n"))
    }
}

/// 转义会出现在 Markdown 行内语境里的动态文本，避免用户输入破坏结构。
pub(crate) fn escape_markdown_inline(text: &str) -> String {
    let mut escaped = String::new();
    for ch in text.trim().replace(['\r', '\n'], " ").chars() {
        if matches!(
            ch,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '!'
                | '|'
                | '>'
        ) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// 多行段落默认逐行按行内文本转义，避免标题、列表和引用被用户输入意外触发。
pub(crate) fn escape_markdown_text(text: &str) -> String {
    text.lines()
        .map(escape_markdown_inline)
        .collect::<Vec<_>>()
        .join("  \n")
}

//! Markdown 剥离工具。
//!
//! 将 LLM 回复中的 Markdown 修饰（标题、列表、链接、代码、加粗等）剥除，
//! 保留纯文字用于 QQ 纯文本通道。该模块是纯文本处理，不依赖业务状态，
//! 最初位于 `qq-maid-core` 的 `runtime/respond/markdown_strip.rs`，
//! 因 Gateway 普通消息分段（Issue #124）需要按段为同一原文生成纯文本 fallback
//! 也复用同一套 strip 语义，故迁移到 `qq-maid-common` 共享，避免两套实现漂移。
//!
//! 行为约束：
//! - 围栏代码块（``` ```）内容原样保留，不剥除其中的 Markdown 符号；
//! - 表格展平为"单元格1 / 单元格2"格式，移除分隔行；
//! - 链接保留标签文字，URL 以全角括号附在后面；
//! - 图片使用 alt 文本，去掉 `!` 标记；
//! - 转义符号 `\\*` `\\_` 还原为字面量；
//! - `<br>`、`</p>` 等 HTML 标签转换为换行后移除其余标签。

mod legacy_strip;
mod plain_text;
mod qq;

pub use legacy_strip::strip_markdown_for_chat;
pub use plain_text::render_markdown_as_plain_text;
pub use qq::{render_markdown_for_qq, render_markdown_for_qq_with_limit};

fn ensure_line_break(output: &mut String) {
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
}

fn ensure_paragraph_break(output: &mut String) {
    if !output.is_empty() && !output.ends_with("\n\n") {
        ensure_line_break(output);
        output.push('\n');
    }
}

fn push_paragraph_break(output: &mut String) {
    if !output.ends_with("\n\n") {
        ensure_line_break(output);
        output.push('\n');
    }
}

#[cfg(test)]
mod tests;

//! Markdown 公共处理工具。
//!
//! 统一提供动态文本转义、HTTP(S) 链接构造、QQ Markdown 安全重渲染和
//! 纯文本 fallback。该模块不依赖业务状态，Core、Gateway 和业务工具都应复用
//! 这里的规则，避免多套实现漂移。
//!
//! 行为约束：
//! - 围栏代码块（``` ```）内容原样保留，不剥除其中的 Markdown 符号；
//! - 表格展平为"单元格1 / 单元格2"格式，移除分隔行；
//! - 链接保留标签文字，URL 以全角括号附在后面；
//! - 图片使用 alt 文本，去掉 `!` 标记；
//! - 转义符号 `\\*` `\\_` 还原为字面量；
//! - `<br>`、`</p>` 等 HTML 标签转换为换行后移除其余标签。

mod backticks;
mod chat_text;
mod plain_text;
mod qq;

pub use backticks::escape_unclosed_backticks;
pub use chat_text::to_chat_text;
pub use plain_text::to_plain_text;
pub use qq::{to_qq, to_qq_with_limit};

/// 转义 Markdown 行内语境中的动态文本，并把换行折叠为空格。
pub fn escape_inline(text: &str) -> String {
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

/// 逐行转义动态 Markdown 文本，并保留可预测的 Markdown 换行。
pub fn escape_text(text: &str) -> String {
    text.lines()
        .map(escape_inline)
        .collect::<Vec<_>>()
        .join("  \n")
}

/// 构造仅允许 HTTP(S) 目标的 Markdown 链接。
///
/// 非 HTTP(S) 或空目标只返回安全标签，避免调用方手写链接语法时遗漏转义与协议校验。
pub fn link(label: &str, destination: &str) -> String {
    let label = escape_inline(label);
    match sanitize_link_destination(destination) {
        Some(destination) => format!("[{label}](<{destination}>)"),
        None => label,
    }
}

pub(super) fn sanitize_link_destination(destination: &str) -> Option<String> {
    let destination = destination.trim();
    let lower = destination.to_ascii_lowercase();
    (!destination.is_empty()
        && (lower.starts_with("https://") || lower.starts_with("http://"))
        && !destination
            .chars()
            .any(|ch| matches!(ch, '\n' | '\r' | '<' | '>')))
    .then(|| destination.to_owned())
}

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

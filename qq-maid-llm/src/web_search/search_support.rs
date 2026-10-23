//! 独立搜索 API 共用的超时配置校验和来源链接排版。
use super::{WebSearchConfig, WebSearchSource};
use crate::error::LlmError;
use qq_maid_common::text::truncate_chars_with_ellipsis_trimmed;

pub(super) fn validate_timeouts(config: &WebSearchConfig) -> Result<(), LlmError> {
    if config.connect_timeout_seconds == 0
        || config.first_response_timeout_seconds == 0
        || config.total_timeout_seconds == 0
    {
        return Err(LlmError::config(
            "tools.web_search timeout values must be greater than zero",
        ));
    }
    if config.connect_timeout_seconds > config.first_response_timeout_seconds {
        return Err(LlmError::config(
            "tools.web_search.connect_timeout_seconds must not exceed first_response_timeout_seconds",
        ));
    }
    if config.first_response_timeout_seconds > config.total_timeout_seconds {
        return Err(LlmError::config(
            "tools.web_search.first_response_timeout_seconds must not exceed total_timeout_seconds",
        ));
    }
    Ok(())
}

pub(super) fn render_source_links(answer: Option<&str>, sources: &[WebSearchSource]) -> String {
    let answer = answer.map(str::trim).filter(|value| !value.is_empty());
    let mut output = answer
        .map(str::to_owned)
        .unwrap_or_else(|| "搜索到以下公开网页结果：".to_owned());
    output.push_str("\n\n参考来源：");
    for source in sources {
        push_escaped_source_link(
            &mut output,
            &escape_markdown_label(&source.title),
            &source.url,
        );
    }
    output
}

/// 追加一条 `- [已转义标签](URL)` 参考来源行。
///
/// 展示层会按字符预算裁剪正文，链接必须整行渲染：残缺的 `[标题](URL)` 既不可点击
/// 也会污染 Markdown 结构，因此所有来源链接排版统一从这里输出。标签必须由调用方
/// 转义，避免重复转义破坏标题。
fn push_escaped_source_link(output: &mut String, escaped_label: &str, url: &str) {
    output.push_str("\n- [");
    output.push_str(escaped_label);
    output.push_str("](");
    output.push_str(url);
    output.push(')');
}

/// 渲染一行带上限展示标题的参考来源链接（含前导换行），URL 必须完整保留。
///
/// 标题过长时只截断链接标签，完整标题仍保留在结构化 `sources`；URL 为空时返回
/// `None`，调用方应整条省略该来源。
pub(super) fn source_link_line(title: &str, url: &str, title_max_chars: usize) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let label = display_title(title, title_max_chars);
    // 标题为空或全被截断时退回使用 URL 作为标签，保证链接始终可点击。
    let label = if label.is_empty() {
        escape_markdown_label(url)
    } else {
        label
    };
    let mut line = String::new();
    push_escaped_source_link(&mut line, &label, url);
    Some(line)
}

/// answer 摘要中的展示标题：转义后再按展示上限截断。
pub(super) fn display_title(title: &str, max_chars: usize) -> String {
    truncate_chars_with_ellipsis_trimmed(&escape_markdown_label(title.trim()), max_chars)
}

/// 转义 Markdown 链接标签中的方括号和反斜杠，供来源链接与 answer 摘要共用。
pub(super) fn escape_markdown_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

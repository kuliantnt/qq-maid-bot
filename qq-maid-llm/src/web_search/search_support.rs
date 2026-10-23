//! 独立搜索 API 共用的超时配置校验和来源链接排版。
use super::{WebSearchConfig, WebSearchSource};
use crate::error::LlmError;

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
        output.push_str("\n- [");
        output.push_str(&escape_markdown_label(&source.title));
        output.push_str("](");
        output.push_str(&source.url);
        output.push(')');
    }
    output
}

/// 转义 Markdown 链接标签中的方括号和反斜杠，供来源链接与 answer 摘要共用。
pub(super) fn escape_markdown_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

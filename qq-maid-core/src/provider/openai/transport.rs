//! OpenAI Responses HTTP 传输层。
//!
//! Responses 主链路只关心“发什么 payload”和“如何解析返回值”；真正的 URL 拼接、
//! Accept 头、HTTP 错误文本裁剪统一放在这里，避免调用点重复处理 transport 细节。

use reqwest::{StatusCode, header};
use serde_json::Value;

use crate::error::LlmError;

/// OpenAI API 默认基础地址。
const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// 构造 OpenAI Responses API 完整 URL。
pub(crate) fn openai_responses_url(base_url: Option<&str>) -> String {
    let base_url = base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(OPENAI_DEFAULT_BASE_URL);
    format!("{}/responses", base_url.trim_end_matches('/'))
}

/// 发送 OpenAI Responses 请求。
pub(crate) async fn send_openai_responses_request(
    client: &reqwest::Client,
    api_key: &str,
    base_url: Option<&str>,
    payload: &Value,
    stream: bool,
) -> Result<reqwest::Response, LlmError> {
    let mut request = client
        .post(openai_responses_url(base_url))
        .bearer_auth(api_key)
        .json(payload);
    if stream {
        request = request.header(header::ACCEPT, "text/event-stream");
    }
    let response = request.send().await.map_err(|err| {
        let context = if stream {
            "OpenAI chat stream request failed"
        } else {
            "OpenAI chat request failed"
        };
        LlmError::http(format!("{context}: {err}"))
    })?;

    let status = response.status();
    if !status.is_success() {
        return Err(openai_chat_status_error(status, response).await);
    }
    Ok(response)
}

async fn openai_chat_status_error(status: StatusCode, response: reqwest::Response) -> LlmError {
    let detail = response.text().await.unwrap_or_default();
    let detail = truncate_error_detail(detail.trim(), 500);
    if detail.is_empty() {
        return LlmError::http(format!("OpenAI chat returned HTTP {}", status.as_u16()));
    }
    LlmError::http(format!(
        "OpenAI chat returned HTTP {}: {}",
        status.as_u16(),
        detail
    ))
}

fn truncate_error_detail(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let mut truncated = value.chars().take(limit).collect::<String>();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_responses_url_uses_default_or_custom_base() {
        assert_eq!(
            openai_responses_url(None),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            openai_responses_url(Some("https://proxy.example/v1/")),
            "https://proxy.example/v1/responses"
        );
    }
}

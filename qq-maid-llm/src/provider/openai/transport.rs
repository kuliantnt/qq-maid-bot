//! OpenAI Responses HTTP 传输层。
//!
//! Responses 主链路只关心“发什么 payload”和“如何解析返回值”；真正的 URL 拼接、
//! Accept 头、HTTP 错误文本裁剪统一放在这里，避免调用点重复处理 transport 细节。

use std::time::Instant;

use reqwest::{StatusCode, header};
use serde_json::Value;

use crate::{
    config::HttpAuthConfig,
    error::{LlmError, LlmErrorKind},
};

/// OpenAI API 默认基础地址。
const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// 单次 Responses 传输的低敏观测上下文。
pub(crate) struct ResponsesTransportContext<'a> {
    pub(crate) provider: &'a str,
    pub(crate) model: &'a str,
    pub(crate) stream: bool,
}

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
    auth: Option<&HttpAuthConfig>,
    payload: &Value,
    transport: ResponsesTransportContext<'_>,
) -> Result<reqwest::Response, LlmError> {
    let ResponsesTransportContext {
        provider,
        model,
        stream,
    } = transport;
    let request = client.post(openai_responses_url(base_url));
    let mut request = match auth {
        Some(auth) => {
            let header_name = reqwest::header::HeaderName::from_bytes(auth.header.as_bytes())
                .map_err(|_| {
                    LlmError::config(format!(
                        "{} auth header `{}` is invalid",
                        provider, auth.header
                    ))
                })?;
            let header_value = match auth.scheme.as_deref() {
                Some(scheme) => format!("{scheme} {api_key}"),
                None => api_key.to_owned(),
            };
            request.header(header_name, header_value)
        }
        None => request.bearer_auth(api_key),
    }
    .json(payload);
    if stream {
        request = request.header(header::ACCEPT, "text/event-stream");
    }
    let started = Instant::now();
    let response = request.send().await.map_err(|err| {
        let stage = if err.is_timeout() {
            "http_request_timeout"
        } else {
            "http_request"
        };
        let context = if stream { "stream request" } else { "request" };
        let mapped = LlmError::from_error_source(
            &err,
            LlmErrorKind::Network,
            stage,
            format!("{provider} Responses {context} failed"),
        );
        tracing::warn!(
            provider,
            model,
            stream,
            timeout_stage = mapped.stage.as_str(),
            elapsed_ms = started.elapsed().as_millis(),
            error_kind = mapped.kind().as_str(),
            error = %err,
            "LLM Responses 传输失败"
        );
        mapped
    })?;

    let status = response.status();
    if !status.is_success() {
        return Err(responses_status_error(provider, status, response).await);
    }
    Ok(response)
}

async fn responses_status_error(
    provider: &str,
    status: StatusCode,
    response: reqwest::Response,
) -> LlmError {
    let detail = response.text().await.unwrap_or_default();
    let detail = truncate_error_detail(detail.trim(), 500);
    let message = if detail.is_empty() {
        format!("{provider} Responses returned HTTP {}", status.as_u16())
    } else {
        format!(
            "{provider} Responses returned HTTP {}: {}",
            status.as_u16(),
            detail
        )
    };
    // 缺失 API Key 等本地错误仍由 Provider 构造阶段返回 config；已经到达上游的
    // 401/403 保留 Authentication 分类，同时由候选路由继续执行既有 fallback。
    LlmError::from_upstream_status(status.as_u16(), message, "http")
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

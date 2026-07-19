use std::time::Instant;

use async_trait::async_trait;
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::{
    config::LlmConfig,
    error::LlmError,
    metrics::duration_ms,
    provider::types::{ModelId, ModelProvider},
};
use qq_maid_common::time_context::request_time_context;

use super::{
    WebSearchExecutor, WebSearchOutcome, WebSearchRequest, WebSearchSource, build_query_prompt,
    configured_max_results, trace_query_input_enabled, truncate_error_detail,
};

/// Gemini generateContent API 默认基础地址。
const GEMINI_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

pub(super) struct MissingGeminiWebSearchExecutor;

#[async_trait]
impl WebSearchExecutor for MissingGeminiWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Err(LlmError::config(
            "GEMINI_API_KEY is required for Gemini web query service",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "gemini"
    }
}

/// 基于 Gemini 原生 Google Search 工具的 Web Search 执行器。
pub(super) struct GeminiWebSearchExecutor {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    search_model: String,
}

impl GeminiWebSearchExecutor {
    pub(super) fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let api_key = config
            .gemini_api_key
            .clone()
            .ok_or_else(|| LlmError::config("GEMINI_API_KEY is required"))?;
        let client = qq_maid_common::http_client::try_builder()
            .map_err(|err| {
                LlmError::config(format!("failed to configure Gemini query TLS: {err}"))
            })?
            .timeout(std::time::Duration::from_secs(
                config.request_timeout_seconds,
            ))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build Gemini query HTTP client: {err}"))
            })?;
        let search_model = gemini_search_model_name(&config.gemini_model, "GEMINI_MODEL")?;

        Ok(Self {
            client,
            api_key,
            base_url: gemini_native_base_url(&config.gemini_base_url),
            search_model,
        })
    }
}

#[async_trait]
impl WebSearchExecutor for GeminiWebSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        let query = req.query.trim();
        if query.is_empty() {
            return Err(LlmError::new(
                "bad_request",
                "query must not be empty",
                "request",
            ));
        }

        let started = Instant::now();
        let max_results = configured_max_results(req.max_results);
        let model = req.model_override.as_deref().unwrap_or(&self.search_model);
        let payload = gemini_web_search_payload(&req, query, max_results);
        let url = gemini_generate_content_url(&self.base_url, model);
        trace_gemini_query_payload(&req, &url, model, &payload);

        let response = self
            .client
            .post(url)
            .header("x-goog-api-key", &self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    LlmError::timeout("http")
                } else {
                    LlmError::http(format!("Gemini web query request failed: {err}"))
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(gemini_status_error(status, response).await);
        }

        let body: Value = response.json().await.map_err(|err| {
            LlmError::provider(format!("invalid Gemini query JSON: {err}"), "json")
        })?;
        let answer = extract_gemini_output_text(&body).ok_or_else(|| {
            LlmError::provider("Gemini web query returned empty text output", "provider")
        })?;
        let sources = extract_gemini_sources(&body, usize::from(max_results));

        Ok(WebSearchOutcome {
            answer,
            sources,
            provider: "gemini".to_owned(),
            elapsed_ms: duration_ms(started.elapsed()),
        })
    }

    fn provider_name(&self) -> &'static str {
        "gemini"
    }
}

fn gemini_search_model_name(value: &str, name: &str) -> Result<String, LlmError> {
    let model = ModelId::parse_config(value, name)?;
    match model.provider {
        Some(ModelProvider::Gemini) | None => Ok(model.name),
        Some(provider) => Err(LlmError::config(format!(
            "{name} cannot use `{}` provider prefix for Gemini query model",
            provider.as_str()
        ))),
    }
}

fn gemini_native_base_url(base_url: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    let base_url = base_url.strip_suffix("/openai").unwrap_or(base_url);
    if base_url.is_empty() {
        GEMINI_DEFAULT_BASE_URL.to_owned()
    } else {
        base_url.to_owned()
    }
}

fn gemini_generate_content_url(base_url: &str, model: &str) -> String {
    format!(
        "{}/models/{}:generateContent",
        base_url.trim_end_matches('/'),
        percent_encode_path_segment(model)
    )
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte))
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

fn gemini_web_search_payload(req: &WebSearchRequest, query: &str, max_results: u8) -> Value {
    json!({
        "contents": [{
            "role": "user",
            "parts": [{
                "text": build_query_prompt(
                    query,
                    req.raw_question.as_deref(),
                    max_results,
                    &request_time_context()
                )
            }]
        }],
        "tools": [{
            "google_search": {}
        }]
    })
}

fn trace_gemini_query_payload(req: &WebSearchRequest, url: &str, model: &str, payload: &Value) {
    if !tracing::enabled!(tracing::Level::TRACE) {
        return;
    }

    let input = payload
        .get("contents")
        .and_then(Value::as_array)
        .and_then(|contents| contents.first())
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tools = payload.get("tools").unwrap_or(&Value::Null).to_string();
    tracing::trace!(
        upstream_url = url,
        model = model,
        tools = %tools,
        input_chars = input.chars().count(),
        query_chars = req.query.trim().chars().count(),
        "gemini query request payload summary"
    );

    if trace_query_input_enabled() {
        tracing::trace!(
            upstream_url = url,
            input = %input,
            "gemini query request input"
        );
    }
}

async fn gemini_status_error(status: StatusCode, response: reqwest::Response) -> LlmError {
    let detail = response.text().await.unwrap_or_default();
    let detail = truncate_error_detail(detail.trim(), 500);
    let message = if detail.is_empty() {
        format!("Gemini web query returned HTTP {}", status.as_u16())
    } else {
        format!(
            "Gemini web query returned HTTP {}: {detail}",
            status.as_u16()
        )
    };
    match status.as_u16() {
        401 | 403 => LlmError::config(message),
        429 => LlmError::new("rate_limited", message, "http"),
        500..=599 => LlmError::new("upstream_unavailable", message, "http"),
        _ => LlmError::http(message),
    }
}

fn extract_gemini_output_text(body: &Value) -> Option<String> {
    let candidates = body.get("candidates").and_then(Value::as_array)?;
    let mut parts = Vec::new();
    for candidate in candidates {
        let Some(content_parts) = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for part in content_parts {
            let Some(text) = part
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
                continue;
            };
            parts.push(text.to_owned());
        }
    }

    let answer = parts.join("\n\n");
    let answer = answer.trim();
    if answer.is_empty() {
        None
    } else {
        Some(answer.to_owned())
    }
}

fn extract_gemini_sources(body: &Value, max_results: usize) -> Vec<WebSearchSource> {
    let mut sources = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();
    let Some(candidates) = body.get("candidates").and_then(Value::as_array) else {
        return sources;
    };
    for candidate in candidates {
        let Some(chunks) = candidate
            .get("groundingMetadata")
            .and_then(|metadata| metadata.get("groundingChunks"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        collect_gemini_grounding_chunks(chunks, &mut sources, &mut seen_urls, max_results);
        if sources.len() >= max_results {
            break;
        }
    }
    sources
}

fn collect_gemini_grounding_chunks(
    values: &[Value],
    sources: &mut Vec<WebSearchSource>,
    seen_urls: &mut std::collections::HashSet<String>,
    max_results: usize,
) {
    for value in values {
        if sources.len() >= max_results {
            return;
        }
        let Some(web) = value.get("web") else {
            continue;
        };
        let Some(url) = web
            .get("uri")
            .or_else(|| web.get("url"))
            .and_then(Value::as_str)
            .map(str::trim)
        else {
            continue;
        };
        if url.is_empty() || seen_urls.contains(url) {
            continue;
        }
        let title = web
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(url);
        sources.push(WebSearchSource {
            title: title.to_owned(),
            url: url.to_owned(),
            snippet: String::new(),
        });
        seen_urls.insert(url.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_url_payload_and_model_parsing_use_google_search_tool() {
        let req = WebSearchRequest {
            query: "Gemini 搜索".to_owned(),
            raw_question: Some("/查 Gemini 搜索".to_owned()),
            max_results: Some(4),
            context_size: Some("high".to_owned()),
            model_override: None,
        };
        let payload = gemini_web_search_payload(&req, &req.query, 4);

        assert_eq!(
            gemini_native_base_url("https://generativelanguage.googleapis.com/v1beta/openai"),
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(
            gemini_generate_content_url(
                "https://generativelanguage.googleapis.com/v1beta",
                "gemini-2.5-flash"
            ),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert_eq!(
            gemini_search_model_name("gemini:gemini-2.5-flash", "OPENAI_SEARCH_MODEL").unwrap(),
            "gemini-2.5-flash"
        );
        assert!(payload["tools"][0].get("google_search").is_some());
        assert!(
            payload["contents"][0]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .contains("参考来源最多列出 4 条")
        );
    }

    #[test]
    fn extracts_gemini_output_text_and_sources() {
        let body = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "第一段"},
                        {"text": "第二段"}
                    ]
                },
                "groundingMetadata": {
                    "groundingChunks": [
                        {"web": {"title": "A", "uri": "https://a.test"}},
                        {"web": {"title": "A duplicate", "uri": "https://a.test"}},
                        {"web": {"title": "B", "uri": "https://b.test"}}
                    ]
                }
            }]
        });

        assert_eq!(
            extract_gemini_output_text(&body).as_deref(),
            Some("第一段\n\n第二段")
        );
        let sources = extract_gemini_sources(&body, 5);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].title, "A");
        assert_eq!(sources[0].url, "https://a.test");
        assert_eq!(sources[1].url, "https://b.test");
    }
}

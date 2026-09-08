//! 显式管理诊断：复用协议载荷，限制超时、响应体及并发，绝不回传上游正文。

use crate::{
    config::HttpAuthConfig,
    error::LlmError,
    provider::types::{ChatMessage, ChatRole},
};
use serde::Serialize;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

static PROBES: Semaphore = Semaphore::const_new(2);
const MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeState {
    Success,
    Failed,
    Unknown,
    NotTested,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionDiagnostic {
    pub network: ProbeState,
    pub authentication: ProbeState,
    pub adapter: ProbeState,
    pub model_call: ProbeState,
    pub elapsed_ms: u64,
    pub http_status: Option<u16>,
    pub category: &'static str,
}

pub async fn test_connection(
    base_url: &str,
    responses: bool,
    auth: &HttpAuthConfig,
    api_key: &str,
    model: &str,
) -> Result<ConnectionDiagnostic, LlmError> {
    let _permit = PROBES
        .try_acquire()
        .map_err(|_| LlmError::config("连接测试繁忙，请稍后重试"))?;
    if model.trim().is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        return Err(LlmError::config("请输入有效模型 ID"));
    }
    let client = qq_maid_common::http_client::try_builder()
        .map_err(|_| LlmError::config("无法初始化诊断 TLS"))?
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| LlmError::config("无法初始化诊断客户端"))?;
    let messages = [ChatMessage {
        role: ChatRole::User,
        content: "Reply OK.".into(),
        content_parts: vec![],
    }];
    let payload = if responses {
        super::payload::openai_responses_payload(&messages, model, 0, 32, None, false, false)?
    } else {
        super::chat::chat_completions_payload(&messages, model, 0, 32, false)?
    };
    let url = format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        if responses {
            "responses"
        } else {
            "chat/completions"
        }
    );
    let header = reqwest::header::HeaderName::from_bytes(auth.header.as_bytes())
        .map_err(|_| LlmError::config("认证 Header 无效"))?;
    let value = match &auth.scheme {
        Some(scheme) => format!("{scheme} {api_key}"),
        None => api_key.to_owned(),
    };
    let mut value = reqwest::header::HeaderValue::from_str(&value)
        .map_err(|_| LlmError::config("认证凭证格式无效"))?;
    value.set_sensitive(true);
    let start = Instant::now();
    let mut result = ConnectionDiagnostic {
        network: ProbeState::Unknown,
        authentication: ProbeState::Unknown,
        adapter: ProbeState::NotTested,
        model_call: ProbeState::NotTested,
        elapsed_ms: 0,
        http_status: None,
        category: "unknown",
    };
    let request = client.post(url).header(header, value).json(&payload);
    match tokio::time::timeout(
        Duration::from_secs(15),
        probe(request, responses, &mut result),
    )
    .await
    {
        Ok(()) => {}
        Err(_) => {
            result.model_call = ProbeState::Failed;
            result.category = "timeout";
        }
    }
    result.elapsed_ms = start.elapsed().as_millis() as u64;
    Ok(result)
}

async fn probe(
    request: reqwest::RequestBuilder,
    responses: bool,
    result: &mut ConnectionDiagnostic,
) {
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            result.network = if error.is_connect() {
                ProbeState::Failed
            } else {
                ProbeState::Unknown
            };
            result.model_call = ProbeState::Failed;
            result.category = if error.is_timeout() {
                "timeout"
            } else {
                "transport"
            };
            return;
        }
    };
    result.network = ProbeState::Success;
    let status = response.status();
    result.http_status = Some(status.as_u16());
    if !status.is_success() {
        result.model_call = ProbeState::Failed;
        result.category = match status.as_u16() {
            401 | 403 => {
                result.authentication = ProbeState::Failed;
                "authentication"
            }
            404 | 405 => "endpoint_or_model",
            429 => "rate_limit",
            500..=599 => "upstream",
            300..=399 => "redirect_rejected",
            _ => "request_rejected",
        };
        return;
    }
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len() + chunk.len() <= MAX_BYTES => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(Some(_)) => {
                result.category = "response_too_large";
                result.model_call = ProbeState::Failed;
                return;
            }
            Ok(None) => break,
            Err(_) => {
                result.category = "response_read";
                result.model_call = ProbeState::Failed;
                return;
            }
        }
    }
    let body = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    // HTTP 200 或 /models 成功不代表模型可用：必须存在相应 Adapter 的真实文本输出。
    let valid = body.as_ref().is_some_and(|body| {
        if responses {
            body.get("output")
                .and_then(|value| value.as_array())
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("content")
                            .and_then(|value| value.as_array())
                            .is_some_and(|content| {
                                content.iter().any(|part| {
                                    part["type"] == "output_text"
                                        && part["text"]
                                            .as_str()
                                            .is_some_and(|text| !text.is_empty())
                                })
                            })
                    })
                })
        } else {
            body.pointer("/choices/0/message/content")
                .and_then(|value| value.as_str())
                .is_some_and(|text| !text.is_empty())
        }
    });
    result.model_call = if valid {
        ProbeState::Success
    } else {
        ProbeState::Failed
    };
    result.adapter = if valid {
        ProbeState::Success
    } else {
        ProbeState::Unknown
    };
    result.authentication = if valid {
        ProbeState::Success
    } else {
        ProbeState::Unknown
    };
    result.category = if valid {
        "model_response"
    } else {
        "unrecognized_response"
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
    use serde_json::json;

    #[tokio::test]
    async fn diagnostics_require_real_adapter_output_and_never_echo_upstream_secrets() {
        let app = Router::new()
            .route(
                "/chat/chat/completions",
                post(|| async { Json(json!({"choices": [{"message": {"content": "OK"}}]})) }),
            )
            .route(
                "/responses/responses",
                post(|| async {
                    Json(json!({"output": [{"content": [{"type":"output_text", "text":"OK"}]}]}))
                }),
            )
            .route(
                "/auth/chat/completions",
                post(|| async { (StatusCode::UNAUTHORIZED, "test-only-sensitive-response") }),
            )
            .route(
                "/invalid/chat/completions",
                post(|| async {
                    Json(json!({"data": [], "secret": "test-only-sensitive-response"}))
                }),
            )
            .route(
                "/large/chat/completions",
                post(|| async { "x".repeat(MAX_BYTES + 1) }),
            )
            .route(
                "/redirect/chat/completions",
                post(|| async {
                    (
                        StatusCode::TEMPORARY_REDIRECT,
                        [("location", "http://127.0.0.1:1")],
                        "",
                    )
                        .into_response()
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for (path, responses, expected) in [
            ("chat", false, "model_response"),
            ("responses", true, "model_response"),
            ("auth", false, "authentication"),
            ("invalid", false, "unrecognized_response"),
            ("large", false, "response_too_large"),
            ("redirect", false, "redirect_rejected"),
        ] {
            let result = test_connection(
                &format!("http://{address}/{path}"),
                responses,
                &HttpAuthConfig::default(),
                "test-only-key",
                "test-model",
            )
            .await
            .unwrap();
            assert_eq!(result.category, expected);
            assert_eq!(result.network, ProbeState::Success);
            assert_eq!(
                result.model_call == ProbeState::Success,
                expected == "model_response"
            );
            let serialized = serde_json::to_string(&result).unwrap();
            assert!(!serialized.contains("test-only"));
            if expected == "authentication" {
                assert_eq!(result.authentication, ProbeState::Failed);
            }
        }
        server.abort();
    }
}

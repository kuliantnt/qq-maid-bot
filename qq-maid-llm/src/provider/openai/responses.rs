//! OpenAI Responses 主链路。
//!
//! 这里仅负责 Responses API 的流式/非流式聊天执行，以及在需要时回退到同 provider
//! 的非流式请求；不直接接触 rig-core，以保证 Responses 与 fallback provider 彻底解耦。

use serde_json::Value;

use crate::{
    error::LlmError,
    provider::{ChatOutcome, types::ChatMessage},
    util::metrics::MetricsRecorder,
};

use super::{
    extract::{extract_response_output_text, extract_response_usage},
    fallback::{
        should_retry_non_stream_after_empty_stream, should_retry_non_stream_after_stream_error,
    },
    payload::openai_responses_payload,
    stream::{handle_openai_chat_stream_event, parse_sse_frame, take_sse_frame},
    transport::send_openai_responses_request,
};

/// OpenAI Responses 聊天请求上下文。
///
/// 这些字段必须作为同一次请求整体传入，避免流式失败后非流式重试时误用不同配置。
pub(crate) struct OpenAiResponsesChatRequest<'a> {
    pub(crate) stream: bool,
    pub(crate) client: &'a reqwest::Client,
    pub(crate) api_key: &'a str,
    pub(crate) base_url: Option<&'a str>,
    pub(crate) provider: &'a str,
    pub(crate) model: &'a str,
    pub(crate) max_output_tokens: u64,
    pub(crate) messages: &'a [ChatMessage],
}

/// 执行 OpenAI Responses API 聊天补全，并在流式异常时补一次非流式请求。
pub(crate) async fn openai_responses_chat_with_stream_fallback(
    req: OpenAiResponsesChatRequest<'_>,
) -> Result<ChatOutcome, LlmError> {
    if req.stream {
        match openai_responses_stream_chat(
            req.client,
            req.api_key,
            req.base_url,
            req.provider,
            req.model,
            req.max_output_tokens,
            req.messages,
        )
        .await
        {
            Ok(outcome) => {
                if !should_retry_non_stream_after_empty_stream(&outcome) {
                    return Ok(outcome);
                }
                tracing::warn!(
                    provider = req.provider,
                    model = %req.model,
                    "streaming OpenAI Responses chat returned empty reply; retrying once with non-stream request"
                );
            }
            Err(err) => {
                if !should_retry_non_stream_after_stream_error(&err) {
                    return Err(err);
                }
                tracing::warn!(
                    provider = req.provider,
                    model = %req.model,
                    error_code = err.code.as_str(),
                    error_stage = err.stage.as_str(),
                    "streaming OpenAI Responses chat failed; retrying once with non-stream request"
                );
            }
        }
    }

    openai_responses_non_stream_chat(
        req.client,
        req.api_key,
        req.base_url,
        req.provider,
        req.model,
        req.max_output_tokens,
        req.messages,
    )
    .await
}

/// 非流式 OpenAI Responses 聊天请求。
pub(crate) async fn openai_responses_non_stream_chat(
    client: &reqwest::Client,
    api_key: &str,
    base_url: Option<&str>,
    provider: &str,
    model: &str,
    max_output_tokens: u64,
    messages: &[ChatMessage],
) -> Result<ChatOutcome, LlmError> {
    let recorder = MetricsRecorder::start();
    let payload = openai_responses_payload(messages, model, max_output_tokens, false)?;
    let response =
        send_openai_responses_request(client, api_key, base_url, &payload, false).await?;

    let body: Value = response
        .json()
        .await
        .map_err(|err| LlmError::provider(format!("invalid OpenAI chat JSON: {err}"), "json"))?;
    let reply = extract_response_output_text(&body)
        .ok_or_else(|| LlmError::provider("OpenAI chat returned empty text output", "provider"))?;
    let usage = extract_response_usage(&body);
    let metrics = recorder.finish(provider, model, false);

    Ok(ChatOutcome {
        reply,
        metrics,
        usage,
    })
}

/// 流式 OpenAI Responses 聊天请求。
pub(crate) async fn openai_responses_stream_chat(
    client: &reqwest::Client,
    api_key: &str,
    base_url: Option<&str>,
    provider: &str,
    model: &str,
    max_output_tokens: u64,
    messages: &[ChatMessage],
) -> Result<ChatOutcome, LlmError> {
    let mut recorder = MetricsRecorder::start();
    let payload = openai_responses_payload(messages, model, max_output_tokens, true)?;
    let mut response =
        send_openai_responses_request(client, api_key, base_url, &payload, true).await?;

    let mut frame_buffer = Vec::new();
    let mut answer = String::new();
    let mut completed_response: Option<Value> = None;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| LlmError::http(format!("OpenAI chat stream failed: {err}")))?
    {
        frame_buffer.extend_from_slice(&chunk);
        while let Some(frame) = take_sse_frame(&mut frame_buffer) {
            let Some(event) = parse_sse_frame(&frame)? else {
                continue;
            };
            recorder.mark_event();
            handle_openai_chat_stream_event(
                event,
                &mut recorder,
                &mut answer,
                &mut completed_response,
            )?;
        }
    }
    if !frame_buffer.is_empty()
        && let Some(event) = parse_sse_frame(&frame_buffer)?
    {
        recorder.mark_event();
        handle_openai_chat_stream_event(
            event,
            &mut recorder,
            &mut answer,
            &mut completed_response,
        )?;
    }

    // 某些兼容层不会把完整正文持续写成 delta，而是只在 completed 事件中附带最终响应。
    if answer.trim().is_empty()
        && let Some(response) = completed_response.as_ref()
    {
        answer = extract_response_output_text(response).unwrap_or_default();
    }
    let reply = answer.trim().to_owned();
    if reply.is_empty() {
        return Err(LlmError::provider(
            "OpenAI chat returned empty text output",
            "provider",
        ));
    }
    let usage = completed_response.as_ref().and_then(extract_response_usage);
    let metrics = recorder.finish(provider, model, true);

    Ok(ChatOutcome {
        reply,
        metrics,
        usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{StatusCode, header},
        response::IntoResponse,
        routing::post,
    };
    use std::sync::Arc;
    use tokio::{net::TcpListener, sync::Mutex};

    #[derive(Debug)]
    struct MockResponsesState {
        body: String,
        status: StatusCode,
        calls: usize,
    }

    async fn mock_responses_handler(
        State(state): State<Arc<Mutex<MockResponsesState>>>,
        _body: Body,
    ) -> impl IntoResponse {
        let mut state = state.lock().await;
        state.calls += 1;
        (
            state.status,
            [(header::CONTENT_TYPE, "text/event-stream")],
            state.body.clone(),
        )
    }

    async fn spawn_mock_responses(
        body: String,
        status: StatusCode,
    ) -> (String, Arc<Mutex<MockResponsesState>>) {
        let state = Arc::new(Mutex::new(MockResponsesState {
            body,
            status,
            calls: 0,
        }));
        let app = Router::new()
            .route("/v1/responses", post(mock_responses_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1"), state)
    }

    #[tokio::test]
    async fn openai_responses_stream_uses_completed_response_when_delta_is_missing() {
        let (base_url, state) = spawn_mock_responses(
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"stream fallback\"}}\n\n"
                .to_owned(),
            StatusCode::OK,
        )
        .await;
        let client = reqwest::Client::new();
        let outcome = openai_responses_stream_chat(
            &client,
            "test-key",
            Some(&base_url),
            "openai",
            "gpt-5.5",
            1200,
            &[crate::provider::types::ChatMessage::user("hi")],
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "stream fallback");
        let state = state.lock().await;
        assert_eq!(state.calls, 1);
    }
}

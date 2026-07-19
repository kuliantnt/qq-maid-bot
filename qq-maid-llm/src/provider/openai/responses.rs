//! OpenAI Responses 主链路。
//!
//! 这里仅负责 Responses API 的流式/非流式聊天执行，以及在需要时回退到同 provider
//! 的非流式请求；不直接接触 Chat Completions，以保证 Responses 与 fallback provider 解耦。

use futures::stream;
use serde_json::Value;

use crate::{
    error::LlmError,
    metrics::MetricsRecorder,
    provider::{
        ChatOutcome, LlmStream, LlmStreamEvent, collect_llm_stream,
        types::{ChatMessage, ReasoningEffort},
    },
    sse::{parse_sse_frame, take_sse_frame},
};

use super::{
    extract::{extract_response_output_text, extract_response_usage},
    fallback::{
        should_retry_non_stream_after_empty_stream, should_retry_non_stream_after_stream_error,
    },
    payload::openai_responses_payload,
    stream::{
        handle_openai_chat_stream_event, is_openai_responses_done_sentinel,
        responses_stream_is_complete,
    },
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
    pub(crate) media_max_bytes: u64,
    pub(crate) max_output_tokens: u64,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) messages: &'a [ChatMessage],
    pub(crate) allow_completed_response_fallback: bool,
}

/// 执行 OpenAI Responses API 聊天补全，并在流式异常时补一次非流式请求。
pub(crate) async fn openai_responses_chat_with_stream_fallback(
    req: OpenAiResponsesChatRequest<'_>,
) -> Result<ChatOutcome, LlmError> {
    if req.stream {
        match openai_responses_stream_chat(&req).await {
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

    openai_responses_non_stream_chat(&req).await
}

/// 非流式 OpenAI Responses 聊天请求。
pub(crate) async fn openai_responses_non_stream_chat(
    req: &OpenAiResponsesChatRequest<'_>,
) -> Result<ChatOutcome, LlmError> {
    let recorder = MetricsRecorder::start();
    let payload = openai_responses_payload(
        req.messages,
        req.model,
        req.media_max_bytes,
        req.max_output_tokens,
        req.reasoning_effort,
        false,
    )?;
    let response =
        send_openai_responses_request(req.client, req.api_key, req.base_url, &payload, false)
            .await?;

    let body: Value = response
        .json()
        .await
        .map_err(|err| LlmError::provider(format!("invalid OpenAI chat JSON: {err}"), "json"))?;
    let reply = extract_response_output_text(&body)
        .ok_or_else(|| LlmError::provider("OpenAI chat returned empty text output", "provider"))?;
    let usage = extract_response_usage(&body);
    let metrics = recorder.finish(req.provider, req.model, false);

    Ok(ChatOutcome {
        reply,
        metrics,
        usage,
        fallback_used: false,
        agent: Default::default(),
    })
}

/// 流式 OpenAI Responses 聊天请求。
pub(crate) async fn openai_responses_stream_chat(
    req: &OpenAiResponsesChatRequest<'_>,
) -> Result<ChatOutcome, LlmError> {
    let stream = openai_responses_chat_stream(req).await?;
    collect_llm_stream(stream, req.provider, req.model).await
}

pub(crate) async fn openai_responses_chat_stream(
    req: &OpenAiResponsesChatRequest<'_>,
) -> Result<LlmStream, LlmError> {
    let recorder = MetricsRecorder::start();
    let payload = openai_responses_payload(
        req.messages,
        req.model,
        req.media_max_bytes,
        req.max_output_tokens,
        req.reasoning_effort,
        true,
    )?;
    let response =
        send_openai_responses_request(req.client, req.api_key, req.base_url, &payload, true)
            .await?;

    let frame_buffer = Vec::new();
    let answer = String::new();
    let completed_response: Option<Value> = None;
    let saw_completed = false;
    Ok(Box::pin(stream::unfold(
        ResponsesStreamState {
            response,
            frame_buffer,
            recorder,
            answer,
            completed_response,
            saw_completed,
            saw_done: false,
            allow_completed_response_fallback: req.allow_completed_response_fallback,
            finished: false,
        },
        |mut state| async move {
            let event = next_responses_stream_event(&mut state).await;
            event.map(|event| (event, state))
        },
    )))
}

struct ResponsesStreamState {
    response: reqwest::Response,
    frame_buffer: Vec<u8>,
    recorder: MetricsRecorder,
    answer: String,
    completed_response: Option<Value>,
    saw_completed: bool,
    saw_done: bool,
    allow_completed_response_fallback: bool,
    finished: bool,
}

async fn next_responses_stream_event(
    state: &mut ResponsesStreamState,
) -> Option<Result<LlmStreamEvent, LlmError>> {
    loop {
        if let Some(frame) = take_sse_frame(&mut state.frame_buffer) {
            let Some(event) = (match parse_sse_frame(&frame) {
                Ok(event) => event,
                Err(err) => return Some(Err(err)),
            }) else {
                continue;
            };
            if is_openai_responses_done_sentinel(&event.data) {
                state.saw_done = true;
                continue;
            }
            state.recorder.mark_event();
            match handle_openai_chat_stream_event(
                event,
                &mut state.recorder,
                &mut state.answer,
                &mut state.completed_response,
                &mut state.saw_completed,
            ) {
                Ok(Some(delta)) => return Some(Ok(LlmStreamEvent::TextDelta(delta))),
                Ok(None) => continue,
                Err(err) => return Some(Err(err)),
            }
        }

        if state.finished {
            return None;
        }

        if responses_stream_is_complete(state.saw_completed, &state.completed_response)
            || (state.saw_done && !state.answer.trim().is_empty())
        {
            if state.answer.trim().is_empty()
                && state.allow_completed_response_fallback
                && let Some(response) = state.completed_response.as_ref()
                && let Some(answer) = extract_response_output_text(response)
                && !answer.trim().is_empty()
            {
                // 只在没有真实 delta 时从 completed response 回补，保证最终正文来源单一。
                state.answer = answer.clone();
                state.recorder.mark_token();
                return Some(Ok(LlmStreamEvent::TextDelta(answer)));
            }
            let usage = state
                .completed_response
                .as_ref()
                .and_then(extract_response_usage);
            state.finished = true;
            return Some(Ok(LlmStreamEvent::Completed {
                usage,
                finish_reason: None,
                fallback_used: false,
            }));
        }

        match state.response.chunk().await {
            Ok(Some(chunk)) => {
                state.frame_buffer.extend_from_slice(&chunk);
            }
            Ok(None) => {
                if !state.frame_buffer.is_empty() {
                    let Some(event) = (match parse_sse_frame(&state.frame_buffer) {
                        Ok(event) => event,
                        Err(err) => return Some(Err(err)),
                    }) else {
                        state.frame_buffer.clear();
                        continue;
                    };
                    state.frame_buffer.clear();
                    if !is_openai_responses_done_sentinel(&event.data) {
                        state.recorder.mark_event();
                        match handle_openai_chat_stream_event(
                            event,
                            &mut state.recorder,
                            &mut state.answer,
                            &mut state.completed_response,
                            &mut state.saw_completed,
                        ) {
                            Ok(Some(delta)) => return Some(Ok(LlmStreamEvent::TextDelta(delta))),
                            Ok(None) => {}
                            Err(err) => return Some(Err(err)),
                        }
                    }
                }
                if state.answer.trim().is_empty()
                    && state.allow_completed_response_fallback
                    && let Some(response) = state.completed_response.as_ref()
                    && let Some(answer) = extract_response_output_text(response)
                    && !answer.trim().is_empty()
                {
                    // 只在没有真实 delta 时从 completed response 回补，保证最终正文来源单一。
                    state.answer = answer.clone();
                    state.recorder.mark_token();
                    return Some(Ok(LlmStreamEvent::TextDelta(answer)));
                }
                if !state.saw_completed {
                    state.finished = true;
                    return Some(Err(incomplete_stream_eof_error(
                        "OpenAI Responses chat stream ended before response.completed",
                        &state.answer,
                    )));
                }
                let usage = state
                    .completed_response
                    .as_ref()
                    .and_then(extract_response_usage);
                state.finished = true;
                return Some(Ok(LlmStreamEvent::Completed {
                    usage,
                    finish_reason: None,
                    fallback_used: false,
                }));
            }
            Err(err) => {
                return Some(Err(stream_transport_error(
                    format!("OpenAI chat stream failed: {err}"),
                    &state.answer,
                )));
            }
        }
    }
}

pub(crate) fn incomplete_stream_eof_error(message: &str, answer: &str) -> LlmError {
    let stage = if answer.trim().is_empty() {
        "stream"
    } else {
        "stream_after_delta"
    };
    LlmError::provider(message, stage)
}

pub(crate) fn stream_transport_error(message: String, answer: &str) -> LlmError {
    let stage = if answer.trim().is_empty() {
        "http"
    } else {
        "stream_after_delta"
    };
    LlmError::new("http_error", message, stage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::{Response, StatusCode, header},
        response::IntoResponse,
        routing::post,
    };
    use futures::{StreamExt, stream};
    use std::{convert::Infallible, sync::Arc, time::Duration};
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

    async fn never_closing_completed_handler() -> Response<Body> {
        let completed = Bytes::from_static(
            b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"prompt completion\"}}\n\n",
        );
        let body = Body::from_stream(
            stream::once(async move { Ok::<Bytes, Infallible>(completed) })
                .chain(stream::pending()),
        );
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(body)
            .unwrap()
    }

    async fn spawn_never_closing_completed_response() -> String {
        let app = Router::new().route("/v1/responses", post(never_closing_completed_handler));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/v1")
    }

    fn stream_req<'a>(
        client: &'a reqwest::Client,
        base_url: &'a str,
        messages: &'a [ChatMessage],
    ) -> OpenAiResponsesChatRequest<'a> {
        OpenAiResponsesChatRequest {
            stream: true,
            client,
            api_key: "test-key",
            base_url: Some(base_url),
            provider: "openai",
            model: "gpt-5.5",
            media_max_bytes: 10 * 1024 * 1024,
            max_output_tokens: 1200,
            reasoning_effort: None,
            messages,
            allow_completed_response_fallback: true,
        }
    }

    #[tokio::test]
    async fn openai_responses_stream_uses_completed_response_when_delta_is_missing() {
        let (base_url, state) = spawn_mock_responses(
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"stream fallback\"}}\n\n"
                .to_owned(),
            StatusCode::OK,
        )
        .await;
        let client = qq_maid_common::http_client::client();
        let messages = [ChatMessage::user("hi")];
        let req = stream_req(&client, &base_url, &messages);
        let outcome = openai_responses_stream_chat(&req).await.unwrap();

        assert_eq!(outcome.reply, "stream fallback");
        let state = state.lock().await;
        assert_eq!(state.calls, 1);
    }

    #[tokio::test]
    async fn ordinary_responses_stream_finishes_on_completed_without_http_eof() {
        let base_url = spawn_never_closing_completed_response().await;
        let client = qq_maid_common::http_client::client();
        let messages = [ChatMessage::user("hi")];
        let req = stream_req(&client, &base_url, &messages);

        let outcome = tokio::time::timeout(
            Duration::from_millis(300),
            openai_responses_stream_chat(&req),
        )
        .await
        .expect("ordinary Responses stream must finish from response.completed")
        .unwrap();

        assert_eq!(outcome.reply, "prompt completion");
    }

    #[tokio::test]
    async fn openai_responses_stream_requires_completed_after_delta() {
        let (base_url, _state) = spawn_mock_responses(
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"半截\"}\n\n"
                .to_owned(),
            StatusCode::OK,
        )
        .await;
        let client = qq_maid_common::http_client::client();
        let messages = [ChatMessage::user("hi")];
        let req = stream_req(&client, &base_url, &messages);

        let err = openai_responses_stream_chat(&req).await.unwrap_err();

        assert_eq!(err.stage, "stream_after_delta");
        assert!(err.message.contains("response.completed"));
    }

    #[tokio::test]
    async fn openai_responses_stream_accepts_delta_then_completed() {
        let (base_url, _state) = spawn_mock_responses(
            concat!(
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你\"}\n\n",
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"好\"}\n\n",
                "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"你好\"}}\n\n",
            )
            .to_owned(),
            StatusCode::OK,
        )
        .await;
        let client = qq_maid_common::http_client::client();
        let messages = [ChatMessage::user("hi")];
        let req = stream_req(&client, &base_url, &messages);

        let outcome = openai_responses_stream_chat(&req).await.unwrap();

        assert_eq!(outcome.reply, "你好");
    }

    #[tokio::test]
    async fn openai_responses_stream_skips_done_between_delta_and_completed() {
        let (base_url, _state) = spawn_mock_responses(
            concat!(
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你好\"}\n\n",
                "data: [DONE]\n\n",
                "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"你好\"}}\n\n",
            )
            .to_owned(),
            StatusCode::OK,
        )
        .await;
        let client = qq_maid_common::http_client::client();
        let messages = [ChatMessage::user("hi")];
        let req = stream_req(&client, &base_url, &messages);

        let outcome = openai_responses_stream_chat(&req).await.unwrap();

        assert_eq!(outcome.reply, "你好");
    }

    #[tokio::test]
    async fn openai_responses_stream_skips_done_after_completed_at_eof() {
        let (base_url, _state) = spawn_mock_responses(
            concat!(
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你好\"}\n\n",
                "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"你好\"}}\n\n",
                "data: [DONE]",
            )
            .to_owned(),
            StatusCode::OK,
        )
        .await;
        let client = qq_maid_common::http_client::client();
        let messages = [ChatMessage::user("hi")];
        let req = stream_req(&client, &base_url, &messages);

        let outcome = openai_responses_stream_chat(&req).await.unwrap();

        assert_eq!(outcome.reply, "你好");
    }

    #[tokio::test]
    async fn openai_responses_stream_skips_null_and_metadata_before_text() {
        let (base_url, _state) = spawn_mock_responses(
            concat!(
                "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_test\"}}\n\n",
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":null}\n\n",
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\"}\n\n",
                "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"可以\"}\n\n",
                "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"output_text\":\"可以\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
            )
            .to_owned(),
            StatusCode::OK,
        )
        .await;
        let client = qq_maid_common::http_client::client();
        let messages = [ChatMessage::user("hi")];
        let req = stream_req(&client, &base_url, &messages);

        let outcome = openai_responses_stream_chat(&req).await.unwrap();

        assert_eq!(outcome.reply, "可以");
        assert!(!outcome.reply.starts_with("null"));
        assert_eq!(outcome.usage.unwrap().total_tokens, Some(2));
    }

    #[tokio::test]
    async fn openai_responses_non_stream_still_extracts_text_and_usage() {
        let (base_url, state) = spawn_mock_responses(
            serde_json::json!({
                "output_text": "non stream ok",
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 2,
                    "total_tokens": 3
                }
            })
            .to_string(),
            StatusCode::OK,
        )
        .await;
        let client = qq_maid_common::http_client::client();
        let messages = [ChatMessage::user("hi")];
        let mut req = stream_req(&client, &base_url, &messages);
        req.stream = false;

        let outcome = openai_responses_non_stream_chat(&req).await.unwrap();

        assert_eq!(outcome.reply, "non stream ok");
        assert_eq!(outcome.usage.unwrap().total_tokens, Some(3));
        assert_eq!(state.lock().await.calls, 1);
    }
}

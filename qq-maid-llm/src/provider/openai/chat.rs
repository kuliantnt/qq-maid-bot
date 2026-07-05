//! OpenAI 兼容 Chat Completions adapter。
//!
//! OpenAI fallback 和 DeepSeek 都复用同一套 `/chat/completions` HTTP/SSE 实现，
//! 只在 base URL、API key 和模型规则上区分。

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures::stream;
use reqwest::{
    StatusCode, header,
    header::{HeaderName, HeaderValue},
};
use serde_json::{Value, json};
use std::{collections::VecDeque, path::Path};

use crate::{
    config::HttpAuthConfig,
    error::LlmError,
    metrics::MetricsRecorder,
    provider::{
        ChatOutcome, LlmStream, LlmStreamEvent, collect_llm_stream,
        types::{ChatMessage, ChatRole, TokenUsage},
    },
    sse::{parse_sse_frame, take_sse_frame},
};
use qq_maid_common::input_part::{MediaStatus, MessageInputPart, MessageMedia};

use super::fallback::{
    should_retry_non_stream_after_empty_stream, should_retry_non_stream_after_stream_error,
};
use super::responses::{incomplete_stream_eof_error, stream_transport_error};

const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// OpenAI 兼容 Chat Completions 客户端包装。
#[derive(Clone)]
pub(crate) struct ChatCompletionsClient {
    client: reqwest::Client,
    api_key: String,
    base_url: Option<String>,
    auth: HttpAuthConfig,
}

impl ChatCompletionsClient {
    pub(crate) fn new(
        api_key: impl Into<String>,
        base_url: Option<&str>,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            client: http_client,
            api_key: api_key.into(),
            base_url: base_url
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            auth: HttpAuthConfig::default(),
        }
    }

    pub(crate) fn with_auth(mut self, auth: HttpAuthConfig) -> Self {
        self.auth = auth;
        self
    }
}

/// 执行可选流式 Chat Completions，并在流式失败或空流时补一次非流式请求。
pub(crate) async fn chat_completions_with_stream_fallback(
    stream: bool,
    client: &ChatCompletionsClient,
    provider: &str,
    model: &str,
    max_output_tokens: u64,
    messages: &[ChatMessage],
) -> Result<ChatOutcome, LlmError> {
    if stream {
        match stream_completion(client, provider, model, max_output_tokens, messages).await {
            Ok(outcome) => {
                if !should_retry_non_stream_after_empty_stream(&outcome) {
                    return Ok(outcome);
                }
                tracing::warn!(
                    provider,
                    model = %model,
                    "streaming chat completions returned empty reply; retrying once with non-stream request"
                );
            }
            Err(err) => {
                // 兼容网关经常只在 SSE 链路上不稳定；先补同 provider 非流式请求，
                // 避免过早切换到跨模型候选并产生额外行为差异。
                if !should_retry_non_stream_after_stream_error(&err) {
                    return Err(err);
                }
                tracing::warn!(
                    provider,
                    model = %model,
                    error_code = err.code.as_str(),
                    error_stage = err.stage.as_str(),
                    "streaming chat completions failed; retrying once with non-stream request"
                );
            }
        }
    }

    non_stream_completion(client, provider, model, max_output_tokens, messages).await
}

fn chat_completions_url(base_url: Option<&str>) -> String {
    let base_url = base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(OPENAI_DEFAULT_BASE_URL);
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

fn chat_completions_payload(
    messages: &[ChatMessage],
    model: &str,
    max_output_tokens: u64,
    stream: bool,
) -> Result<Value, LlmError> {
    let messages = chat_completions_messages(messages)?;
    let mut payload = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_output_tokens,
    });
    if stream {
        payload["stream"] = json!(true);
        // 部分兼容网关忽略该选项；官方接口会在最终 chunk 返回 usage。
        payload["stream_options"] = json!({"include_usage": true});
    }
    Ok(payload)
}

pub(super) fn chat_completions_messages(messages: &[ChatMessage]) -> Result<Vec<Value>, LlmError> {
    if messages.is_empty() {
        return Err(LlmError::new(
            "bad_request",
            "messages must not be empty",
            "request",
        ));
    }
    let converted = messages
        .iter()
        .filter(|message| message_has_payload(message))
        .map(chat_completions_message)
        .collect::<Result<Vec<_>, _>>()?;
    if converted.is_empty() {
        return Err(LlmError::new(
            "bad_request",
            "messages must contain non-empty content",
            "request",
        ));
    }
    Ok(converted)
}

fn chat_completions_message(message: &ChatMessage) -> Result<Value, LlmError> {
    let role = match message.role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
    };
    Ok(json!({
        "role": role,
        "content": chat_completions_content(message)?,
    }))
}

fn message_has_payload(message: &ChatMessage) -> bool {
    !message.content.trim().is_empty() || !message.content_parts.is_empty()
}

fn chat_completions_content(message: &ChatMessage) -> Result<Vec<Value>, LlmError> {
    if message.role != ChatRole::User || message.content_parts.is_empty() {
        return Ok(vec![
            json!({"type": "text", "text": message.content.as_str()}),
        ]);
    }
    let mut content = Vec::new();
    for part in message.effective_content_parts() {
        match part {
            MessageInputPart::Text { text, .. } => {
                if !text.trim().is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
            }
            MessageInputPart::Image { media } => {
                ensure_media_available(media.status, "图片")?;
                let url = image_reference_for_openai(&media)?;
                content.push(json!({
                    "type": "image_url",
                    "image_url": {"url": url},
                }));
            }
            MessageInputPart::File { .. } | MessageInputPart::Unknown { .. } => {
                return Err(file_unsupported_error());
            }
        }
    }
    if content.is_empty() {
        return Err(LlmError::new(
            "bad_request",
            "messages must contain non-empty content",
            "request",
        ));
    }
    Ok(content)
}

fn ensure_media_available(status: MediaStatus, label: &str) -> Result<(), LlmError> {
    match status {
        MediaStatus::Available => Ok(()),
        MediaStatus::MissingReadableUrl => Err(image_reference_error()),
        MediaStatus::SizeExceeded => Err(LlmError::new(
            "unsupported_input_part",
            format!("{label}太大了，暂时无法处理。"),
            "request",
        )),
        MediaStatus::UnsupportedType => Err(LlmError::new(
            "unsupported_input_part",
            format!("我收到这个{label}了，但目前还不能读取这种类型。"),
            "request",
        )),
        MediaStatus::DownloadFailed => Err(LlmError::new(
            "unsupported_input_part",
            format!("{label}已收到，但下载失败，请重新发送一次。"),
            "request",
        )),
        MediaStatus::Expired => Err(LlmError::new(
            "unsupported_input_part",
            format!("{label}已收到，但访问地址已过期，请重新发送一次。"),
            "request",
        )),
    }
}

pub(crate) fn image_reference_for_openai(media: &MessageMedia) -> Result<String, LlmError> {
    if let Some(local_path) = media
        .local_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return local_image_data_url(local_path, media.mime_type.as_deref());
    }
    media
        .remote_url()
        .map(str::to_owned)
        .ok_or_else(image_reference_error)
}

fn local_image_data_url(path: &str, mime_type: Option<&str>) -> Result<String, LlmError> {
    let bytes = std::fs::read(path).map_err(|_| {
        LlmError::new(
            "unsupported_input_part",
            "图片已收到，但本地读取失败，请重新发送一次。",
            "request",
        )
    })?;
    let mime_type = clean_image_mime_type(mime_type)
        .or_else(|| infer_image_mime_type_from_path(path))
        .unwrap_or("image/jpeg");
    Ok(format!(
        "data:{mime_type};base64,{}",
        BASE64_STANDARD.encode(bytes)
    ))
}

fn clean_image_mime_type(value: Option<&str>) -> Option<&'static str> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/png" => Some("image/png"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "image/bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn infer_image_mime_type_from_path(path: &str) -> Option<&'static str> {
    match Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("bmp") => Some("image/bmp"),
        _ => None,
    }
}

pub(crate) fn image_reference_error() -> LlmError {
    LlmError::new(
        "unsupported_input_part",
        "我收到图片了，但当前入口没有提供可读取图片内容。你可以补充文字说明，我先帮你记录。",
        "request",
    )
}

pub(crate) fn file_unsupported_error() -> LlmError {
    LlmError::new(
        "unsupported_input_part",
        "我收到这个文件了，但目前还不能读取这种文件类型。",
        "request",
    )
}

pub(super) async fn send_chat_completions_request(
    client: &ChatCompletionsClient,
    payload: &Value,
    stream: bool,
) -> Result<reqwest::Response, LlmError> {
    let header_name = HeaderName::from_bytes(client.auth.header.as_bytes()).map_err(|err| {
        LlmError::config(format!(
            "invalid Chat Completions auth header `{}`: {err}",
            client.auth.header
        ))
    })?;
    let auth_value = match client.auth.scheme.as_deref() {
        Some(scheme) if !scheme.trim().is_empty() => {
            format!("{} {}", scheme.trim(), client.api_key)
        }
        _ => client.api_key.clone(),
    };
    let auth_value = HeaderValue::from_str(&auth_value).map_err(|err| {
        LlmError::config(format!(
            "invalid Chat Completions auth value for header `{}`: {err}",
            client.auth.header
        ))
    })?;
    let mut request = client
        .client
        .post(chat_completions_url(client.base_url.as_deref()))
        .header(header_name, auth_value)
        .json(payload);
    if stream {
        request = request.header(header::ACCEPT, "text/event-stream");
    }
    let response = request.send().await.map_err(|err| {
        if err.is_timeout() {
            LlmError::timeout("http")
        } else {
            let context = if stream {
                "Chat Completions stream request failed"
            } else {
                "Chat Completions request failed"
            };
            LlmError::http(format!("{context}: {err}"))
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(chat_status_error(status, response).await);
    }
    Ok(response)
}

async fn chat_status_error(status: StatusCode, response: reqwest::Response) -> LlmError {
    let detail = response.text().await.unwrap_or_default();
    let detail = truncate_error_detail(detail.trim(), 500);
    let message = if detail.is_empty() {
        format!("Chat Completions returned HTTP {}", status.as_u16())
    } else {
        format!(
            "Chat Completions returned HTTP {}: {detail}",
            status.as_u16()
        )
    };
    // OpenAI 兼容网关可能把安全拦截放在 HTTP 400 返回体中；这不是本地请求格式错误，
    // 需要保留独立错误码，避免 Gateway 向用户展示“请求格式有误”的误导文案。
    if is_prompt_blocked_error(&detail) {
        return LlmError::new("safety_blocked", message, "http");
    }
    match status.as_u16() {
        401 | 403 => LlmError::config(message),
        400 | 404 | 422 => LlmError::new("bad_request", message, "http"),
        429 => LlmError::new("rate_limited", message, "http"),
        500..=599 => LlmError::new("upstream_unavailable", message, "http"),
        _ => LlmError::http(message),
    }
}

fn truncate_error_detail(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let mut truncated = value.chars().take(limit).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn is_prompt_blocked_error(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("prompt_blocked")
        || lower.contains("moderation policy")
        || lower.contains("content policy")
        || lower.contains("safety policy")
}

pub(crate) async fn non_stream_completion(
    client: &ChatCompletionsClient,
    provider: &str,
    model: &str,
    max_output_tokens: u64,
    messages: &[ChatMessage],
) -> Result<ChatOutcome, LlmError> {
    let recorder = MetricsRecorder::start();
    let payload = chat_completions_payload(messages, model, max_output_tokens, false)?;
    let response = send_chat_completions_request(client, &payload, false).await?;
    let body: Value = response.json().await.map_err(|err| {
        LlmError::provider(format!("invalid Chat Completions JSON: {err}"), "json")
    })?;
    let reply = extract_chat_completion_text(&body).ok_or_else(|| {
        LlmError::provider("Chat Completions returned empty text output", "provider")
    })?;
    let usage = extract_chat_completion_usage(&body);
    let metrics = recorder.finish(provider, model, false);

    Ok(ChatOutcome {
        reply,
        metrics,
        usage,
        fallback_used: false,
        executed_tools: Vec::new(),
        tool_results: Vec::new(),
    })
}

pub(crate) async fn stream_completion(
    client: &ChatCompletionsClient,
    provider: &str,
    model: &str,
    max_output_tokens: u64,
    messages: &[ChatMessage],
) -> Result<ChatOutcome, LlmError> {
    let stream =
        chat_completions_stream(client, provider, model, max_output_tokens, messages, true).await?;
    collect_llm_stream(stream, provider, model).await
}

pub(crate) async fn chat_completions_stream(
    client: &ChatCompletionsClient,
    _provider: &str,
    _model: &str,
    max_output_tokens: u64,
    messages: &[ChatMessage],
    allow_completed_message_fallback: bool,
) -> Result<LlmStream, LlmError> {
    let recorder = MetricsRecorder::start();
    let payload = chat_completions_payload(messages, _model, max_output_tokens, true)?;
    let response = send_chat_completions_request(client, &payload, true).await?;
    let frame_buffer = Vec::new();
    let answer = String::new();
    let final_message = String::new();
    let usage = None;

    Ok(Box::pin(stream::unfold(
        ChatStreamState {
            response,
            frame_buffer,
            recorder,
            answer,
            final_message,
            usage,
            pending_events: VecDeque::new(),
            allow_completed_message_fallback,
            saw_done: false,
            finish_reason: None,
            finished: false,
        },
        |mut state| async move {
            let event = next_chat_stream_event(&mut state).await;
            event.map(|event| (event, state))
        },
    )))
}

fn handle_chat_stream_event(
    data: &str,
    recorder: &mut MetricsRecorder,
    answer: &mut String,
    final_message: &mut String,
    usage: &mut Option<TokenUsage>,
) -> Result<(Vec<LlmStreamEvent>, Option<String>), LlmError> {
    let value = serde_json::from_str::<Value>(data).map_err(|err| {
        LlmError::provider(
            format!("invalid Chat Completions stream JSON: {err}"),
            "sse",
        )
    })?;
    if let Some(event_usage) = extract_chat_completion_usage(&value) {
        *usage = Some(event_usage);
    }
    let mut events = Vec::new();
    let Some(choices) = value.get("choices").and_then(Value::as_array) else {
        return Ok((events, None));
    };
    let mut finish_reason = None;
    for choice in choices {
        if let Some(delta_value) = choice.get("delta") {
            let content = delta_value.get("content");
            if let Some(delta) = extract_content_value(content)
                && !delta.is_empty()
            {
                recorder.mark_token();
                answer.push_str(&delta);
                events.push(LlmStreamEvent::TextDelta(delta));
            } else if content.is_some_and(|value| !value.is_null()) {
                trace_ignored_chat_stream_content("delta.content", content);
            }
        }
        if let Some(message_value) = choice.get("message") {
            let content = message_value.get("content");
            if let Some(message) = extract_content_value(content)
                && !message.is_empty()
            {
                final_message.push_str(&message);
            } else if content.is_some_and(|value| !value.is_null()) {
                trace_ignored_chat_stream_content("message.content", content);
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str)
            && !reason.trim().is_empty()
        {
            finish_reason = Some(reason.to_owned());
        }
    }
    Ok((events, finish_reason))
}

struct ChatStreamState {
    response: reqwest::Response,
    frame_buffer: Vec<u8>,
    recorder: MetricsRecorder,
    answer: String,
    final_message: String,
    usage: Option<TokenUsage>,
    pending_events: VecDeque<LlmStreamEvent>,
    allow_completed_message_fallback: bool,
    saw_done: bool,
    finish_reason: Option<String>,
    finished: bool,
}

async fn next_chat_stream_event(
    state: &mut ChatStreamState,
) -> Option<Result<LlmStreamEvent, LlmError>> {
    loop {
        if let Some(event) = state.pending_events.pop_front() {
            return Some(Ok(event));
        }
        if let Some(frame) = take_sse_frame(&mut state.frame_buffer) {
            let Some(event) = (match parse_sse_frame(&frame) {
                Ok(event) => event,
                Err(err) => return Some(Err(err)),
            }) else {
                continue;
            };
            if event.data.trim() == "[DONE]" {
                state.saw_done = true;
                continue;
            }
            state.recorder.mark_event();
            match handle_chat_stream_event(
                &event.data,
                &mut state.recorder,
                &mut state.answer,
                &mut state.final_message,
                &mut state.usage,
            ) {
                Ok((events, finish_reason)) => {
                    if finish_reason.is_some() {
                        state.finish_reason = finish_reason;
                    }
                    state.pending_events.extend(events);
                }
                Err(err) => return Some(Err(err)),
            }
            continue;
        }

        if state.finished {
            return None;
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
                    if event.data.trim() == "[DONE]" {
                        state.saw_done = true;
                    } else {
                        state.recorder.mark_event();
                        match handle_chat_stream_event(
                            &event.data,
                            &mut state.recorder,
                            &mut state.answer,
                            &mut state.final_message,
                            &mut state.usage,
                        ) {
                            Ok((events, finish_reason)) => {
                                if finish_reason.is_some() {
                                    state.finish_reason = finish_reason;
                                }
                                state.pending_events.extend(events);
                            }
                            Err(err) => return Some(Err(err)),
                        }
                    }
                }
                if state.answer.trim().is_empty()
                    && state.allow_completed_message_fallback
                    && (state.saw_done || state.finish_reason.is_some())
                    && !state.final_message.trim().is_empty()
                {
                    // 仅在没有真实 delta 时回补 completed message，避免把两套正文拼接。
                    state.answer = state.final_message.clone();
                    state.recorder.mark_token();
                    return Some(Ok(LlmStreamEvent::TextDelta(state.final_message.clone())));
                }
                if !state.saw_done && state.finish_reason.is_none() {
                    state.finished = true;
                    return Some(Err(incomplete_stream_eof_error(
                        "Chat Completions stream ended before [DONE] or finish_reason",
                        &state.answer,
                    )));
                }
                state.finished = true;
                return Some(Ok(LlmStreamEvent::Completed {
                    usage: state.usage.clone(),
                    finish_reason: state.finish_reason.clone(),
                    fallback_used: false,
                }));
            }
            Err(err) => {
                return Some(Err(stream_transport_error(
                    format!("Chat Completions stream failed: {err}"),
                    &state.answer,
                )));
            }
        }
    }
}

pub(super) fn extract_chat_completion_text(body: &Value) -> Option<String> {
    let choices = body.get("choices").and_then(Value::as_array)?;
    let mut parts = Vec::new();
    for choice in choices {
        let Some(text) = choice
            .get("message")
            .and_then(|message| extract_content_value(message.get("content")))
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
        else {
            continue;
        };
        parts.push(text);
    }
    let text = parts.join("");
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn extract_content_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.to_owned()),
        Value::Array(items) => {
            let text = items
                .iter()
                .filter_map(|item| {
                    let item_type = item.get("type").and_then(Value::as_str);
                    if matches!(item_type, Some("text") | None) {
                        item.get("text").and_then(Value::as_str)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn trace_ignored_chat_stream_content(field: &str, value: Option<&Value>) {
    let Some(value) = value else {
        return;
    };
    let kind = match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array_without_text",
        Value::Object(_) => "object",
    };
    tracing::trace!(
        field,
        kind,
        "ignored non-text Chat Completions stream content"
    );
}

pub(super) fn extract_chat_completion_usage(body: &Value) -> Option<TokenUsage> {
    let usage = body.get("usage")?;
    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64);
    let cached_input_tokens = usage
        .get("prompt_tokens_details")
        .or_else(|| usage.get("input_tokens_details"))
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64);
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64);
    let total_tokens = usage.get("total_tokens").and_then(Value::as_u64);
    if matches!(
        (
            input_tokens,
            output_tokens,
            total_tokens,
            cached_input_tokens
        ),
        (None | Some(0), None | Some(0), None | Some(0), None)
    ) {
        return None;
    }
    Some(TokenUsage {
        input_tokens,
        cached_input_tokens,
        output_tokens,
        total_tokens,
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
    use qq_maid_common::input_part::MessageMedia;
    use std::sync::Arc;
    use tokio::{net::TcpListener, sync::Mutex};

    #[derive(Debug)]
    struct MockChatState {
        bodies: Vec<String>,
        status: StatusCode,
        requests: Vec<Value>,
    }

    async fn mock_chat_handler(
        State(state): State<Arc<Mutex<MockChatState>>>,
        body: Body,
    ) -> impl IntoResponse {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let request: Value = serde_json::from_slice(&bytes).unwrap();
        let mut state = state.lock().await;
        state.requests.push(request);
        let body = state.bodies.remove(0);
        (
            state.status,
            [(header::CONTENT_TYPE, "text/event-stream")],
            body,
        )
    }

    async fn spawn_mock_chat(
        bodies: Vec<String>,
        status: StatusCode,
    ) -> (String, Arc<Mutex<MockChatState>>) {
        let state = Arc::new(Mutex::new(MockChatState {
            bodies,
            status,
            requests: Vec::new(),
        }));
        let app = Router::new()
            .route("/v1/chat/completions", post(mock_chat_handler))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1"), state)
    }

    #[test]
    fn chat_completions_payload_keeps_reply_context_before_image_parts() {
        let payload = chat_completions_payload(
            &[ChatMessage::user_with_parts(
                "[reply message_id=quoted-1]\n上一条\n[/reply]\n看图",
                vec![
                    MessageInputPart::text("[reply message_id=quoted-1]\n上一条\n[/reply]\n"),
                    MessageInputPart::text("看图"),
                    MessageInputPart::image(MessageMedia {
                        mime_type: Some("image/jpeg".to_owned()),
                        filename: Some("a.jpg".to_owned()),
                        url: Some("https://example.test/a.jpg".to_owned()),
                        ..Default::default()
                    }),
                ],
            )],
            "gpt-test",
            1200,
            false,
        )
        .unwrap();
        let content = payload["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content[0]["type"], "text");
        assert_eq!(
            content[0]["text"],
            "[reply message_id=quoted-1]\n上一条\n[/reply]\n"
        );
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "看图");
        assert_eq!(content[2]["type"], "image_url");
        assert_eq!(content[2]["image_url"]["url"], "https://example.test/a.jpg");
    }

    #[test]
    fn chat_completions_payload_rejects_file_url_image_part() {
        let err = chat_completions_payload(
            &[ChatMessage::user_with_parts(
                "看图",
                vec![
                    MessageInputPart::text("看图"),
                    MessageInputPart::image(MessageMedia {
                        mime_type: Some("image/jpeg".to_owned()),
                        filename: Some("a.jpg".to_owned()),
                        url: Some("file://C:\\Users\\ThinkPad\\Pictures\\a.jpg".to_owned()),
                        ..Default::default()
                    }),
                ],
            )],
            "gpt-test",
            1200,
            false,
        )
        .unwrap_err();

        assert_eq!(err.code, "unsupported_input_part");
        assert!(err.message.contains("当前入口没有提供可读取图片内容"));
        assert!(!err.message.contains("C:\\Users"));
    }

    #[test]
    fn chat_completions_payload_uses_local_path_as_data_url() {
        let path = std::env::temp_dir().join(format!(
            "qq-maid-chat-local-image-{}.jpg",
            std::process::id()
        ));
        std::fs::write(&path, b"fake-jpg").unwrap();

        let payload = chat_completions_payload(
            &[ChatMessage::user_with_parts(
                "看图",
                vec![MessageInputPart::image(MessageMedia {
                    mime_type: Some("image/jpeg".to_owned()),
                    filename: Some("a.jpg".to_owned()),
                    local_path: Some(path.to_string_lossy().to_string()),
                    ..Default::default()
                })],
            )],
            "gpt-test",
            1200,
            false,
        )
        .unwrap();
        let image_url = payload["messages"][0]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap();

        assert!(image_url.starts_with("data:image/jpeg;base64,"));
        assert!(!image_url.contains(path.to_string_lossy().as_ref()));
    }

    #[tokio::test]
    async fn non_stream_chat_completion_extracts_text_and_usage() {
        let (base_url, state) = spawn_mock_chat(
            vec![
                json!({
                    "choices": [{"message": {"content": "ok"}}],
                    "usage": {
                        "prompt_tokens": 2,
                        "completion_tokens": 3,
                        "total_tokens": 5,
                        "prompt_tokens_details": {"cached_tokens": 0}
                    }
                })
                .to_string(),
            ],
            StatusCode::OK,
        )
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let outcome = non_stream_completion(
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "ok");
        assert_eq!(outcome.usage.unwrap().cached_input_tokens, Some(0));
        assert_eq!(
            state.lock().await.requests[0]["messages"][0]["content"][0]["type"],
            "text"
        );
    }

    #[tokio::test]
    async fn stream_chat_completion_extracts_delta() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"好\"}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_owned();
        let (base_url, _state) = spawn_mock_chat(vec![body], StatusCode::OK).await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let outcome = stream_completion(
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "你好");
        assert_eq!(outcome.usage.unwrap().total_tokens, Some(3));
    }

    #[tokio::test]
    async fn stream_chat_completion_skips_null_and_non_body_chunks() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":null}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
            "data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"可以\"}}]}\n\n",
            "data: [DONE]\n\n",
        )
        .to_owned();
        let (base_url, _state) = spawn_mock_chat(vec![body], StatusCode::OK).await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let outcome = stream_completion(
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "可以");
        assert!(!outcome.reply.starts_with("null"));
        assert_eq!(outcome.usage.unwrap().total_tokens, Some(3));
    }

    #[tokio::test]
    async fn stream_chat_completion_requires_done_after_delta() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"半截\"}}]}\n\n".to_owned();
        let (base_url, _state) = spawn_mock_chat(vec![body], StatusCode::OK).await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let err = stream_completion(
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap_err();

        assert_eq!(err.stage, "stream_after_delta");
        assert!(err.message.contains("[DONE]"));
    }

    #[tokio::test]
    async fn stream_chat_completion_accepts_finish_reason_without_done() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"好\"},\"finish_reason\":\"stop\"}]}\n\n",
        )
        .to_owned();
        let (base_url, _state) = spawn_mock_chat(vec![body], StatusCode::OK).await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let outcome = stream_completion(
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "你好");
    }

    #[tokio::test]
    async fn empty_stream_retries_non_stream() {
        let (base_url, state) = spawn_mock_chat(
            vec![
                "data: [DONE]\n\n".to_owned(),
                json!({"choices": [{"message": {"content": "retry ok"}}]}).to_string(),
            ],
            StatusCode::OK,
        )
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let outcome = chat_completions_with_stream_fallback(
            true,
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "retry ok");
        assert_eq!(state.lock().await.requests.len(), 2);
    }

    #[tokio::test]
    async fn chat_with_stream_fallback_retries_non_stream_after_stream_parse_error() {
        let (base_url, state) = spawn_mock_chat(
            vec![
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"半截\"}}]}\n\n",
                    "data: {not-json}\n\n",
                )
                .to_owned(),
                json!({"choices": [{"message": {"content": "non stream ok"}}]}).to_string(),
            ],
            StatusCode::OK,
        )
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let outcome = chat_completions_with_stream_fallback(
            true,
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap();

        assert_eq!(outcome.reply, "non stream ok");
        let requests = &state.lock().await.requests;
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["stream"], true);
        assert!(requests[1].get("stream").is_none());
    }

    #[tokio::test]
    async fn raw_stream_chat_does_not_retry_non_stream_after_delta_error() {
        let (base_url, state) = spawn_mock_chat(
            vec![
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"半截\"}}]}\n\n",
                    "data: {not-json}\n\n",
                )
                .to_owned(),
            ],
            StatusCode::OK,
        )
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let err = stream_completion(
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap_err();

        assert_eq!(err.stage, "sse");
        assert_eq!(state.lock().await.requests.len(), 1);
    }

    #[tokio::test]
    async fn prompt_blocked_error_keeps_safety_code() {
        let (base_url, _state) = spawn_mock_chat(
            vec![
                json!({
                    "error": {
                        "message": "request blocked by moderation policy",
                        "type": "prompt_blocked"
                    }
                })
                .to_string(),
            ],
            StatusCode::BAD_REQUEST,
        )
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let err = non_stream_completion(
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "safety_blocked");
        assert_eq!(err.stage, "http");
        assert!(err.message.contains("prompt_blocked"));
    }

    #[tokio::test]
    async fn non_stream_empty_reply_is_error() {
        let (base_url, _state) = spawn_mock_chat(
            vec![json!({"choices": [{"message": {"content": ""}}]}).to_string()],
            StatusCode::OK,
        )
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let err = non_stream_completion(
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "provider_error");
    }

    #[tokio::test]
    async fn status_codes_are_classified() {
        let (base_url, _state) = spawn_mock_chat(
            vec!["rate limited".to_owned()],
            StatusCode::TOO_MANY_REQUESTS,
        )
        .await;
        let client =
            ChatCompletionsClient::new("test-key", Some(&base_url), reqwest::Client::new());

        let err = non_stream_completion(
            &client,
            "openai",
            "gpt-test",
            1200,
            &[ChatMessage::user("hi")],
        )
        .await
        .unwrap_err();

        assert_eq!(err.code, "rate_limited");
        assert!(err.message.contains("HTTP 429"));
    }

    #[test]
    fn custom_endpoint_is_used() {
        assert_eq!(
            chat_completions_url(Some("https://proxy.example/v1/")),
            "https://proxy.example/v1/chat/completions"
        );
    }
}

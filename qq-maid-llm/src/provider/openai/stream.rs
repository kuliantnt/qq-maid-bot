//! OpenAI Responses SSE 解析与事件处理。
//!
//! reqwest 返回的 chunk 不保证 UTF-8 或 frame 边界，且上游兼容层可能混用
//! `event:` 与 JSON `type`。这里统一接住原始 SSE，再把事件语义转换成聊天正文和
//! completed response，避免 Responses 主链路在多个位置重复解析。

use serde_json::Value;

use crate::{error::LlmError, metrics::MetricsRecorder, sse::SseFrame};

use super::extract::extract_completed_response;

/// Responses 流只有在 `response.completed` 携带完整 response 时才能安全定稿。
///
/// `[DONE]` 在部分兼容网关中可能先于 `response.completed` 到达，因此它本身不能
/// 证明 function call 参数已经完整；调用方应继续等待完整 completed response。
pub(crate) fn responses_stream_is_complete(
    saw_completed: bool,
    completed_response: &Option<Value>,
) -> bool {
    saw_completed && completed_response.is_some()
}

pub(crate) fn is_openai_responses_done_sentinel(data: &str) -> bool {
    data.trim() == "[DONE]"
}

/// 处理 OpenAI Responses SSE 事件。
pub(crate) fn handle_openai_chat_stream_event(
    event: SseFrame,
    recorder: &mut MetricsRecorder,
    answer: &mut String,
    completed_response: &mut Option<Value>,
    saw_completed: &mut bool,
) -> Result<Option<String>, LlmError> {
    let value = serde_json::from_str::<Value>(&event.data).map_err(|err| {
        LlmError::provider(format!("invalid OpenAI chat stream JSON: {err}"), "sse")
    })?;
    let event_type = event
        .event
        .as_deref()
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("");

    match event_type {
        "response.output_text.delta" | "response.refusal.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str)
                && !delta.is_empty()
            {
                recorder.mark_token();
                answer.push_str(delta);
                return Ok(Some(delta.to_owned()));
            } else if value.get("delta").is_some_and(|delta| !delta.is_null()) {
                trace_ignored_responses_stream_delta(event_type, value.get("delta"));
            }
        }
        "response.completed" => {
            *saw_completed = true;
            *completed_response = extract_completed_response(&value);
        }
        "response.failed" | "response.incomplete" | "error" => {
            let message = stream_error_message(&value)
                .unwrap_or_else(|| format!("OpenAI chat stream event {event_type}"));
            return Err(LlmError::provider(message, "sse"));
        }
        _ => {}
    }

    Ok(None)
}

fn trace_ignored_responses_stream_delta(event_type: &str, delta: Option<&Value>) {
    let Some(delta) = delta else {
        return;
    };
    let kind = match delta {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "empty_string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    tracing::trace!(
        event_type,
        kind,
        "ignored non-text OpenAI Responses stream delta"
    );
}

fn stream_error_message(value: &Value) -> Option<String> {
    value
        .get("error")
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("error"))
        })
        .and_then(|error| error.get("message").or(Some(error)))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::{parse_sse_frame, take_sse_frame};

    #[test]
    fn parses_sse_frames_across_chunks() {
        let mut buffer = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你"
            .as_bytes()
            .to_vec();
        assert!(take_sse_frame(&mut buffer).is_none());
        buffer.extend_from_slice("好\"}\n\n".as_bytes());

        let frame = take_sse_frame(&mut buffer).unwrap();
        let parsed = parse_sse_frame(&frame).unwrap().unwrap();

        assert_eq!(parsed.event.as_deref(), Some("response.output_text.delta"));
        assert!(parsed.data.contains("你好"));
    }

    #[test]
    fn parses_crlf_delimited_frame() {
        let mut buffer = b"event: done\r\ndata: {\"ok\":true}\r\n\r\n".to_vec();
        let frame = take_sse_frame(&mut buffer).unwrap();
        let parsed = parse_sse_frame(&frame).unwrap().unwrap();

        assert_eq!(parsed.event.as_deref(), Some("done"));
        assert_eq!(parsed.data, "{\"ok\":true}");
        assert!(buffer.is_empty());
    }

    #[test]
    fn handles_openai_chat_stream_delta_and_completed_response() {
        let mut recorder = MetricsRecorder::start();
        let mut answer = String::new();
        let mut completed_response = None;
        let mut saw_completed = false;

        handle_openai_chat_stream_event(
            SseFrame {
                event: Some("response.output_text.delta".to_owned()),
                data: r#"{"type":"response.output_text.delta","delta":"你"}"#.to_owned(),
            },
            &mut recorder,
            &mut answer,
            &mut completed_response,
            &mut saw_completed,
        )
        .unwrap();
        handle_openai_chat_stream_event(
            SseFrame {
                event: Some("response.completed".to_owned()),
                data: r#"{"type":"response.completed","response":{"output_text":"你好"}}"#
                    .to_owned(),
            },
            &mut recorder,
            &mut answer,
            &mut completed_response,
            &mut saw_completed,
        )
        .unwrap();

        assert_eq!(answer, "你");
        assert!(saw_completed);
        assert_eq!(
            completed_response.as_ref().and_then(|value| {
                value
                    .get("output_text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            }),
            Some("你好".to_owned())
        );
    }
}

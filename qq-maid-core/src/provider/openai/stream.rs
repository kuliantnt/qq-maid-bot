//! OpenAI Responses SSE 解析与事件处理。
//!
//! reqwest 返回的 chunk 不保证 UTF-8 或 frame 边界，且上游兼容层可能混用
//! `event:` 与 JSON `type`。这里统一接住原始 SSE，再把事件语义转换成聊天正文和
//! completed response，避免 Responses 主链路在多个位置重复解析。

use serde_json::Value;

use crate::{error::LlmError, util::metrics::MetricsRecorder};

use super::extract::extract_completed_response;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseFrame {
    pub(crate) event: Option<String>,
    pub(crate) data: String,
}

/// 从 SSE 字节缓冲区中取出一个完整 frame。
pub(crate) fn take_sse_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let (index, delimiter_len) = find_sse_delimiter(buffer)?;
    let frame = buffer[..index].to_vec();
    buffer.drain(..index + delimiter_len);
    Some(frame)
}

fn find_sse_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(a), Some(b)) if a < b => Some((a, 2)),
        (Some(_), Some(b)) => Some((b, 4)),
        (Some(a), None) => Some((a, 2)),
        (None, Some(b)) => Some((b, 4)),
        (None, None) => None,
    }
}

/// 解析单个 SSE frame。
pub(crate) fn parse_sse_frame(frame: &[u8]) -> Result<Option<SseFrame>, LlmError> {
    let text = std::str::from_utf8(frame)
        .map_err(|err| LlmError::provider(format!("invalid SSE stream UTF-8: {err}"), "sse"))?;
    let mut event = None;
    let mut data_lines = Vec::new();
    for raw_line in text.replace("\r\n", "\n").lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim_start().to_owned());
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_owned());
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let data = data_lines.join("\n");
    if data.trim() == "[DONE]" {
        return Ok(None);
    }
    Ok(Some(SseFrame { event, data }))
}

/// 处理 OpenAI Responses SSE 事件。
pub(crate) fn handle_openai_chat_stream_event(
    event: SseFrame,
    recorder: &mut MetricsRecorder,
    answer: &mut String,
    completed_response: &mut Option<Value>,
) -> Result<(), LlmError> {
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
            }
        }
        "response.completed" => {
            *completed_response = extract_completed_response(&value);
        }
        "response.failed" | "response.incomplete" | "error" => {
            let message = stream_error_message(&value)
                .unwrap_or_else(|| format!("OpenAI chat stream event {event_type}"));
            return Err(LlmError::provider(message, "sse"));
        }
        _ => {}
    }

    Ok(())
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

        handle_openai_chat_stream_event(
            SseFrame {
                event: Some("response.output_text.delta".to_owned()),
                data: r#"{"type":"response.output_text.delta","delta":"你"}"#.to_owned(),
            },
            &mut recorder,
            &mut answer,
            &mut completed_response,
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
        )
        .unwrap();

        assert_eq!(answer, "你");
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

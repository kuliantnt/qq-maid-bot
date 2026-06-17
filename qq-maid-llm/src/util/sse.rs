//! SSE frame 解析工具。
//!
//! reqwest 返回的 chunk 不保证 UTF-8 或 SSE frame 边界；这里先按字节寻找空行，
//! 再把完整 frame 转成文本，避免中文 delta 被拆包时出现乱码。

use crate::error::LlmError;

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
}

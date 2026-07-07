use super::event_stream::C2cStreamSender;
use crate::{
    api::{C2cStreamState, StreamSendResult},
    markdown::MarkdownPayload,
    respond::RespondResponse,
};

/// QQ 结束帧要求 Markdown content 非空；零宽空格满足非空校验，且不会把完整正文再次追加到已发送内容后。
pub(crate) const STREAM_FINAL_MARKER: &str = "\u{200B}";

pub(crate) fn completed_response_content(response: &RespondResponse) -> Option<&str> {
    response.markdown_content().or(response.text_content())
}

pub(crate) fn stream_final_packet_content(pending_delta: &str) -> &str {
    if pending_delta.is_empty() {
        STREAM_FINAL_MARKER
    } else {
        pending_delta
    }
}

pub(crate) fn response_from_incomplete_stream_text(content: &str) -> RespondResponse {
    RespondResponse {
        output: Some(qq_maid_core::service::AssistantOutput::markdown(
            content, content,
        )),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
    }
}

/// 发送流式消息分片到 QQ。
///
/// `reset=false` 时 QQ 会把本次 `markdown.content` 追加到现有流式消息后面，
/// 因此这里传入的 content 必须是尚未发送过的增量。
/// 首帧只有拿到 stream id 才能进入 Active；后续帧即使 QQ 返回新的消息 id，
/// 也必须保留首帧 id，避免最终帧的 id/index 序列被 QQ 判定为无效。
pub(crate) async fn send_stream_chunk<S: C2cStreamSender + ?Sized>(
    sender: &S,
    user_openid: &str,
    msg_id: Option<&str>,
    content: &str,
    stream_state: &mut C2cStreamState,
    stream_state_value: u8,
    reset: bool,
) -> StreamSendResult {
    let markdown = MarkdownPayload::new(content);
    let result = sender
        .send_stream_markdown(
            user_openid,
            msg_id,
            &markdown,
            stream_state,
            stream_state_value,
            Some(reset),
        )
        .await?;
    if stream_state.stream_id.is_none()
        && let Some(id) = result.as_deref().filter(|id| !id.trim().is_empty())
    {
        // QQ 流式续接 id 以首帧返回值为准；中间帧返回的是消息 id，不应覆盖，
        // 否则后续 index 会相对于错误 id 递增，最终帧可能报 stream.index 无效。
        stream_state.stream_id = Some(id.to_owned());
    }
    stream_state.index += 1;
    Ok(result)
}

/// 发送流式结束帧（state=10）。
///
/// 真实环境要求结束包的 Markdown 非空：正常收尾使用未发送尾部或不可见占位，
/// 并按参考实现继续携带同一个 stream id、连续 index 和 reset=false。
/// 首帧成功后不会回退成第二条普通消息，保持流式气泡的唯一发送所有权。
pub(crate) async fn send_stream_end<S: C2cStreamSender + ?Sized>(
    sender: &S,
    user_openid: &str,
    msg_id: Option<&str>,
    content: &str,
    stream_state: &mut C2cStreamState,
) -> Result<(), crate::api::ApiError> {
    // QQ 会校验 markdown.content 非空；调用方需避免在 reset=false 的结束帧重复提交已发送正文。
    let markdown = MarkdownPayload::new(content);
    let result = sender
        .send_stream_markdown(
            user_openid,
            msg_id,
            &markdown,
            stream_state,
            10,
            Some(false),
        )
        .await?;
    if stream_state.stream_id.is_none()
        && let Some(id) = result.as_deref().filter(|id| !id.trim().is_empty())
    {
        // 正常收尾前已经有首帧 id；这里只兼容“直接最终帧”或异常状态下的空 id。
        stream_state.stream_id = Some(id.to_owned());
    }
    stream_state.index += 1;
    Ok(())
}

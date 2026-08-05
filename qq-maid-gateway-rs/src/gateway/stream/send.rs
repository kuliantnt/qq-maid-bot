use super::event_stream::C2cStreamSender;
use crate::{
    api::{C2cStreamState, StreamSendResult},
    markdown::MarkdownPayload,
};
use qq_maid_common::output_part::AssistantOutput;
use qq_maid_core::service::CoreResponse;
use unicode_segmentation::UnicodeSegmentation;

/// QQ 结束帧要求 Markdown content 非空；零宽空格满足非空校验，且不会把完整正文再次追加到已发送内容后。
pub(crate) const STREAM_FINAL_MARKER: &str = "\u{200B}";

/// QQ C2C 流式接口的保守单片上限（按 Unicode scalar 数量计算）。
///
/// 当前仓库没有可复用的 QQ 流式硬上限常量；线上已观察到 2963 字符的单片被 QQ
/// 以 40054014 拒绝。取 2000 作为单一保守阈值，并与普通消息的 5000 字符软限制
/// 分开，避免普通消息配置变化后再次把超长缓冲直接送入流式接口。
pub(crate) const STREAM_CHUNK_CHAR_LIMIT: usize = 2000;

pub(crate) fn completed_response_content(response: &CoreResponse) -> Option<&str> {
    response.markdown_content().or(response.text_content())
}

pub(crate) fn response_from_incomplete_stream_text(content: &str) -> CoreResponse {
    CoreResponse {
        output: Some(AssistantOutput::markdown(content, content)),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
        visible_entity_snapshot: None,
        delivery_hint: None,
    }
}

/// 发送尚未提交的流式消息内容到 QQ。
///
/// `reset=false` 时 QQ 会把本次 `markdown.content` 追加到现有流式消息后面，
/// 因此这里传入的 content 必须是尚未发送过的增量。一个调用可能发送多个平台
/// 分片；每个分片成功后立即从 `content` 消费，失败分片及其后续内容会保留。
/// 首帧只有拿到 stream id 才能进入 Active；后续帧即使 QQ 返回新的消息 id，
/// 也必须保留首帧 id，避免最终帧的 id/index 序列被 QQ 判定为无效。
pub(crate) async fn send_stream_chunk<S: C2cStreamSender + ?Sized>(
    sender: &S,
    user_openid: &str,
    msg_id: Option<&str>,
    content: &mut String,
    stream_state: &mut C2cStreamState,
    stream_state_value: u8,
    reset: bool,
) -> StreamSendResult {
    let mut first_result = None;
    while !content.is_empty() {
        let chunk_end = next_stream_chunk_end(content);
        let chunk = content[..chunk_end].to_owned();
        let result = send_one_stream_chunk(
            sender,
            user_openid,
            msg_id,
            &chunk,
            stream_state,
            stream_state_value,
            reset,
        )
        .await?;
        let first_send = first_result.is_none();
        if first_send {
            first_result = Some(result.clone());
        }
        // send_one_stream_chunk 只有在 QQ 接受当前帧后才推进 index；此处随后消费
        // 同一段内容，确保失败帧和其后的内容不会被误标为已发送。
        content.drain(..chunk_end);

        // 首帧没有 stream id 时不能安全续发剩余分片；保留旧的 Pending fallback
        // 语义，等待 Completed 走普通回复。真实 QQ 首帧成功时应始终返回 stream id。
        if first_send && stream_state.stream_id.is_none() {
            break;
        }
    }
    Ok(first_result.unwrap_or(None))
}

/// 发送一个已经由上层切好的 QQ 流式帧。
async fn send_one_stream_chunk<S: C2cStreamSender + ?Sized>(
    sender: &S,
    user_openid: &str,
    msg_id: Option<&str>,
    content: &str,
    stream_state: &mut C2cStreamState,
    stream_state_value: u8,
    reset: bool,
) -> StreamSendResult {
    debug_assert!(content.chars().count() <= STREAM_CHUNK_CHAR_LIMIT);
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
    content: &mut String,
    stream_state: &mut C2cStreamState,
) -> Result<(), crate::api::ApiError> {
    // 结束帧只能提交一个合法大小的尾部；若待发送正文超过上限，先用 state=1
    // 逐片排空，再用不可见占位 state=10 收尾，避免 broken_active 原样重发超长正文。
    if content.chars().count() > STREAM_CHUNK_CHAR_LIMIT {
        send_stream_chunk(sender, user_openid, msg_id, content, stream_state, 1, false).await?;
    }
    let final_content = if content.is_empty() {
        STREAM_FINAL_MARKER
    } else {
        content.as_str()
    };
    // QQ 会校验 markdown.content 非空；正常收尾只提交尚未发送的尾部。
    let markdown = MarkdownPayload::new(final_content);
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
    content.clear();
    Ok(())
}

/// 返回不超过流式单片上限的 UTF-8 字节边界。
///
/// 以 Unicode grapheme cluster 为不可拆分单位，同时按 scalar 数量计数，避免在
/// 组合字符、ZWJ Emoji 或区域指示符之间切断可见字符。
fn next_stream_chunk_end(content: &str) -> usize {
    let mut scalar_count = 0_usize;
    let mut end = 0;
    for (start, grapheme) in content.grapheme_indices(true) {
        let grapheme_scalars = grapheme.chars().count();
        if scalar_count > 0
            && scalar_count.saturating_add(grapheme_scalars) > STREAM_CHUNK_CHAR_LIMIT
        {
            break;
        }
        scalar_count = scalar_count.saturating_add(grapheme_scalars);
        end = start + grapheme.len();
        if scalar_count >= STREAM_CHUNK_CHAR_LIMIT {
            break;
        }
    }
    end
}

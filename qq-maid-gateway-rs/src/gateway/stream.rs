//! C2C 流式发送状态机。
//!
//! 该模块只处理 Core 流事件到 QQ Markdown 流式消息的映射。普通 C2C fallback
//! 继续复用 `c2c.rs` 的发送路径，保证 Markdown、文本 fallback、图片开关和运行状态记录一致。

use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};

use tracing::{debug, warn};

use super::{
    c2c::send_c2c_respond_response_with_sender,
    event::C2cMessage,
    logging::{mask_identifier, mask_openid},
    outbound::{RuntimeRecordingSender, record_qq_send_result},
    ping::GatewayRuntimeStatus,
};
use crate::{
    api::{C2cStreamState, OutboundSender, QqApiClient, StreamSendResult},
    config::AppConfig,
    markdown::MarkdownPayload,
    respond::{RespondEvent, RespondResponse},
};

/// QQ C2C 流式发送的节流间隔（毫秒）。
///
/// 避免每个 LLM delta 都请求一次 QQ API，减少接口压力。
const STREAM_THROTTLE_MS: u64 = 500;

/// QQ 结束帧要求 Markdown content 非空；零宽空格满足非空校验，且不会把完整正文再次追加到已发送内容后。
const STREAM_FINAL_MARKER: &str = "\u{200B}";

type RespondEventFuture<'a> = Pin<Box<dyn Future<Output = Option<RespondEvent>> + Send + 'a>>;
type StreamSendFuture<'a> = Pin<Box<dyn Future<Output = StreamSendResult> + Send + 'a>>;

/// Core 流事件来源抽象，仅用于把 C2C 流式状态机与真实 Core channel 解耦，便于覆盖异常分支。
trait RespondEventStream: Send {
    fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a>;
}

impl RespondEventStream for qq_maid_core::service::CoreResponseStream {
    fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a> {
        Box::pin(async move { self.recv().await })
    }
}

/// C2C 流式发送抽象；普通消息能力复用 `OutboundSender`，确保 Pending fallback 走同一发送链路。
trait C2cStreamSender: OutboundSender {
    fn send_stream_markdown<'a>(
        &'a self,
        user_openid: &'a str,
        msg_id: Option<&'a str>,
        markdown: &'a MarkdownPayload,
        stream_state: &'a mut C2cStreamState,
        stream_state_value: u8,
        reset: Option<bool>,
    ) -> StreamSendFuture<'a>;
}

impl C2cStreamSender for RuntimeRecordingSender<'_> {
    fn send_stream_markdown<'a>(
        &'a self,
        user_openid: &'a str,
        msg_id: Option<&'a str>,
        markdown: &'a MarkdownPayload,
        stream_state: &'a mut C2cStreamState,
        stream_state_value: u8,
        reset: Option<bool>,
    ) -> StreamSendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_markdown_stream(
                    user_openid,
                    msg_id,
                    markdown,
                    stream_state,
                    stream_state_value,
                    reset,
                )
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }
}

#[derive(Debug)]
enum C2cStreamingPhase {
    Pending(C2cStreamState),
    Active(C2cStreamState),
    BrokenActive(C2cStreamState),
    Completed,
}

impl C2cStreamingPhase {
    fn name(&self) -> &'static str {
        match self {
            Self::Pending(_) => "pending",
            Self::Active(_) => "active",
            Self::BrokenActive(_) => "broken_active",
            Self::Completed => "completed",
        }
    }
}

/// QQ C2C 流式响应处理。
///
/// 在 QQ 官方机器人 C2C 私聊中，将 Core 流式响应接入 QQ 流式消息接口，
/// 让同一条消息在生成过程中持续更新。
///
/// # Fallback 行为
///
/// - `Pending` 首帧尚未成功时，Completed 后最多发送一次普通 C2C 回复。
/// - 一旦首帧成功进入 `Active`，本轮用户可见回复只归流式发送器所有；中间帧或最终帧失败只记录错误并保持 `BrokenActive`，禁止再发完整普通正文。
pub(super) async fn stream_respond_c2c(
    stream: qq_maid_core::service::CoreResponseStream,
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &C2cMessage,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let sender = RuntimeRecordingSender {
        inner: api,
        runtime,
    };
    stream_respond_c2c_with_sender(stream, &sender, message, config)
        .await
        .map(|_| ())
}

async fn stream_respond_c2c_with_sender<E, S>(
    mut stream: E,
    sender: &S,
    message: &C2cMessage,
    config: &AppConfig,
) -> anyhow::Result<C2cStreamingPhase>
where
    E: RespondEventStream,
    S: C2cStreamSender + ?Sized,
{
    let user_openid = &message.user_openid;
    let masked_user = mask_openid(user_openid);
    let reply_msg_id = &message.message_id;
    let masked_reply_msg_id = mask_identifier(reply_msg_id);
    let mut phase = C2cStreamingPhase::Pending(C2cStreamState::new());
    let mut accumulated = String::new();
    // QQ stream 的 reset=false 是“续接本次 Markdown content”，因此内容分片不能反复提交全文。
    // 完成包同样携带连续 index/reset=false，但只发送未发送尾部或不可见占位，避免把完整 final_text 再追加一遍。
    let mut pending_delta = String::new();
    let mut last_send_at = Instant::now();
    let mut stream_first_attempted = false;

    while let Some(event) = stream.recv_event().await {
        match event {
            RespondEvent::TextDelta(delta) => {
                if delta.is_empty() {
                    continue;
                }
                accumulated.push_str(&delta);
                pending_delta.push_str(&delta);

                match phase {
                    C2cStreamingPhase::Pending(mut stream_state) => {
                        if stream_first_attempted {
                            phase = C2cStreamingPhase::Pending(stream_state);
                            continue;
                        }
                        stream_first_attempted = true;
                        let index = stream_state.index;
                        let had_stream_id = stream_state.stream_id.is_some();
                        match send_stream_chunk(
                            sender,
                            user_openid,
                            Some(reply_msg_id),
                            &pending_delta,
                            &mut stream_state,
                            1,
                            false,
                        )
                        .await
                        {
                            Ok(Some(_)) => {
                                let content_chars = pending_delta.chars().count();
                                pending_delta.clear();
                                last_send_at = Instant::now();
                                debug!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "first_chunk",
                                    stream_state = "active",
                                    stream_state_value = 1_u8,
                                    reset = false,
                                    index,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = stream_state.stream_id.is_some(),
                                    content_chars,
                                    accumulated_chars = accumulated.chars().count(),
                                    "QQ stream first send succeeded"
                                );
                                phase = C2cStreamingPhase::Active(stream_state);
                            }
                            Ok(None) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "first_chunk",
                                    stream_state = "pending",
                                    stream_state_value = 1_u8,
                                    reset = false,
                                    index,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = false,
                                    content_chars = pending_delta.chars().count(),
                                    accumulated_chars = accumulated.chars().count(),
                                    "QQ stream first send returned no stream id; ordinary reply remains allowed on Completed"
                                );
                                phase = C2cStreamingPhase::Pending(stream_state);
                            }
                            Err(err) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "first_chunk",
                                    stream_state = "pending",
                                    stream_state_value = 1_u8,
                                    reset = false,
                                    index,
                                    has_stream_id_before_send = had_stream_id,
                                    content_chars = pending_delta.chars().count(),
                                    error = %err.log_summary(),
                                    accumulated_chars = accumulated.chars().count(),
                                    "QQ stream first send failed; ordinary reply remains allowed on Completed"
                                );
                                phase = C2cStreamingPhase::Pending(stream_state);
                            }
                        }
                    }
                    C2cStreamingPhase::Active(mut stream_state) => {
                        let elapsed = last_send_at.elapsed();
                        if elapsed >= Duration::from_millis(STREAM_THROTTLE_MS)
                            && !pending_delta.is_empty()
                        {
                            let chunk = pending_delta.clone();
                            let index = stream_state.index;
                            let had_stream_id = stream_state.stream_id.is_some();
                            match send_stream_chunk(
                                sender,
                                user_openid,
                                Some(reply_msg_id),
                                &chunk,
                                &mut stream_state,
                                1,
                                false,
                            )
                            .await
                            {
                                Ok(_) => {
                                    pending_delta.clear();
                                    last_send_at = Instant::now();
                                    debug!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        phase = "middle_chunk",
                                        stream_state = "active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        has_stream_id_after_send = stream_state.stream_id.is_some(),
                                        sent_len = accumulated.len(),
                                        chunk_chars = chunk.chars().count(),
                                        "QQ stream middle send succeeded"
                                    );
                                    phase = C2cStreamingPhase::Active(stream_state);
                                }
                                Err(err) => {
                                    warn!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        phase = "middle_chunk",
                                        stream_state = "broken_active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        content_chars = chunk.chars().count(),
                                        error = %err.log_summary(),
                                        accumulated_chars = accumulated.chars().count(),
                                        "QQ stream middle send failed; ordinary fallback is disabled after stream id was created"
                                    );
                                    phase = C2cStreamingPhase::BrokenActive(stream_state);
                                }
                            }
                        } else {
                            phase = C2cStreamingPhase::Active(stream_state);
                        }
                    }
                    C2cStreamingPhase::BrokenActive(stream_state) => {
                        phase = C2cStreamingPhase::BrokenActive(stream_state);
                    }
                    C2cStreamingPhase::Completed => {}
                }
            }
            RespondEvent::Completed(response) => {
                let final_content = completed_response_content(&response).unwrap_or(&accumulated);
                let final_chars = final_content.chars().count();
                match phase {
                    C2cStreamingPhase::Active(mut stream_state) => {
                        // Active 表示 QQ 已创建流式气泡，Completed 只能继续使用同一个 stream id。
                        if !pending_delta.is_empty() {
                            let chunk = pending_delta.clone();
                            let index = stream_state.index;
                            let had_stream_id = stream_state.stream_id.is_some();
                            match send_stream_chunk(
                                sender,
                                user_openid,
                                Some(reply_msg_id),
                                &chunk,
                                &mut stream_state,
                                1,
                                false,
                            )
                            .await
                            {
                                Ok(_) => {
                                    pending_delta.clear();
                                    debug!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        phase = "completed_flush",
                                        stream_state = "active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        has_stream_id_after_send = stream_state.stream_id.is_some(),
                                        content_chars = chunk.chars().count(),
                                        final_chars,
                                        "QQ stream pending delta flushed before final"
                                    );
                                }
                                Err(err) => {
                                    warn!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        phase = "completed_flush",
                                        stream_state = "broken_active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        content_chars = chunk.chars().count(),
                                        error = %err.log_summary(),
                                        final_chars,
                                        "QQ stream pending delta flush failed; ordinary fallback is disabled"
                                    );
                                    match send_stream_end(
                                        sender,
                                        user_openid,
                                        Some(reply_msg_id),
                                        stream_final_packet_content(&pending_delta),
                                        &mut stream_state,
                                    )
                                    .await
                                    {
                                        Ok(()) => {
                                            debug!(
                                                user = %masked_user,
                                                reply_msg_id = %masked_reply_msg_id,
                                                phase = "completed_flush_final_chunk",
                                                stream_state = C2cStreamingPhase::Completed.name(),
                                                stream_state_value = 10_u8,
                                                reset = false,
                                                index = stream_state.index,
                                                has_stream_id_before_send = stream_state.stream_id.is_some(),
                                                has_stream_id_after_send = stream_state.stream_id.is_some(),
                                                content_chars = final_chars,
                                                final_chars,
                                                "QQ stream end after pending delta flush failure succeeded"
                                            );
                                            return Ok(C2cStreamingPhase::Completed);
                                        }
                                        Err(end_err) => {
                                            warn!(
                                                user = %masked_user,
                                                reply_msg_id = %masked_reply_msg_id,
                                                phase = "completed_flush_final_chunk",
                                                stream_state = "broken_active",
                                                stream_state_value = 10_u8,
                                                reset = false,
                                                index = stream_state.index,
                                                has_stream_id_before_send = stream_state.stream_id.is_some(),
                                                content_chars = final_chars,
                                                error = %end_err.log_summary(),
                                                final_chars,
                                                "QQ stream end after pending delta flush failure failed"
                                            );
                                            return Ok(C2cStreamingPhase::BrokenActive(
                                                stream_state,
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        let had_stream_id = stream_state.stream_id.is_some();
                        match send_stream_end(
                            sender,
                            user_openid,
                            Some(reply_msg_id),
                            stream_final_packet_content(&pending_delta),
                            &mut stream_state,
                        )
                        .await
                        {
                            Ok(()) => {
                                debug!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "final_chunk",
                                    stream_state = C2cStreamingPhase::Completed.name(),
                                    stream_state_value = 10_u8,
                                    reset = false,
                                    index = stream_state.index,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = stream_state.stream_id.is_some(),
                                    content_chars = final_chars,
                                    final_chars,
                                    "QQ stream final send succeeded"
                                );
                                return Ok(C2cStreamingPhase::Completed);
                            }
                            Err(err) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "final_chunk",
                                    stream_state = "broken_active",
                                    stream_state_value = 10_u8,
                                    reset = false,
                                    index = stream_state.index,
                                    has_stream_id_before_send = had_stream_id,
                                    content_chars = final_chars,
                                    error = %err.log_summary(),
                                    final_chars,
                                    "QQ stream final send failed; ordinary fallback is disabled after stream id was created"
                                );
                                return Ok(C2cStreamingPhase::BrokenActive(stream_state));
                            }
                        }
                    }
                    C2cStreamingPhase::BrokenActive(mut stream_state) => {
                        let had_stream_id = stream_state.stream_id.is_some();
                        match send_stream_end(
                            sender,
                            user_openid,
                            Some(reply_msg_id),
                            stream_final_packet_content(&pending_delta),
                            &mut stream_state,
                        )
                        .await
                        {
                            Ok(()) => {
                                debug!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "broken_active_final_chunk",
                                    stream_state = C2cStreamingPhase::Completed.name(),
                                    stream_state_value = 10_u8,
                                    reset = false,
                                    index = stream_state.index,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = stream_state.stream_id.is_some(),
                                    content_chars = final_chars,
                                    final_chars,
                                    "QQ stream end after broken active succeeded"
                                );
                                return Ok(C2cStreamingPhase::Completed);
                            }
                            Err(err) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "broken_active_final_chunk",
                                    stream_state = "broken_active",
                                    stream_state_value = 10_u8,
                                    reset = false,
                                    index = stream_state.index,
                                    has_stream_id_before_send = had_stream_id,
                                    content_chars = final_chars,
                                    error = %err.log_summary(),
                                    final_chars,
                                    "QQ stream end after broken active failed; ordinary fallback is disabled"
                                );
                                return Ok(C2cStreamingPhase::BrokenActive(stream_state));
                            }
                        }
                    }
                    C2cStreamingPhase::Completed => return Ok(C2cStreamingPhase::Completed),
                    C2cStreamingPhase::Pending(_) => {
                        let stream_state_name = phase.name();
                        send_c2c_respond_response_with_sender(sender, message, &response, config)
                            .await
                            .inspect(|_| {
                                debug!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "ordinary_fallback_on_completed",
                                    stream_state = stream_state_name,
                                    final_chars,
                                    "QQ ordinary fallback send succeeded"
                                );
                            })
                            .inspect_err(|fallback_err| {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "ordinary_fallback_on_completed",
                                    stream_state = stream_state_name,
                                    error = %fallback_err,
                                    final_chars,
                                    "QQ ordinary fallback send failed"
                                );
                            })?;
                        return Ok(C2cStreamingPhase::Completed);
                    }
                }
            }
            RespondEvent::Failed(failure) => {
                warn!(
                    user = %masked_user,
                    reply_msg_id = %masked_reply_msg_id,
                    kind = ?failure.kind,
                    retryable = failure.retryable,
                    stream_state = phase.name(),
                    accumulated_chars = accumulated.chars().count(),
                    "core respond stream failed"
                );
                if let C2cStreamingPhase::Active(mut stream_state)
                | C2cStreamingPhase::BrokenActive(mut stream_state) = phase
                {
                    send_stream_end(
                        sender,
                        user_openid,
                        Some(reply_msg_id),
                        stream_final_packet_content(&pending_delta),
                        &mut stream_state,
                    )
                    .await
                    .inspect_err(|err| {
                        warn!(
                            user = %masked_user,
                            reply_msg_id = %masked_reply_msg_id,
                            phase = "failed_final_chunk",
                            stream_state_value = 10_u8,
                            reset = false,
                            index = stream_state.index,
                            has_stream_id = stream_state.stream_id.is_some(),
                            content_chars = accumulated.chars().count(),
                            error = %err.log_summary(),
                            accumulated_chars = accumulated.chars().count(),
                            "QQ stream finalization after core failure failed"
                        );
                    })?;
                }
                return Err(anyhow::anyhow!(
                    "core respond stream failed before Completed: kind={:?}, retryable={}",
                    failure.kind,
                    failure.retryable
                ));
            }
        }
    }

    let accumulated_chars = accumulated.chars().count();
    warn!(
        user = %masked_user,
        reply_msg_id = %masked_reply_msg_id,
        stream_state = phase.name(),
        accumulated_chars,
        "core respond stream closed before Completed"
    );
    match phase {
        C2cStreamingPhase::Active(mut stream_state)
        | C2cStreamingPhase::BrokenActive(mut stream_state) => {
            send_stream_end(
                sender,
                user_openid,
                Some(reply_msg_id),
                stream_final_packet_content(&pending_delta),
                &mut stream_state,
            )
            .await?;
        }
        C2cStreamingPhase::Pending(_) if !accumulated.is_empty() && !stream_first_attempted => {
            let response = response_from_incomplete_stream_text(&accumulated);
            send_c2c_respond_response_with_sender(sender, message, &response, config).await?;
        }
        C2cStreamingPhase::Pending(_) | C2cStreamingPhase::Completed => {}
    }
    Err(anyhow::anyhow!(
        "core respond stream closed before Completed; accumulated_chars={accumulated_chars}"
    ))
}

fn completed_response_content(response: &RespondResponse) -> Option<&str> {
    response.markdown.as_deref().or(response.text.as_deref())
}

fn stream_final_packet_content(pending_delta: &str) -> &str {
    if pending_delta.is_empty() {
        STREAM_FINAL_MARKER
    } else {
        pending_delta
    }
}

fn response_from_incomplete_stream_text(content: &str) -> RespondResponse {
    RespondResponse {
        text: Some(content.to_owned()),
        markdown: Some(content.to_owned()),
        handled: Some(true),
        session_id: None,
        command: None,
        diagnostics: None,
    }
}

/// 发送流式消息分片到 QQ。
///
/// `reset=false` 时 QQ 会把本次 `markdown.content` 追加到现有流式消息后面，
/// 因此这里传入的 content 必须是尚未发送过的增量。
/// 首帧只有拿到 stream id 才能进入 Active；后续帧即使 QQ 返回新的消息 id，
/// 也必须保留首帧 id，避免最终帧的 id/index 序列被 QQ 判定为无效。
async fn send_stream_chunk<S: C2cStreamSender + ?Sized>(
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
async fn send_stream_end<S: C2cStreamSender + ?Sized>(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::{ApiError, C2cReplyTarget, SendFuture},
        config::{
            DEFAULT_CONVERSATION_QUEUE_CAPACITY, DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS,
            DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS, DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS,
            DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES, DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS,
            DEFAULT_MESSAGE_AGGREGATION_QUIET_MS, GroupMessageMode, MessageAggregationConfig,
        },
        media::ImagePayload,
    };
    use qq_maid_core::service::{CoreFailureKind, CoreRespondFailure};
    use std::collections::VecDeque;

    #[derive(Debug)]
    struct FakeEventStream {
        events: VecDeque<(Duration, RespondEvent)>,
    }

    impl FakeEventStream {
        fn new(events: impl IntoIterator<Item = RespondEvent>) -> Self {
            Self {
                events: events
                    .into_iter()
                    .map(|event| (Duration::ZERO, event))
                    .collect(),
            }
        }

        fn with_delays(events: impl IntoIterator<Item = (Duration, RespondEvent)>) -> Self {
            Self {
                events: events.into_iter().collect(),
            }
        }
    }

    impl RespondEventStream for FakeEventStream {
        fn recv_event<'a>(&'a mut self) -> RespondEventFuture<'a> {
            Box::pin(async move {
                let (delay, event) = self.events.pop_front()?;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                Some(event)
            })
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum FakeCall {
        Stream {
            content: String,
            msg_id: Option<String>,
            stream_id: Option<String>,
            index: u32,
            stream_state_value: u8,
            reset: Option<bool>,
        },
        Markdown {
            content: String,
            msg_id: Option<String>,
        },
        Text {
            content: String,
            msg_id: Option<String>,
        },
        Image,
    }

    #[derive(Debug)]
    struct FakeStreamSender {
        stream_results: std::sync::Mutex<VecDeque<StreamSendResult>>,
        calls: std::sync::Mutex<Vec<FakeCall>>,
    }

    impl FakeStreamSender {
        fn new(stream_results: impl IntoIterator<Item = StreamSendResult>) -> Self {
            Self {
                stream_results: std::sync::Mutex::new(stream_results.into_iter().collect()),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<FakeCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl OutboundSender for FakeStreamSender {
        fn send_text<'a>(&'a self, target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(FakeCall::Text {
                    content: text.to_owned(),
                    msg_id: target.msg_id.clone(),
                });
                Ok(Some("ordinary-text-id".to_owned()))
            })
        }

        fn send_markdown<'a>(
            &'a self,
            target: &'a C2cReplyTarget,
            markdown: &'a MarkdownPayload,
        ) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(FakeCall::Markdown {
                    content: markdown.content.clone(),
                    msg_id: target.msg_id.clone(),
                });
                Ok(Some("ordinary-markdown-id".to_owned()))
            })
        }

        fn send_image<'a>(
            &'a self,
            _target: &'a C2cReplyTarget,
            _image: &'a ImagePayload,
        ) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(FakeCall::Image);
                Err(ApiError::Unsupported("image"))
            })
        }
    }

    impl C2cStreamSender for FakeStreamSender {
        fn send_stream_markdown<'a>(
            &'a self,
            _user_openid: &'a str,
            msg_id: Option<&'a str>,
            markdown: &'a MarkdownPayload,
            stream_state: &'a mut C2cStreamState,
            stream_state_value: u8,
            reset: Option<bool>,
        ) -> StreamSendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(FakeCall::Stream {
                    content: markdown.content.clone(),
                    msg_id: msg_id.map(str::to_owned),
                    stream_id: stream_state.stream_id.clone(),
                    index: stream_state.index,
                    stream_state_value,
                    reset,
                });
                self.stream_results
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or_else(|| Ok(None))
            })
        }
    }

    fn c2c_message() -> C2cMessage {
        C2cMessage {
            message_id: "msg-1".to_owned(),
            event_id: Some("event-1".to_owned()),
            source_message_ids: vec!["msg-1".to_owned()],
            source_event_ids: vec!["event-1".to_owned()],
            user_openid: "user-1".to_owned(),
            content: "晚上好".to_owned(),
            reply: None,
            timestamp: None,
            first_message_timestamp: None,
            last_message_timestamp: None,
            attachments: Vec::new(),
        }
    }

    fn respond_response(text: &str) -> RespondResponse {
        RespondResponse {
            text: Some(text.to_owned()),
            markdown: Some(text.to_owned()),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
        }
    }

    fn test_config() -> AppConfig {
        AppConfig {
            app_id: "app".to_owned(),
            app_secret: "secret".to_owned(),
            sandbox: false,
            api_base: "https://example.test".to_owned(),
            token_refresh_margin: Duration::from_secs(60),
            enable_markdown: true,
            enable_image: false,
            enable_group_messages: false,
            verbose_log: false,
            group_message_mode: GroupMessageMode::Mention,
            group_active_keywords: vec!["小女仆".to_owned()],
            conversation_queue_capacity: DEFAULT_CONVERSATION_QUEUE_CAPACITY,
            max_active_conversation_workers: DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS,
            conversation_worker_idle_timeout: Duration::from_secs(300),
            message_aggregation: MessageAggregationConfig {
                private_enabled: true,
                group_enabled: false,
                quiet: Duration::from_millis(DEFAULT_MESSAGE_AGGREGATION_QUIET_MS),
                max_wait: Duration::from_millis(DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS),
                max_messages: DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES,
                max_chars: DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS,
                max_active_keys: DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS,
            },
        }
    }

    #[tokio::test]
    async fn stream_first_send_error_falls_back_to_completed_response() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("晚上".to_owned()),
            RespondEvent::TextDelta("好".to_owned()),
            RespondEvent::Completed(respond_response("晚上好")),
        ]);
        let sender = FakeStreamSender::new([Err(ApiError::Unsupported("stream"))]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Markdown {
                    content: "晚上好".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_first_send_without_id_falls_back_to_completed_response() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("晚上".to_owned()),
            RespondEvent::TextDelta("好".to_owned()),
            RespondEvent::Completed(respond_response("晚上好")),
        ]);
        let sender = FakeStreamSender::new([Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Markdown {
                    content: "晚上好".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_single_content_packet_then_final_keeps_stream_id() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("测试成功".to_owned()),
            RespondEvent::Completed(respond_response("测试成功")),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);

        let phase = stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert!(matches!(phase, C2cStreamingPhase::Completed));
        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "测试成功".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: STREAM_FINAL_MARKER.to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 10,
                    reset: Some(false),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_active_path_reuses_id_and_increments_content_index() {
        let events = FakeEventStream::with_delays([
            (Duration::ZERO, RespondEvent::TextDelta("晚上".to_owned())),
            (
                Duration::from_millis(STREAM_THROTTLE_MS + 50),
                RespondEvent::TextDelta("好".to_owned()),
            ),
            (
                Duration::ZERO,
                RespondEvent::Completed(respond_response("晚上好")),
            ),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "好".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: STREAM_FINAL_MARKER.to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 2,
                    stream_state_value: 10,
                    reset: Some(false),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_empty_delta_does_not_consume_index() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta(String::new()),
            RespondEvent::TextDelta("好".to_owned()),
            RespondEvent::Completed(respond_response("好")),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "好".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: STREAM_FINAL_MARKER.to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 10,
                    reset: Some(false),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_middle_returned_id_does_not_replace_first_stream_id() {
        let events = FakeEventStream::with_delays([
            (Duration::ZERO, RespondEvent::TextDelta("晚".to_owned())),
            (
                Duration::from_millis(STREAM_THROTTLE_MS + 50),
                RespondEvent::TextDelta("上".to_owned()),
            ),
            (
                Duration::ZERO,
                RespondEvent::Completed(respond_response("晚上")),
            ),
        ]);
        let sender = FakeStreamSender::new([
            Ok(Some("stream-1".to_owned())),
            Ok(Some("middle-message-id".to_owned())),
            Ok(None),
        ]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: STREAM_FINAL_MARKER.to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 2,
                    stream_state_value: 10,
                    reset: Some(false),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_middle_chunks_coalesce_only_unsent_delta() {
        let events = FakeEventStream::with_delays([
            (Duration::ZERO, RespondEvent::TextDelta("晚".to_owned())),
            (Duration::ZERO, RespondEvent::TextDelta("上".to_owned())),
            (
                Duration::from_millis(STREAM_THROTTLE_MS + 50),
                RespondEvent::TextDelta("好".to_owned()),
            ),
            (
                Duration::ZERO,
                RespondEvent::Completed(respond_response("晚上好")),
            ),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "上好".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: STREAM_FINAL_MARKER.to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 2,
                    stream_state_value: 10,
                    reset: Some(false),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_final_failure_does_not_send_ordinary_fallback_after_active() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("晚上".to_owned()),
            RespondEvent::Completed(respond_response("晚上好")),
        ]);
        let sender = FakeStreamSender::new([
            Ok(Some("stream-1".to_owned())),
            Err(ApiError::Unsupported("stream")),
        ]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: STREAM_FINAL_MARKER.to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 10,
                    reset: Some(false),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_completed_flushes_pending_delta_before_final() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("晚".to_owned()),
            RespondEvent::TextDelta("上".to_owned()),
            RespondEvent::Completed(respond_response("晚上")),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: STREAM_FINAL_MARKER.to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 2,
                    stream_state_value: 10,
                    reset: Some(false),
                },
            ]
        );
    }

    #[tokio::test]
    async fn stream_completed_without_delta_uses_ordinary_reply_path() {
        let events = FakeEventStream::new([RespondEvent::Completed(respond_response("晚上好"))]);
        let sender = FakeStreamSender::new([]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![FakeCall::Markdown {
                content: "晚上好".to_owned(),
                msg_id: Some("msg-1".to_owned()),
            }]
        );
    }

    #[tokio::test]
    async fn stream_completed_sends_single_final_chunk() {
        let events = FakeEventStream::new([
            RespondEvent::TextDelta("好".to_owned()),
            RespondEvent::Completed(respond_response("好")),
            RespondEvent::Completed(respond_response("好")),
        ]);
        let sender = FakeStreamSender::new([Ok(Some("stream-1".to_owned())), Ok(None)]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        let final_count = sender
            .calls()
            .into_iter()
            .filter(|call| {
                matches!(
                    call,
                    FakeCall::Stream {
                        stream_state_value: 10,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(final_count, 1);
    }

    #[tokio::test]
    async fn stream_chunk_failure_does_not_advance_next_index() {
        let sender = FakeStreamSender::new([Err(ApiError::Unsupported("stream"))]);
        let mut stream_state = C2cStreamState::new();
        stream_state.stream_id = Some("stream-1".to_owned());
        stream_state.index = 1;

        let result = send_stream_chunk(
            &sender,
            "user-1",
            Some("msg-1"),
            "失败分片",
            &mut stream_state,
            1,
            false,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(stream_state.index, 1);
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Stream {
                content: "失败分片".to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 1,
                stream_state_value: 1,
                reset: Some(false),
            }]
        );
    }

    #[tokio::test]
    async fn stream_final_success_commits_next_index() {
        let sender = FakeStreamSender::new([Ok(None)]);
        let mut stream_state = C2cStreamState::new();
        stream_state.stream_id = Some("stream-1".to_owned());
        stream_state.index = 2;

        send_stream_end(
            &sender,
            "user-1",
            Some("msg-1"),
            "最终正文",
            &mut stream_state,
        )
        .await
        .unwrap();

        assert_eq!(stream_state.index, 3);
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Stream {
                content: "最终正文".to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: Some("stream-1".to_owned()),
                index: 2,
                stream_state_value: 10,
                reset: Some(false),
            }]
        );
    }

    #[tokio::test]
    async fn stream_closed_before_completed_is_not_silent_success() {
        let events = FakeEventStream::new([RespondEvent::TextDelta("晚上".to_owned())]);
        let sender = FakeStreamSender::new([Err(ApiError::Unsupported("stream"))]);

        let result =
            stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config()).await;

        assert!(result.is_err());
        assert_eq!(
            sender.calls(),
            vec![FakeCall::Stream {
                content: "晚上".to_owned(),
                msg_id: Some("msg-1".to_owned()),
                stream_id: None,
                index: 0,
                stream_state_value: 1,
                reset: Some(false),
            }]
        );
    }

    #[tokio::test]
    async fn stream_middle_failure_does_not_send_ordinary_fallback_on_completed() {
        let events = FakeEventStream::with_delays([
            (Duration::ZERO, RespondEvent::TextDelta("晚".to_owned())),
            (
                Duration::from_millis(STREAM_THROTTLE_MS + 50),
                RespondEvent::TextDelta("上".to_owned()),
            ),
            (
                Duration::ZERO,
                RespondEvent::Completed(respond_response("晚上")),
            ),
        ]);
        let sender = FakeStreamSender::new([
            Ok(Some("stream-1".to_owned())),
            Err(ApiError::Unsupported("stream")),
            Ok(None),
        ]);

        stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config())
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec![
                FakeCall::Stream {
                    content: "晚".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: None,
                    index: 0,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 1,
                    reset: Some(false),
                },
                FakeCall::Stream {
                    content: "上".to_owned(),
                    msg_id: Some("msg-1".to_owned()),
                    stream_id: Some("stream-1".to_owned()),
                    index: 1,
                    stream_state_value: 10,
                    reset: Some(false),
                },
            ]
        );
    }

    #[tokio::test]
    async fn core_failed_event_is_returned_as_observable_error() {
        let events = FakeEventStream::new([RespondEvent::Failed(CoreRespondFailure {
            kind: CoreFailureKind::Internal,
            message: "boom".to_owned(),
            retryable: false,
        })]);
        let sender = FakeStreamSender::new([]);

        let result =
            stream_respond_c2c_with_sender(events, &sender, &c2c_message(), &test_config()).await;

        assert!(result.is_err());
        assert!(sender.calls().is_empty());
    }
}

use std::time::{Duration, Instant};

use tracing::{debug, info, trace, warn};

use super::{
    event_stream::{C2cStreamSender, RespondEventStream, failure_stop_reason},
    send::{
        completed_response_content, response_from_incomplete_stream_text, send_stream_chunk,
        send_stream_end, stream_final_packet_content,
    },
    types::C2cStreamingPhase,
};
use crate::{
    api::{C2cReplyTarget, C2cStreamState, QqApiClient},
    config::AppConfig,
    gateway::{
        event::C2cMessage,
        logging::{mask_identifier, mask_openid},
        outbound::{ReplyCapability, RuntimeRecordingSender},
        ping::GatewayRuntimeStatus,
        qq_official::c2c::{
            record_c2c_bot_outbound_refs, send_c2c_respond_response_with_sender,
            send_local_c2c_failure_text,
        },
        ref_index::SharedRefIndex,
        typing::{C2cTypingStatusGuard, TypingStopReason},
    },
    render::{OutboundMessage, render_respond_response_parts_for_profile},
};
use qq_maid_core::service::{
    CoreOutputPolicy, CoreResponse, CoreResponseEvent, CoreResponseStatus,
};

/// QQ C2C 流式发送的节流间隔（毫秒）。
///
/// 避免每个 LLM delta 都请求一次 QQ API，减少接口压力。
pub(crate) const STREAM_THROTTLE_MS: u64 = 500;

async fn send_completed_media_after_stream<S: C2cStreamSender + ?Sized>(
    sender: &S,
    message: &C2cMessage,
    response: &CoreResponse,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let capability = ReplyCapability::qq_official_c2c(config);
    let media = render_respond_response_parts_for_profile(response, &capability.render)
        .into_iter()
        .filter(|outbound| {
            matches!(
                outbound,
                OutboundMessage::Image { .. } | OutboundMessage::ImagePlaceholder { .. }
            )
        })
        .collect::<Vec<_>>();
    if media.is_empty() {
        return Ok(());
    }

    let target = crate::gateway::outbound::ReplyTarget::qq_c2c(
        message.user_openid.clone(),
        Some(message.message_id.clone()),
    )
    .to_qq_c2c_target()
    .expect("QQ C2C reply target should adapt to QQ API target");
    let limits = crate::message_chunk::ChunkLimits::new(
        config.markdown_chunk_soft_limit,
        config.text_chunk_soft_limit,
    );
    for outbound in &media {
        crate::message_chunk::send_c2c_outbound_chunked(
            sender,
            &target,
            outbound,
            &limits,
            |_, _| {},
        )
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    }
    Ok(())
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
pub(crate) async fn stream_respond_c2c(
    stream: qq_maid_core::service::CoreResponseStream,
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &C2cMessage,
    config: &AppConfig,
    typing: Option<C2cTypingStatusGuard>,
    ref_index: &SharedRefIndex,
) -> anyhow::Result<()> {
    let sender = RuntimeRecordingSender {
        inner: api,
        runtime,
    };
    stream_respond_c2c_with_sender_and_typing_and_ref_index(
        stream,
        &sender,
        message,
        config,
        typing,
        Some(ref_index),
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
pub(crate) async fn stream_respond_c2c_with_sender<E, S>(
    stream: E,
    sender: &S,
    message: &C2cMessage,
    config: &AppConfig,
) -> anyhow::Result<C2cStreamingPhase>
where
    E: RespondEventStream,
    S: C2cStreamSender + ?Sized,
{
    stream_respond_c2c_with_sender_and_typing(stream, sender, message, config, None).await
}

#[cfg(test)]
pub(crate) async fn stream_respond_c2c_with_sender_and_typing<E, S>(
    stream: E,
    sender: &S,
    message: &C2cMessage,
    config: &AppConfig,
    typing: Option<C2cTypingStatusGuard>,
) -> anyhow::Result<C2cStreamingPhase>
where
    E: RespondEventStream,
    S: C2cStreamSender + ?Sized,
{
    stream_respond_c2c_with_sender_and_typing_and_ref_index(
        stream, sender, message, config, typing, None,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn stream_respond_c2c_with_sender_and_ref_index<E, S>(
    stream: E,
    sender: &S,
    message: &C2cMessage,
    config: &AppConfig,
    ref_index: &SharedRefIndex,
) -> anyhow::Result<C2cStreamingPhase>
where
    E: RespondEventStream,
    S: C2cStreamSender + ?Sized,
{
    stream_respond_c2c_with_sender_and_typing_and_ref_index(
        stream,
        sender,
        message,
        config,
        None,
        Some(ref_index),
    )
    .await
}

async fn stream_respond_c2c_with_sender_and_typing_and_ref_index<E, S>(
    mut stream: E,
    sender: &S,
    message: &C2cMessage,
    config: &AppConfig,
    mut typing: Option<C2cTypingStatusGuard>,
    ref_index: Option<&SharedRefIndex>,
) -> anyhow::Result<C2cStreamingPhase>
where
    E: RespondEventStream,
    S: C2cStreamSender + ?Sized,
{
    let user_openid = &message.user_openid;
    let masked_user = mask_openid(user_openid);
    let reply_msg_id = &message.message_id;
    let masked_reply_msg_id = mask_identifier(reply_msg_id);
    let started_at = Instant::now();
    let output_policy = stream.output_policy();
    let mut phase = C2cStreamingPhase::Pending(C2cStreamState::new());
    let mut accumulated = String::new();
    // QQ stream 的 reset=false 是“续接本次 Markdown content”，因此内容分片不能反复提交全文。
    // 完成包同样携带连续 index/reset=false，但只发送未发送尾部或不可见占位，避免把完整 final_text 再追加一遍。
    let mut pending_delta = String::new();
    let mut last_send_at = Instant::now();
    let mut stream_first_attempted = false;
    let mut text_delta_count = 0_usize;
    let mut status_event_count = 0_usize;
    let mut progress_status_send_attempted = false;

    while let Some(event) = stream.recv_event().await {
        match event {
            CoreResponseEvent::Status(status) => {
                status_event_count += 1;
                trace!(
                    user = %masked_user,
                    reply_msg_id = %masked_reply_msg_id,
                    status_kind = status.kind.as_str(),
                    response_delivery_mode = "progress_status",
                    stream_state = phase.name(),
                    status_chars = status.text.chars().count(),
                    status_event_count,
                    "core progress status event recorded by C2C stream state machine"
                );
                if should_send_progress_status(
                    config.c2c_visible_progress_status_enabled,
                    output_policy,
                    progress_status_send_attempted,
                ) {
                    progress_status_send_attempted = true;
                    send_progress_status(
                        sender,
                        message,
                        &status,
                        &masked_user,
                        &masked_reply_msg_id,
                        phase.name(),
                    )
                    .await;
                }
            }
            CoreResponseEvent::TextDelta(delta) => {
                if delta.is_empty() {
                    continue;
                }
                text_delta_count += 1;
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
                                if let Some(typing) = typing.as_mut() {
                                    typing.stop(TypingStopReason::FirstFrame);
                                }
                                let content_chars = pending_delta.chars().count();
                                pending_delta.clear();
                                last_send_at = Instant::now();
                                trace!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "first_chunk",
                                    response_delivery_mode = output_policy.as_str(),
                                    stream_state = "active",
                                    stream_state_value = 1_u8,
                                    reset = false,
                                    index,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = stream_state.stream_id.is_some(),
                                    text_delta_count,
                                    stream_entered_active = true,
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
                                    response_delivery_mode = "stream_fallback",
                                    stream_state = "pending",
                                    stream_state_value = 1_u8,
                                    reset = false,
                                    index,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = false,
                                    text_delta_count,
                                    stream_entered_active = false,
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
                                    response_delivery_mode = "stream_fallback",
                                    stream_state = "pending",
                                    stream_state_value = 1_u8,
                                    reset = false,
                                    index,
                                    has_stream_id_before_send = had_stream_id,
                                    text_delta_count,
                                    stream_entered_active = false,
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
                                    trace!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        phase = "middle_chunk",
                                        response_delivery_mode = output_policy.as_str(),
                                        stream_state = "active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        has_stream_id_after_send = stream_state.stream_id.is_some(),
                                        text_delta_count,
                                        stream_entered_active = true,
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
                                        response_delivery_mode = output_policy.as_str(),
                                        stream_state = "broken_active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        text_delta_count,
                                        stream_entered_active = true,
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
            CoreResponseEvent::Completed(response) => {
                if let Some(typing) = typing.as_mut() {
                    typing.stop(TypingStopReason::FinalReply);
                }
                let final_content = completed_response_content(&response).unwrap_or(&accumulated);
                let final_chars = final_content.chars().count();
                match phase {
                    C2cStreamingPhase::Active(mut stream_state) => {
                        // Active stream 目前只拿到 QQ 用于续接流式消息的 stream_id/index。
                        // 这些字段尚未确认会作为后续 quote 事件中的 ref_msg_idx/msg_idx 回传，
                        // 不能等同于可被引用索引使用的 bot outbound ref key。
                        //
                        // 因此 active stream 路径暂不写 ref_index：
                        // - 不把 stream_id 当作 ref_idx/msg_idx；
                        // - 不把用户入站 msg_id 当作 bot outbound id；
                        // - 避免制造看似可用、实际会错配的引用索引。
                        //
                        // 若后续通过真实 QQ 事件确认 DONE 回包里的某个字段会被 quote 回传，
                        // 再在该字段处补充 ref_index 回填和回归测试。
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
                                    trace!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        phase = "completed_flush",
                                        stream_state = "active",
                                        stream_state_value = 1_u8,
                                        reset = false,
                                        index,
                                        has_stream_id_before_send = had_stream_id,
                                        has_stream_id_after_send = stream_state.stream_id.is_some(),
                                        text_delta_count,
                                        qq_stream_send_count = stream_state.index,
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
                                        text_delta_count,
                                        qq_stream_send_count = stream_state.index,
                                        accumulated_chars = accumulated.chars().count(),
                                        elapsed_ms = started_at.elapsed().as_millis(),
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
                                            trace!(
                                                user = %masked_user,
                                                reply_msg_id = %masked_reply_msg_id,
                                                phase = "completed_flush_final_chunk",
                                                response_delivery_mode = output_policy.as_str(),
                                                stream_state = C2cStreamingPhase::Completed.name(),
                                                stream_state_value = 10_u8,
                                                reset = false,
                                                index = stream_state.index,
                                                has_stream_id_before_send = stream_state.stream_id.is_some(),
                                                has_stream_id_after_send = stream_state.stream_id.is_some(),
                                                text_delta_count,
                                                status_event_count,
                                                stream_entered_active = true,
                                                final_send_exit = "qq_stream_final",
                                                qq_stream_send_count = stream_state.index,
                                                accumulated_chars = accumulated.chars().count(),
                                                elapsed_ms = started_at.elapsed().as_millis(),
                                                content_chars = final_chars,
                                                final_chars,
                                                "QQ stream end after pending delta flush failure succeeded"
                                            );
                                            info!(
                                                user = %masked_user,
                                                reply_msg_id = %masked_reply_msg_id,
                                                response_delivery_mode = output_policy.as_str(),
                                                final_send_exit = "qq_stream_final",
                                                stream_state = C2cStreamingPhase::Completed.name(),
                                                text_delta_count,
                                                status_event_count,
                                                accumulated_chars = accumulated.chars().count(),
                                                final_chars,
                                                qq_stream_send_count = stream_state.index,
                                                elapsed_ms = started_at.elapsed().as_millis(),
                                                stream_entered_active = true,
                                                fallback_used = false,
                                                "QQ C2C stream response completed"
                                            );
                                            send_completed_media_after_stream(
                                                sender, message, &response, config,
                                            )
                                            .await?;
                                            return Ok(C2cStreamingPhase::Completed);
                                        }
                                        Err(end_err) => {
                                            warn!(
                                                user = %masked_user,
                                                reply_msg_id = %masked_reply_msg_id,
                                                phase = "completed_flush_final_chunk",
                                                response_delivery_mode = output_policy.as_str(),
                                                stream_state = "broken_active",
                                                stream_state_value = 10_u8,
                                                reset = false,
                                                index = stream_state.index,
                                                has_stream_id_before_send = stream_state.stream_id.is_some(),
                                                text_delta_count,
                                                status_event_count,
                                                stream_entered_active = true,
                                                final_send_exit = "qq_stream_final",
                                                qq_stream_send_count = stream_state.index,
                                                accumulated_chars = accumulated.chars().count(),
                                                elapsed_ms = started_at.elapsed().as_millis(),
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
                                info!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    response_delivery_mode = output_policy.as_str(),
                                    phase = "final_chunk",
                                    stream_state = C2cStreamingPhase::Completed.name(),
                                    stream_state_value = 10_u8,
                                    reset = false,
                                    index = stream_state.index,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = stream_state.stream_id.is_some(),
                                    text_delta_count,
                                    status_event_count,
                                    stream_entered_active = true,
                                    final_send_exit = "qq_stream_final",
                                    qq_stream_send_count = stream_state.index,
                                    accumulated_chars = accumulated.chars().count(),
                                    elapsed_ms = started_at.elapsed().as_millis(),
                                    fallback_used = false,
                                    content_chars = final_chars,
                                    final_chars,
                                    "QQ C2C stream response completed"
                                );
                                send_completed_media_after_stream(
                                    sender, message, &response, config,
                                )
                                .await?;
                                return Ok(C2cStreamingPhase::Completed);
                            }
                            Err(err) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "final_chunk",
                                    response_delivery_mode = output_policy.as_str(),
                                    stream_state = "broken_active",
                                    stream_state_value = 10_u8,
                                    reset = false,
                                    index = stream_state.index,
                                    has_stream_id_before_send = had_stream_id,
                                    text_delta_count,
                                    status_event_count,
                                    stream_entered_active = true,
                                    final_send_exit = "qq_stream_final",
                                    qq_stream_send_count = stream_state.index,
                                    accumulated_chars = accumulated.chars().count(),
                                    elapsed_ms = started_at.elapsed().as_millis(),
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
                                info!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    response_delivery_mode = output_policy.as_str(),
                                    phase = "broken_active_final_chunk",
                                    stream_state = C2cStreamingPhase::Completed.name(),
                                    stream_state_value = 10_u8,
                                    reset = false,
                                    index = stream_state.index,
                                    has_stream_id_before_send = had_stream_id,
                                    has_stream_id_after_send = stream_state.stream_id.is_some(),
                                    text_delta_count,
                                    status_event_count,
                                    stream_entered_active = true,
                                    final_send_exit = "qq_stream_final",
                                    qq_stream_send_count = stream_state.index,
                                    accumulated_chars = accumulated.chars().count(),
                                    elapsed_ms = started_at.elapsed().as_millis(),
                                    fallback_used = false,
                                    content_chars = final_chars,
                                    final_chars,
                                    "QQ C2C stream response completed after broken active"
                                );
                                send_completed_media_after_stream(
                                    sender, message, &response, config,
                                )
                                .await?;
                                return Ok(C2cStreamingPhase::Completed);
                            }
                            Err(err) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    phase = "broken_active_final_chunk",
                                    response_delivery_mode = output_policy.as_str(),
                                    stream_state = "broken_active",
                                    stream_state_value = 10_u8,
                                    reset = false,
                                    index = stream_state.index,
                                    has_stream_id_before_send = had_stream_id,
                                    text_delta_count,
                                    status_event_count,
                                    stream_entered_active = true,
                                    final_send_exit = "qq_stream_final",
                                    qq_stream_send_count = stream_state.index,
                                    accumulated_chars = accumulated.chars().count(),
                                    elapsed_ms = started_at.elapsed().as_millis(),
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
                        let response_delivery_mode = if stream_first_attempted {
                            "stream_fallback"
                        } else {
                            output_policy.as_str()
                        };
                        let capability = ReplyCapability::qq_official_c2c(config);
                        let (sent_ids, fallback_text) = send_c2c_respond_response_with_sender(
                            sender,
                            message,
                            &response,
                            config,
                            &capability,
                        )
                        .await
                        .inspect(|_| {
                            info!(
                                user = %masked_user,
                                reply_msg_id = %masked_reply_msg_id,
                                phase = "ordinary_fallback_on_completed",
                                response_delivery_mode,
                                stream_state = stream_state_name,
                                text_delta_count,
                                status_event_count,
                                stream_entered_active = false,
                                final_send_exit = "ordinary_reply",
                                qq_stream_send_count = 0_u32,
                                accumulated_chars = accumulated.chars().count(),
                                elapsed_ms = started_at.elapsed().as_millis(),
                                fallback_used = stream_first_attempted,
                                final_chars,
                                "QQ C2C stream response completed"
                            );
                        })
                        .inspect_err(|fallback_err| {
                            warn!(
                                user = %masked_user,
                                reply_msg_id = %masked_reply_msg_id,
                                phase = "ordinary_fallback_on_completed",
                                response_delivery_mode,
                                stream_state = stream_state_name,
                                error = %fallback_err,
                                text_delta_count,
                                status_event_count,
                                stream_entered_active = false,
                                final_send_exit = "ordinary_reply",
                                final_chars,
                                "QQ ordinary fallback send failed"
                            );
                        })?;
                        if let Some(ref_index) = ref_index {
                            record_c2c_bot_outbound_refs(
                                ref_index,
                                message,
                                config,
                                sent_ids,
                                &fallback_text,
                                response.visible_entity_snapshot.clone(),
                            );
                        }
                        return Ok(C2cStreamingPhase::Completed);
                    }
                }
            }
            CoreResponseEvent::Failed(failure) => {
                if let Some(typing) = typing.as_mut() {
                    typing.stop(failure_stop_reason(&failure));
                }
                warn!(
                    user = %masked_user,
                    reply_msg_id = %masked_reply_msg_id,
                    kind = ?failure.kind,
                    retryable = failure.retryable,
                    stream_state = phase.name(),
                    text_delta_count,
                    status_event_count,
                    accumulated_chars = accumulated.chars().count(),
                    "core respond stream failed"
                );
                match phase {
                    C2cStreamingPhase::Pending(_) => {
                        // 进度提示是独立普通消息，不代表 QQ stream 已取得本轮回复所有权。
                        // Core 在首帧前失败时必须把安全失败文案回给用户，避免会话永久停在
                        // “正在处理”；若首帧已成功则继续由 Active 分支独占收尾。
                        let sent_ids =
                            send_local_c2c_failure_text(sender, message, &failure.message).await?;
                        if let Some(ref_index) = ref_index {
                            record_c2c_bot_outbound_refs(
                                ref_index,
                                message,
                                config,
                                [sent_ids],
                                &failure.message,
                                None,
                            );
                        }
                        info!(
                            user = %masked_user,
                            reply_msg_id = %masked_reply_msg_id,
                            kind = ?failure.kind,
                            retryable = failure.retryable,
                            response_delivery_mode = "ordinary_failure_reply",
                            stream_state = "pending",
                            text_delta_count,
                            status_event_count,
                            accumulated_chars = accumulated.chars().count(),
                            elapsed_ms = started_at.elapsed().as_millis(),
                            "QQ C2C pending stream failure sent to user"
                        );
                        return Ok(C2cStreamingPhase::Completed);
                    }
                    C2cStreamingPhase::Active(mut stream_state)
                    | C2cStreamingPhase::BrokenActive(mut stream_state) => {
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
                    C2cStreamingPhase::Completed => return Ok(C2cStreamingPhase::Completed),
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
        text_delta_count,
        status_event_count,
        accumulated_chars,
        "core respond stream closed before Completed"
    );
    if let Some(typing) = typing.as_mut() {
        typing.stop(TypingStopReason::Cancelled);
    }
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
            let capability = ReplyCapability::qq_official_c2c(config);
            let (sent_ids, fallback_text) = send_c2c_respond_response_with_sender(
                sender,
                message,
                &response,
                config,
                &capability,
            )
            .await?;
            if let Some(ref_index) = ref_index {
                record_c2c_bot_outbound_refs(
                    ref_index,
                    message,
                    config,
                    sent_ids,
                    &fallback_text,
                    None,
                );
            }
        }
        C2cStreamingPhase::Pending(_) | C2cStreamingPhase::Completed => {}
    }
    Err(anyhow::anyhow!(
        "core respond stream closed before Completed; accumulated_chars={accumulated_chars}"
    ))
}

fn should_send_progress_status(enabled: bool, policy: CoreOutputPolicy, attempted: bool) -> bool {
    enabled
        && !attempted
        && matches!(
            policy,
            CoreOutputPolicy::ProgressThenComplete | CoreOutputPolicy::ProgressThenStream
        )
}

async fn send_progress_status<S: C2cStreamSender + ?Sized>(
    sender: &S,
    message: &C2cMessage,
    status: &CoreResponseStatus,
    masked_user: &str,
    masked_reply_msg_id: &str,
    stream_state: &str,
) {
    let target = C2cReplyTarget {
        user_openid: message.user_openid.clone(),
        msg_id: Some(message.message_id.clone()),
    };
    // Status 是系统短文案，独立普通文本发送；失败只记日志，不影响 Tool Loop 和最终回复。
    match sender.send_text(&target, &status.text).await {
        Ok(_) => {
            debug!(
                user = %masked_user,
                reply_msg_id = %masked_reply_msg_id,
                status_kind = status.kind.as_str(),
                response_delivery_mode = "progress_status",
                stream_state,
                status_chars = status.text.chars().count(),
                "C2C progress status sent"
            );
        }
        Err(err) => {
            warn!(
                user = %masked_user,
                reply_msg_id = %masked_reply_msg_id,
                status_kind = status.kind.as_str(),
                response_delivery_mode = "progress_status",
                stream_state,
                error = %err.log_summary(),
                "C2C progress status send failed; final response will continue"
            );
        }
    }
}

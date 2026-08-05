use std::time::{Duration, Instant};

use tokio::time::timeout;
use tracing::{info, trace, warn};

use super::{
    event_stream::{C2cStreamSender, RespondEventStream, failure_stop_reason},
    progress::{
        STREAM_THROTTLE_MS, send_progress_status, should_send_progress_status, stream_flush_wait,
    },
    send::{
        CumulativeTextAction, STREAM_INPUT_DONE, STREAM_INPUT_GENERATING,
        completed_response_content, reconcile_cumulative_text,
        response_from_incomplete_stream_text,
    },
    types::{C2cStreamState, C2cStreamingPhase},
};
use crate::{
    api::{ApiError, QqApiClient, SendMessageIds},
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
use qq_maid_core::service::{CoreResponse, CoreResponseEvent};

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

/// 发送一轮官方 replace 更新；只有 HTTP 成功并拿到官方消息 ID 后才接受正文。
async fn send_stream_update<S: C2cStreamSender + ?Sized>(
    sender: &S,
    user_openid: &str,
    msg_id: &str,
    state: &mut C2cStreamState,
    full_text: &str,
) -> Result<(), ApiError> {
    state.begin_opening();
    let result = sender
        .send_stream_markdown(
            user_openid,
            Some(msg_id),
            full_text,
            state,
            STREAM_INPUT_GENERATING,
        )
        .await;
    match result {
        Ok(response) if state.transport.stream_msg_id.is_some() => {
            state.accept_update(full_text, response);
            Ok(())
        }
        Ok(_) => {
            state.mark_failed();
            Err(ApiError::InvalidStreamResponse("missing stream session id"))
        }
        Err(error) => {
            state.mark_failed();
            Err(error)
        }
    }
}

/// 完成当前 StreamSession；同一会话最多发送一次 `input_state=10`。
async fn complete_stream<S: C2cStreamSender + ?Sized>(
    sender: &S,
    user_openid: &str,
    msg_id: &str,
    state: &mut C2cStreamState,
) -> Result<Option<SendMessageIds>, ApiError> {
    if !state.has_accepted_content || state.last_accepted_full.is_empty() {
        return Ok(None);
    }
    if !state.mark_completion_attempted() {
        return Ok(state.final_ids());
    }

    let final_text = state.last_accepted_full.clone();
    let result = sender
        .send_stream_markdown(
            user_openid,
            Some(msg_id),
            &final_text,
            state,
            STREAM_INPUT_DONE,
        )
        .await;
    match result {
        Ok(response) => {
            state.accept_completion(response);
            Ok(state.final_ids())
        }
        Err(error) => {
            state.mark_failed();
            Err(error)
        }
    }
}

fn record_stream_ref_index(
    ref_index: Option<&SharedRefIndex>,
    message: &C2cMessage,
    config: &AppConfig,
    state: &C2cStreamState,
    visible_entity_snapshot: Option<qq_maid_core::service::VisibleEntitySnapshot>,
) {
    let Some(ref_index) = ref_index else {
        return;
    };
    let Some(ids) = state
        .final_ids()
        .filter(|ids| ids.ref_index_lookup_id().is_some())
    else {
        return;
    };
    // 引用正文必须是最后一次被 QQ 接受的累计全文；stream_msg_id/msg_id 不能代替 ref_idx。
    record_c2c_bot_outbound_refs(
        ref_index,
        message,
        config,
        [ids],
        &state.last_accepted_full,
        visible_entity_snapshot,
    );
}

struct StreamFinishContext<'a> {
    user_openid: &'a str,
    msg_id: &'a str,
    message: &'a C2cMessage,
    config: &'a AppConfig,
    ref_index: Option<&'a SharedRefIndex>,
}

/// 完成 Active/BrokenActive 会话。Broken 状态仍会尝试一次串行 complete，但不会伪造成功。
async fn finish_active_stream<S: C2cStreamSender + ?Sized>(
    sender: &S,
    context: &StreamFinishContext<'_>,
    response: &CoreResponse,
    accumulated: &str,
    mut state: C2cStreamState,
    already_broken: bool,
) -> anyhow::Result<C2cStreamingPhase> {
    let mut stream_failed = already_broken;
    if !stream_failed {
        // Completed 事件可能紧跟最后一个 delta；先消费 Gateway 自己的累计全文，
        // 再对齐 Core 最终正文，确保尚未到节流定时器的内容也进入 complete。
        let candidates = [
            accumulated,
            completed_response_content(response).unwrap_or_default(),
        ];
        for candidate in candidates {
            if candidate.is_empty() || stream_failed {
                continue;
            }
            match reconcile_cumulative_text(&state.last_accepted_full, candidate) {
                CumulativeTextAction::Keep => {}
                CumulativeTextAction::Rollover(_) => {
                    trace!(
                        source_message_id = %mask_identifier(context.msg_id),
                        accepted_chars = state.last_accepted_full.chars().count(),
                        candidate_chars = candidate.chars().count(),
                        "模型最终正文回退，完成官方流并保留已接受前缀"
                    );
                }
                CumulativeTextAction::Update(next_full) => {
                    if let Err(error) = send_stream_update(
                        sender,
                        context.user_openid,
                        context.msg_id,
                        &mut state,
                        &next_full,
                    )
                    .await
                    {
                        stream_failed = true;
                        warn!(
                            source_message_id = %mask_identifier(context.msg_id),
                            stream_state = "broken_active",
                            content_chars = next_full.chars().count(),
                            error = %error.log_summary(),
                            "QQ 官方流最终累计正文更新失败，仍将用最后一次已接受正文执行 complete"
                        );
                    }
                }
            }
        }
    }

    let complete_result = complete_stream(sender, context.user_openid, context.msg_id, &mut state)
        .await
        .map_err(|error| anyhow::anyhow!("QQ 官方 C2C 流 complete 失败: {}", error.log_summary()));
    match complete_result {
        Ok(_) if !stream_failed => {
            record_stream_ref_index(
                context.ref_index,
                context.message,
                context.config,
                &state,
                response.visible_entity_snapshot.clone(),
            );
            send_completed_media_after_stream(sender, context.message, response, context.config)
                .await?;
            info!(
                source_message_id = %mask_identifier(context.msg_id),
                stream_state = "completed",
                content_chars = state.last_accepted_full.chars().count(),
                ref_index_written = state
                    .final_result
                    .as_ref()
                    .and_then(|result| result.ref_index_id.as_deref())
                    .is_some(),
                "QQ C2C 官方流式回复已完成"
            );
            Ok(C2cStreamingPhase::Completed)
        }
        Ok(_) => Err(anyhow::anyhow!(
            "QQ C2C 流式更新已失败，complete 已结束会话但不能报告为成功"
        )),
        Err(error) => Err(error),
    }
}

/// QQ C2C 流式响应处理。
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
    let finish_context = StreamFinishContext {
        user_openid,
        msg_id: reply_msg_id,
        message,
        config,
        ref_index,
    };
    let started_at = Instant::now();
    let output_policy = stream.output_policy();
    let mut phase = C2cStreamingPhase::Pending(C2cStreamState::new());
    let mut accumulated = String::new();
    let mut pending_update = false;
    let mut last_send_at = Instant::now() - Duration::from_millis(STREAM_THROTTLE_MS);
    let mut stream_first_attempted = false;
    let mut text_delta_count = 0_usize;
    let mut status_event_count = 0_usize;
    let mut progress_status_send_attempted = false;

    loop {
        let event = match stream_flush_wait(&phase, pending_update, last_send_at) {
            Some(wait) => match timeout(wait, stream.recv_event()).await {
                Ok(event) => event,
                Err(_) => {
                    if let C2cStreamingPhase::Active(mut state) = phase {
                        match send_stream_update(
                            sender,
                            user_openid,
                            reply_msg_id,
                            &mut state,
                            &accumulated,
                        )
                        .await
                        {
                            Ok(()) => {
                                pending_update = false;
                                last_send_at = Instant::now();
                                phase = C2cStreamingPhase::Active(state);
                            }
                            Err(error) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    stream_state = "broken_active",
                                    content_chars = accumulated.chars().count(),
                                    error = %error.log_summary(),
                                    "QQ 官方流定时更新失败，禁止降级为普通完整回复"
                                );
                                phase = C2cStreamingPhase::BrokenActive(state);
                            }
                        }
                    }
                    continue;
                }
            },
            None => stream.recv_event().await,
        };
        let Some(event) = event else {
            break;
        };

        match event {
            CoreResponseEvent::Status(status) => {
                status_event_count += 1;
                trace!(
                    user = %masked_user,
                    reply_msg_id = %masked_reply_msg_id,
                    status_kind = status.kind.as_str(),
                    stream_state = phase.name(),
                    status_chars = status.text.chars().count(),
                    status_event_count,
                    "C2C 流状态机已记录 Core 进度状态事件"
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
                pending_update = true;

                match phase {
                    C2cStreamingPhase::Pending(mut state) if !stream_first_attempted => {
                        stream_first_attempted = true;
                        match send_stream_update(
                            sender,
                            user_openid,
                            reply_msg_id,
                            &mut state,
                            &accumulated,
                        )
                        .await
                        {
                            Ok(()) => {
                                if let Some(typing) = typing.as_mut() {
                                    typing.stop(TypingStopReason::FirstFrame);
                                }
                                pending_update = false;
                                last_send_at = Instant::now();
                                trace!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    stream_state = "active",
                                    content_chars = accumulated.chars().count(),
                                    text_delta_count,
                                    "QQ 官方流式首个累计全文更新成功"
                                );
                                phase = C2cStreamingPhase::Active(state);
                            }
                            Err(error) => {
                                warn!(
                                    user = %masked_user,
                                    reply_msg_id = %masked_reply_msg_id,
                                    stream_state = "pending",
                                    content_chars = accumulated.chars().count(),
                                    error = %error.log_summary(),
                                    "QQ 官方流式首个更新失败，Completed 时允许普通回复 fallback"
                                );
                                phase = C2cStreamingPhase::Pending(state);
                            }
                        }
                    }
                    C2cStreamingPhase::Active(mut state) => {
                        if last_send_at.elapsed() >= Duration::from_millis(STREAM_THROTTLE_MS) {
                            match send_stream_update(
                                sender,
                                user_openid,
                                reply_msg_id,
                                &mut state,
                                &accumulated,
                            )
                            .await
                            {
                                Ok(()) => {
                                    pending_update = false;
                                    last_send_at = Instant::now();
                                    phase = C2cStreamingPhase::Active(state);
                                }
                                Err(error) => {
                                    warn!(
                                        user = %masked_user,
                                        reply_msg_id = %masked_reply_msg_id,
                                        stream_state = "broken_active",
                                        content_chars = accumulated.chars().count(),
                                        error = %error.log_summary(),
                                        "QQ 官方流式更新失败，已保留发送所有权"
                                    );
                                    phase = C2cStreamingPhase::BrokenActive(state);
                                }
                            }
                        } else {
                            phase = C2cStreamingPhase::Active(state);
                        }
                    }
                    C2cStreamingPhase::BrokenActive(state) => {
                        phase = C2cStreamingPhase::BrokenActive(state);
                    }
                    C2cStreamingPhase::Pending(state) => {
                        phase = C2cStreamingPhase::Pending(state);
                    }
                    C2cStreamingPhase::Completed => return Ok(C2cStreamingPhase::Completed),
                }
            }
            CoreResponseEvent::Completed(response) => {
                if let Some(typing) = typing.as_mut() {
                    typing.stop(TypingStopReason::FinalReply);
                }
                match phase {
                    C2cStreamingPhase::Active(state) => {
                        return finish_active_stream(
                            sender,
                            &finish_context,
                            &response,
                            &accumulated,
                            state,
                            false,
                        )
                        .await;
                    }
                    C2cStreamingPhase::BrokenActive(state) => {
                        return finish_active_stream(
                            sender,
                            &finish_context,
                            &response,
                            &accumulated,
                            state,
                            true,
                        )
                        .await;
                    }
                    C2cStreamingPhase::Pending(_) => {
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
                                response.visible_entity_snapshot.clone(),
                            );
                        }
                        info!(
                            user = %masked_user,
                            reply_msg_id = %masked_reply_msg_id,
                            stream_state = "pending",
                            fallback_used = stream_first_attempted,
                            final_chars = completed_response_content(&response)
                                .unwrap_or_default()
                                .chars()
                                .count(),
                            elapsed_ms = started_at.elapsed().as_millis(),
                            "QQ C2C 流式首个更新未成功，已发送一次普通 fallback"
                        );
                        return Ok(C2cStreamingPhase::Completed);
                    }
                    C2cStreamingPhase::Completed => return Ok(C2cStreamingPhase::Completed),
                }
            }
            CoreResponseEvent::Failed(failure) => {
                if let Some(typing) = typing.as_mut() {
                    typing.stop(failure_stop_reason(&failure));
                }
                match phase {
                    C2cStreamingPhase::Pending(_) => {
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
                        return Ok(C2cStreamingPhase::Completed);
                    }
                    C2cStreamingPhase::Active(state) => {
                        finish_failed_stream(sender, user_openid, reply_msg_id, state).await?;
                    }
                    C2cStreamingPhase::BrokenActive(state) => {
                        finish_failed_stream(sender, user_openid, reply_msg_id, state).await?;
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

    if let Some(typing) = typing.as_mut() {
        typing.stop(TypingStopReason::Cancelled);
    }
    warn!(
        user = %masked_user,
        reply_msg_id = %masked_reply_msg_id,
        stream_state = phase.name(),
        text_delta_count,
        status_event_count,
        accumulated_chars = accumulated.chars().count(),
        "Core 回复流在 Completed 前关闭"
    );
    match phase {
        C2cStreamingPhase::Active(state) | C2cStreamingPhase::BrokenActive(state) => {
            finish_closed_stream(sender, user_openid, reply_msg_id, state).await?;
        }
        C2cStreamingPhase::Pending(_) if !accumulated.is_empty() => {
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
        "core respond stream closed before Completed; accumulated_chars={}",
        accumulated.chars().count()
    ))
}

async fn finish_failed_stream<S: C2cStreamSender + ?Sized>(
    sender: &S,
    user_openid: &str,
    msg_id: &str,
    mut state: C2cStreamState,
) -> anyhow::Result<()> {
    complete_stream(sender, user_openid, msg_id, &mut state)
        .await
        .map(|_| ())
        .map_err(|error| {
            anyhow::anyhow!("Core 失败后的 QQ 流 complete 失败: {}", error.log_summary())
        })
}

async fn finish_closed_stream<S: C2cStreamSender + ?Sized>(
    sender: &S,
    user_openid: &str,
    msg_id: &str,
    mut state: C2cStreamState,
) -> anyhow::Result<()> {
    complete_stream(sender, user_openid, msg_id, &mut state)
        .await
        .map(|_| ())
        .map_err(|error| {
            anyhow::anyhow!(
                "Core 流提前关闭后的 QQ complete 失败: {}",
                error.log_summary()
            )
        })
}

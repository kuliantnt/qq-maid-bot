use super::*;

async fn send_chunk_c2c<S: OutboundSender + ?Sized>(
    sender: &S,
    target: &C2cReplyTarget,
    chunk: &OutboundChunk,
) -> (SendResult, bool) {
    if let Some(markdown) = &chunk.markdown {
        match sender.send_markdown(target, markdown).await {
            Ok(id) => return (Ok(id), false),
            Err(err) if !chunk.fallback_text.trim().is_empty() => {
                warn!(
                    user = %mask_openid(&target.user_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    error = %err.log_summary(),
                    "分片 Markdown 发送失败，将逐片降级为文本发送"
                );
                let fallback = sender.send_text(target, &chunk.fallback_text).await;
                let used_fallback = fallback.is_ok();
                return (fallback, used_fallback);
            }
            Err(err) => return (Err(err), false),
        }
    }
    let result = sender.send_text(target, &chunk.fallback_text).await;
    (result, false)
}

async fn send_chunk_group<S: GroupOutboundSender + ?Sized>(
    sender: &S,
    target: &GroupReplyTarget,
    chunk: &OutboundChunk,
) -> (SendResult, bool) {
    if let Some(markdown) = &chunk.markdown {
        match sender.send_markdown(target, markdown).await {
            Ok(id) => return (Ok(id), false),
            Err(err) if !chunk.fallback_text.trim().is_empty() => {
                warn!(
                    group = %mask_openid(&target.group_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    error = %err.log_summary(),
                    "群聊分片 Markdown 发送失败，将逐片降级为文本发送"
                );
                let fallback = sender.send_text(target, &chunk.fallback_text).await;
                let used_fallback = fallback.is_ok();
                return (fallback, used_fallback);
            }
            Err(err) => return (Err(err), false),
        }
    }
    let result = sender.send_text(target, &chunk.fallback_text).await;
    (result, false)
}

fn remaining_chars(chunks: &[OutboundChunk], from_index: usize) -> usize {
    chunks[from_index..]
        .iter()
        .map(|c| c.consumed_original_chars)
        .sum()
}

/// C2C 普通回复分段发送。
///
/// 逐段发送，每段成功后才发送下一段；任一段失败立即停止并返回 `OutboundSendError`。
/// 这里对齐官方非流式长消息发送方式：只 `await` 当前段返回，不额外 `sleep`，
/// 当前段成功后立即发送下一段。
/// `on_sent` 仅在分段成功时回调一次（带该段序号与 QQ 返回的 ID 集），调用方按用途选择
/// `message_id` 或 `ref_index_id`；失败段不回调。返回值为各段成功后收集到的 ID 集列表。
pub async fn send_c2c_outbound_chunked<S, F>(
    sender: &S,
    target: &C2cReplyTarget,
    message: &OutboundMessage,
    limits: &ChunkLimits,
    mut on_sent: F,
) -> Result<Vec<SendMessageIds>, OutboundSendError>
where
    S: OutboundSender + ?Sized,
    F: FnMut(usize, &SendMessageIds),
{
    if let OutboundMessage::Image {
        image,
        fallback_text,
    } = message
    {
        let result = match sender.send_image(target, image).await {
            Ok(ids) => Ok(ids),
            Err(error) => {
                warn!(
                    user = %mask_openid(&target.user_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    error = %error.log_summary(),
                    "C2C 图片发送失败，将降级为文本发送"
                );
                sender.send_text(target, fallback_text).await
            }
        };
        return result
            .map(|ids| {
                on_sent(0, &ids);
                vec![ids]
            })
            .map_err(|source| make_send_error(source, 0, 1, 0));
    }
    let chunks = chunk_outbound(message, limits);
    let total = chunks.len();
    let masked_user = mask_openid(&target.user_openid);
    debug!(
        user = %masked_user,
        source_message_id = target.msg_id.as_deref().unwrap_or(""),
        chunk_count = total,
        kind = outbound_kind(message),
        "正在准备 C2C 分片消息"
    );

    let mut sent_ids = Vec::with_capacity(total);
    let mut fallback_chunks = 0_usize;
    for (index, chunk) in chunks.iter().enumerate() {
        trace!(
            user = %masked_user,
            source_message_id = target.msg_id.as_deref().unwrap_or(""),
            chunk_index = chunk.chunk_index,
            chunk_count = chunk.chunk_count,
            sent_chars = chunk.rendered_chars,
            remaining_chars = remaining_chars(&chunks, index),
            message_type = message_type_name(chunk),
            "正在发送 C2C 消息分片"
        );
        match send_chunk_c2c(sender, target, chunk).await {
            (Ok(id), fallback_used) => {
                if fallback_used {
                    fallback_chunks += 1;
                }
                trace!(
                    user = %masked_user,
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    sent_chars = chunk.rendered_chars,
                    remaining_chars = remaining_chars(&chunks, index + 1),
                    message_type = message_type_name(chunk),
                    fallback_used,
                    "C2C 消息分片已发送"
                );
                on_sent(chunk.chunk_index, &id);
                sent_ids.push(id);
            }
            (Err(err), _) => {
                warn!(
                    user = %masked_user,
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    sent_chunks = index,
                    fallback_chunks,
                    remaining_chars = remaining_chars(&chunks, index),
                    error = %err.log_summary(),
                    "C2C 消息分片发送失败，停止发送剩余分片"
                );
                return Err(make_send_error(
                    err,
                    index,
                    total,
                    remaining_chars(&chunks, index),
                ));
            }
        }
    }
    info!(
        user = %masked_user,
        source_message_id = target.msg_id.as_deref().unwrap_or(""),
        chunk_count = total,
        sent_chunks = sent_ids.len(),
        fallback_chunks,
        kind = outbound_kind(message),
        "C2C 分片消息发送完成"
    );
    Ok(sent_ids)
}

/// 群普通回复分段发送。语义同 C2C 版本，区别仅在 `GroupOutboundSender` 与群 target。
/// 同样对齐官方非流式长消息发送方式：当前段 `await` 成功后立即发送下一段，
/// 不额外 `sleep`。
pub async fn send_group_outbound_chunked<S, F>(
    sender: &S,
    target: &GroupReplyTarget,
    message: &OutboundMessage,
    limits: &ChunkLimits,
    mut on_sent: F,
) -> Result<Vec<SendMessageIds>, OutboundSendError>
where
    S: GroupOutboundSender + ?Sized,
    F: FnMut(usize, &SendMessageIds),
{
    if let OutboundMessage::Image {
        image,
        fallback_text,
    } = message
    {
        let result = match sender.send_image(target, image).await {
            Ok(ids) => Ok(ids),
            Err(error) => {
                warn!(
                    group = %mask_openid(&target.group_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    error = %error.log_summary(),
                    "群聊图片发送失败，将降级为文本发送"
                );
                sender.send_text(target, fallback_text).await
            }
        };
        return result
            .map(|ids| {
                on_sent(0, &ids);
                vec![ids]
            })
            .map_err(|source| make_send_error(source, 0, 1, 0));
    }
    let chunks = chunk_outbound(message, limits);
    let total = chunks.len();
    let masked_group = mask_openid(&target.group_openid);
    debug!(
        group = %masked_group,
        source_message_id = target.msg_id.as_deref().unwrap_or(""),
        chunk_count = total,
        kind = outbound_kind(message),
        "正在准备群聊分片消息"
    );

    let mut sent_ids = Vec::with_capacity(total);
    let mut fallback_chunks = 0_usize;
    for (index, chunk) in chunks.iter().enumerate() {
        trace!(
            group = %masked_group,
            source_message_id = target.msg_id.as_deref().unwrap_or(""),
            chunk_index = chunk.chunk_index,
            chunk_count = chunk.chunk_count,
            sent_chars = chunk.rendered_chars,
            remaining_chars = remaining_chars(&chunks, index),
            message_type = message_type_name(chunk),
            "正在发送群聊消息分片"
        );
        match send_chunk_group(sender, target, chunk).await {
            (Ok(id), fallback_used) => {
                if fallback_used {
                    fallback_chunks += 1;
                }
                trace!(
                    group = %masked_group,
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    sent_chars = chunk.rendered_chars,
                    remaining_chars = remaining_chars(&chunks, index + 1),
                    message_type = message_type_name(chunk),
                    fallback_used,
                    "群聊消息分片已发送"
                );
                on_sent(chunk.chunk_index, &id);
                sent_ids.push(id);
            }
            (Err(err), _) => {
                warn!(
                    group = %masked_group,
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    chunk_index = chunk.chunk_index,
                    chunk_count = chunk.chunk_count,
                    sent_chunks = index,
                    fallback_chunks,
                    remaining_chars = remaining_chars(&chunks, index),
                    error = %err.log_summary(),
                    "群聊消息分片发送失败，停止发送剩余分片"
                );
                return Err(make_send_error(
                    err,
                    index,
                    total,
                    remaining_chars(&chunks, index),
                ));
            }
        }
    }
    info!(
        group = %masked_group,
        source_message_id = target.msg_id.as_deref().unwrap_or(""),
        chunk_count = total,
        sent_chunks = sent_ids.len(),
        fallback_chunks,
        kind = outbound_kind(message),
        "群聊分片消息发送完成"
    );
    Ok(sent_ids)
}

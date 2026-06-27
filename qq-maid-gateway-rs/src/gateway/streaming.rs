//! 流式响应消费。
//!
//! 从 `gateway/mod.rs` 提取的 SSE 流式响应处理逻辑，C2C 与 Group 共用：
//! - `build_streaming_buffered_response`：把流式 delta 缓冲文本与最终响应合并为单条响应；
//! - `collect_streaming_final_response`：群聊侧消费流式事件，等待最终响应；
//! - `handle_streaming_respond_response`：私聊侧消费流式事件并完成 QQ 回发。
//!
//! 行为约束：
//! - QQ 私聊逐条回发 delta 会刷屏，这里统一缓冲到最终文本再发；
//! - 流式异常结束时若有缓冲文本，仍尝试回发部分内容；
//! - 每次真实 QQ 发送只通过 sender / `send_c2c_text_with_status` 记录一次结果。

use anyhow::Result;
use tracing::{debug, warn};

use super::{
    MessageCache, c2c_reply_cache_key,
    event::C2cMessage,
    outbound::{RuntimeRecordingSender, send_c2c_text_with_status},
    ping::GatewayRuntimeStatus,
};
use crate::{
    api::{C2cReplyTarget, QqApiClient, send_outbound_with_fallback},
    config::AppConfig,
    render::render_respond_response,
    respond::{
        RespondResponse, RespondStream, RespondStreamEvent, respond_not_ok_to_qq_text,
        respond_response_error_summary,
    },
};

/// 把流式 delta 缓冲文本与最终响应合并为单条 `RespondResponse`。
///
/// 优先使用最终响应自带的 `text`；若为空则回退到缓冲的 delta 文本。
/// 返回 `None` 表示两者都为空，无需回发。
pub(crate) fn build_streaming_buffered_response(
    response: &RespondResponse,
    buffered_text: &str,
) -> Option<RespondResponse> {
    let text = response
        .text
        .as_ref()
        .filter(|text| !text.trim().is_empty())
        .cloned()
        .or_else(|| (!buffered_text.trim().is_empty()).then_some(buffered_text.to_owned()))?;
    let mut response = response.clone();
    response.text = Some(text);
    Some(response)
}

/// 群聊侧：消费流式事件，等待最终响应并合并缓冲文本。
///
/// 流式正常结束返回 `Some(RespondResponse)`；若流提前结束无最终响应则返回 `None`。
pub(crate) async fn collect_streaming_final_response(
    message_id: &str,
    masked_group: &str,
    mut stream: RespondStream,
) -> Option<RespondResponse> {
    let mut buffered_text = String::new();
    while let Some(event) = stream.receiver.recv().await {
        match event {
            RespondStreamEvent::Delta { text } => buffered_text.push_str(&text),
            RespondStreamEvent::Final { response } => {
                return build_streaming_buffered_response(&response, &buffered_text);
            }
        }
    }
    warn!(
        message_id = %message_id,
        group = %masked_group,
        "streaming group respond ended without final response"
    );
    None
}

/// 私聊侧：消费流式事件并完成 QQ 回发。
///
/// QQ 私聊逐条回发流式 delta 会退化成"一字一条"刷屏。
/// 这里继续保留后端 SSE，以便尽早拿到结果，但 QQ 侧统一等最终文本再发。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_streaming_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &C2cMessage,
    target: &C2cReplyTarget,
    config: &AppConfig,
    stream: RespondStream,
    reply_cache: &mut MessageCache,
) -> Result<()> {
    let mut buffered_text = String::new();
    let mut final_response = None;
    let mut stream = stream;
    while let Some(event) = stream.receiver.recv().await {
        match event {
            RespondStreamEvent::Delta { text } => {
                if !text.is_empty() {
                    debug!(
                        message_id = target.msg_id.as_deref().unwrap_or(""),
                        user = %crate::gateway::logging::mask_openid(&target.user_openid),
                        delta_len = text.chars().count(),
                        "buffering streaming QQ delta"
                    );
                    buffered_text.push_str(&text);
                }
            }
            RespondStreamEvent::Final { response } => {
                final_response = Some(response);
                break;
            }
        }
    }

    let Some(response) = final_response else {
        warn!(
            message_id = %message.message_id,
            user = %crate::gateway::logging::mask_openid(&message.user_openid),
            "streaming respond backend ended without final response"
        );
        return Ok(());
    };

    if !response.ok {
        if let Some(buffered_response) =
            build_streaming_buffered_response(&response, &buffered_text)
        {
            warn!(
                message_id = %message.message_id,
                user = %crate::gateway::logging::mask_openid(&message.user_openid),
                error_summary = %respond_response_error_summary(&response),
                reply_len = buffered_response.text.as_deref().map(|text| text.chars().count()).unwrap_or(0),
                "streaming respond finished with error after buffering partial output"
            );
            let Some(outbound) = render_respond_response(
                &buffered_response,
                config.enable_markdown,
                config.enable_image,
            ) else {
                return Ok(());
            };
            let sender = RuntimeRecordingSender {
                inner: api,
                runtime,
            };
            let sent = send_outbound_with_fallback(&sender, target, &outbound).await;
            if let Ok(Some(sent_id)) = &sent {
                let text = outbound.fallback_text().to_owned();
                if !text.is_empty() {
                    reply_cache.insert(c2c_reply_cache_key(&message.user_openid, sent_id), text);
                }
            }
            sent.inspect_err(|err| {
                warn!(
                    message_id = target.msg_id.as_deref().unwrap_or(""),
                    user = %crate::gateway::logging::mask_openid(&target.user_openid),
                    error = %err.log_summary(),
                    "streaming buffered QQ reply send failed"
                );
            })?;
            return Ok(());
        }

        let qq_text = respond_not_ok_to_qq_text(&response);
        warn!(
            message_id = %message.message_id,
            user = %crate::gateway::logging::mask_openid(&message.user_openid),
            error_summary = %respond_response_error_summary(&response),
            qq_error_text = %qq_text,
            "streaming respond returned not-ok response"
        );
        let sent = send_c2c_text_with_status(
            api,
            runtime,
            &message.user_openid,
            Some(&message.message_id),
            &qq_text,
        )
        .await;
        if let Ok(Some(sent_id)) = &sent {
            let text = qq_text.to_owned();
            if !text.is_empty() {
                reply_cache.insert(c2c_reply_cache_key(&message.user_openid, sent_id), text);
            }
        }
        sent.inspect_err(|send_err| {
            warn!(
                message_id = %message.message_id,
                user = %crate::gateway::logging::mask_openid(&message.user_openid),
                error = %send_err.log_summary(),
                local_fallback = true,
                fallback_reason = "streaming_respond_not_ok",
                qq_error_text = %qq_text,
                "streaming respond QQ fallback send failed"
            );
        })?;
        return Ok(());
    }

    let Some(buffered_response) = build_streaming_buffered_response(&response, &buffered_text)
    else {
        debug!(
            message_id = %message.message_id,
            user = %crate::gateway::logging::mask_openid(&message.user_openid),
            "streaming respond produced no reply text"
        );
        return Ok(());
    };
    let Some(outbound) = render_respond_response(
        &buffered_response,
        config.enable_markdown,
        config.enable_image,
    ) else {
        debug!(
            message_id = %message.message_id,
            user = %crate::gateway::logging::mask_openid(&message.user_openid),
            "streaming respond rendered empty outbound message"
        );
        return Ok(());
    };

    debug!(
        message_id = target.msg_id.as_deref().unwrap_or(""),
        user = %crate::gateway::logging::mask_openid(&target.user_openid),
        reply_len = outbound.fallback_text().chars().count(),
        "preparing streaming QQ reply"
    );
    let sender = RuntimeRecordingSender {
        inner: api,
        runtime,
    };
    let sent = send_outbound_with_fallback(&sender, target, &outbound).await;
    if let Ok(Some(sent_id)) = &sent {
        let text = outbound.fallback_text().to_owned();
        if !text.is_empty() {
            reply_cache.insert(c2c_reply_cache_key(&message.user_openid, sent_id), text);
        }
    }
    sent.inspect_err(|err| {
        warn!(
            message_id = target.msg_id.as_deref().unwrap_or(""),
            user = %crate::gateway::logging::mask_openid(&target.user_openid),
            error = %err.log_summary(),
            "streaming QQ reply send failed"
        );
    })?;
    Ok(())
}

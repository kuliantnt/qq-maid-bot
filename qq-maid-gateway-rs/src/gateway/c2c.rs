//! C2C 私聊消息处理管道。
//!
//! 私聊链路负责本地 `/ping`、Signal Layer 回填、Core 调用和普通回复发送；
//! C2C 流式发送状态机独立放在 `stream.rs`。

use tracing::{debug, info, warn};

use super::{
    cache::{ReplyCache, resolve_signals},
    dedupe::MessageDedupe,
    event::C2cMessage,
    logging::{c2c_message_log_summary, mask_openid},
    outbound::{RuntimeRecordingSender, send_c2c_text_with_status},
    ping::{
        GatewayRuntimeStatus, build_c2c_ping_reply_with_check_failure, is_ping_check_command,
        is_ping_command,
    },
    stream::stream_respond_c2c,
};
use crate::{
    api::{C2cReplyTarget, OutboundSender, QqApiClient, send_outbound_with_fallback},
    auth::AccessTokenManager,
    config::AppConfig,
    markdown::MarkdownPayload,
    render::{OutboundMessage, render_respond_response},
    respond::{
        RespondClient, RespondResponse, RespondTransport, build_respond_content,
        respond_error_to_qq_text,
    },
};

/// 发送 C2C 普通（非流式）回复消息，供真实网关入口调用。
async fn send_c2c_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &C2cMessage,
    response: &RespondResponse,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let sender = RuntimeRecordingSender {
        inner: api,
        runtime,
    };
    send_c2c_respond_response_with_sender(&sender, message, response, config).await
}

/// 普通 C2C 回复发送的共享实现。
///
/// 流式 fallback 必须走这里，才能保留 Markdown、文本 fallback、图片开关、reply target
/// 以及发送状态记录等既有语义。
pub(super) async fn send_c2c_respond_response_with_sender<S: OutboundSender + ?Sized>(
    sender: &S,
    message: &C2cMessage,
    response: &RespondResponse,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let masked_user = mask_openid(&message.user_openid);
    let Some(outbound) =
        render_respond_response(response, config.enable_markdown, config.enable_image)
    else {
        debug!(
            message_id = %message.message_id,
            user = %masked_user,
            "respond backend produced no reply text"
        );
        return Ok(());
    };

    let target = C2cReplyTarget {
        user_openid: message.user_openid.clone(),
        msg_id: Some(message.message_id.clone()),
    };
    debug!(
        message_id = target.msg_id.as_deref().unwrap_or(""),
        user = %masked_user,
        reply_len = outbound.fallback_text().chars().count(),
        "preparing QQ reply"
    );
    send_outbound_with_fallback(sender, &target, &outbound)
        .await
        .inspect_err(|err| {
            warn!(
                message_id = target.msg_id.as_deref().unwrap_or(""),
                user = %masked_user,
                error = %err.log_summary(),
                "QQ reply send failed"
            );
        })?;
    Ok(())
}

// 私聊消息处理需要贯穿 QQ 回复、LLM 调用、去重和诊断状态，保持参数显式便于看清跨层依赖。
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_c2c_message(
    mut message: C2cMessage,
    config: &AppConfig,
    auth: &AccessTokenManager,
    respond: &RespondClient,
    api: &QqApiClient,
    _dedupe: &MessageDedupe,
    reply_cache: &ReplyCache,
    runtime: &GatewayRuntimeStatus,
) -> anyhow::Result<()> {
    // Ingress 已完成解析；这里固定先走 Signal Layer，再进入 Egress content 构建。
    resolve_signals(&mut message, reply_cache);
    log_c2c_message_received(&message, config.verbose_log);
    runtime.record_c2c_message_received(&message);

    let masked_user = mask_openid(&message.user_openid);
    let respond_content = build_respond_content(&message);
    if respond_content.trim().is_empty() {
        debug!(
            message_id = %message.message_id,
            user = %masked_user,
            "ignoring empty C2C message"
        );
        return Ok(());
    }
    // C2C message/event ID 已在 Aggregator 入口原子 reservation；这里不能再按逻辑批次任意 source ID 命中丢弃整批。
    if is_ping_command(&message.content) {
        info!(
            message_id = %message.message_id,
            user = %masked_user,
            "local /ping command matched"
        );
        let check_failure = if is_ping_check_command(&message.content) {
            respond.check_upstream().await.err().map(|err| {
                let summary = format!("主动检查失败：{}", err.qq_visible_kind());
                warn!(
                    message_id = %message.message_id,
                    user = %masked_user,
                    error = %err.log_summary(),
                    "active LLM upstream check request failed"
                );
                summary
            })
        } else {
            None
        };
        let reply = build_c2c_ping_reply_with_check_failure(
            &message,
            config,
            runtime,
            auth,
            &respond.health_snapshot(),
            check_failure.as_deref(),
        )
        .await;
        let target = C2cReplyTarget {
            user_openid: message.user_openid,
            msg_id: Some(message.message_id),
        };
        let outbound = render_local_ping_reply(reply, config.enable_markdown);
        debug!(
            message_id = target.msg_id.as_deref().unwrap_or(""),
            user = %mask_openid(&target.user_openid),
            reply_len = outbound.fallback_text().chars().count(),
            "preparing local /ping reply"
        );
        let sender = RuntimeRecordingSender {
            inner: api,
            runtime,
        };
        send_outbound_with_fallback(&sender, &target, &outbound)
            .await
            .inspect_err(|err| {
                warn!(
                    message_id = target.msg_id.as_deref().unwrap_or(""),
                    user = %mask_openid(&target.user_openid),
                    error = %err.log_summary(),
                    "local /ping QQ reply send failed"
                );
            })?;
        return Ok(());
    }

    info!(
        message_id = %message.message_id,
        user = %masked_user,
        "calling respond backend"
    );
    let transport = match respond.respond_c2c(&message, respond_content).await {
        Ok(response) => {
            runtime.record_respond_success();
            response
        }
        Err(err) => {
            runtime.record_respond_failure(err.log_summary());
            let qq_text = respond_error_to_qq_text(&err);
            warn!(
                message_id = %message.message_id,
                user = %masked_user,
                error = %err.log_summary(),
                local_fallback = true,
                fallback_reason = "respond_error",
                qq_error_text = %qq_text,
                "respond backend call failed; sending local QQ fallback"
            );
            send_c2c_text_with_status(
                api,
                runtime,
                &message.user_openid,
                Some(&message.message_id),
                &qq_text,
            )
            .await
            .inspect_err(|send_err| {
                warn!(
                    message_id = %message.message_id,
                    user = %masked_user,
                    error = %send_err.log_summary(),
                    local_fallback = true,
                    fallback_reason = "respond_error",
                    qq_error_text = %qq_text,
                    "local QQ fallback send failed"
                );
            })?;
            return Ok(());
        }
    };

    match transport {
        RespondTransport::Complete(response) => {
            send_c2c_respond_response(api, runtime, &message, &response, config).await?;
        }
        RespondTransport::Stream(stream) => {
            stream_respond_c2c(stream, api, runtime, &message, config).await?;
        }
    }
    Ok(())
}

fn render_local_ping_reply(reply: String, enable_markdown: bool) -> OutboundMessage {
    if enable_markdown {
        // `/ping` 本地生成的状态报告本身就是 Markdown；发送层复用现有 fallback，
        // 避免 QQ Markdown 权限或平台兼容问题导致诊断消息完全丢失。
        return OutboundMessage::Markdown {
            markdown: MarkdownPayload::new(reply.clone()),
            fallback_text: reply,
        };
    }
    OutboundMessage::Text { text: reply }
}

fn log_c2c_message_received(message: &C2cMessage, verbose_log: bool) {
    let summary = c2c_message_log_summary(message, verbose_log);
    if let Some(extracted_content) = summary.extracted_content.as_deref() {
        info!(
            message_id = %summary.message_id,
            user = %summary.masked_user,
            content_len = summary.content_len,
            attachment_count = summary.attachment_count,
            is_ping = summary.is_ping,
            extracted_content = %extracted_content,
            "received C2C message"
        );
    } else {
        info!(
            message_id = %summary.message_id,
            user = %summary.masked_user,
            content_len = summary.content_len,
            attachment_count = summary.attachment_count,
            is_ping = summary.is_ping,
            "received C2C message"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ping_reply_respects_markdown_config() {
        let markdown = render_local_ping_reply("# 状态\n\n| A | B |".to_owned(), true);
        assert_eq!(
            markdown,
            OutboundMessage::Markdown {
                markdown: MarkdownPayload::new("# 状态\n\n| A | B |"),
                fallback_text: "# 状态\n\n| A | B |".to_owned(),
            }
        );

        let text = render_local_ping_reply("# 状态".to_owned(), false);
        assert_eq!(
            text,
            OutboundMessage::Text {
                text: "# 状态".to_owned(),
            }
        );
    }
}

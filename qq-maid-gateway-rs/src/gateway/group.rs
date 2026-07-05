//! 群消息处理管道。
//!
//! 这里串起群消息过滤、Core 调用、QQ 群回复发送和机器人 outbound id 回填。
//! 群触发策略与冷却的纯判定逻辑放在 `group_filter.rs`，避免处理管道继续膨胀。

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use tracing::{debug, info, warn};

use super::{
    bot_identity::SharedBotIdentity,
    cache::BotOutboundCache,
    dedupe::MessageDedupe,
    event::{GroupEventType, GroupMessage},
    group_filter::{GroupCooldowns, should_ignore_group_message, should_process_group_message},
    logging::{group_message_log_summary, mask_openid},
    media_fetch::{MediaFetchContext, fetch_qq_official_image_attachments},
    outbound::{
        ReplyCapability, ReplyTarget, RuntimeRecordingGroupSender, send_group_text_with_status,
    },
    ping::GatewayRuntimeStatus,
};
use crate::{
    api::QqApiClient,
    config::AppConfig,
    message_chunk::{ChunkLimits, OutboundSendError, send_group_outbound_chunked},
    render::{OutboundMessage, render_respond_response_for_profile},
    respond::{
        RespondClient, RespondEvent, RespondResponse, RespondTransport, respond_error_to_qq_text,
    },
};

fn group_reply_mention_prefix(
    message: &GroupMessage,
    capability: &ReplyCapability,
) -> Option<String> {
    // 只有用户显式 @ 机器人触发的官方群 at 事件，才在回复正文里 @ 回发起人；
    // 普通群命令、关键词触发和回复机器人消息继续只挂原消息 msg_id，避免额外打扰。
    if !capability.supports_at_mention {
        return None;
    }
    if message.event_type != GroupEventType::GroupAtMessage {
        return None;
    }
    message
        .member_openid
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|member_openid| format!("<@{member_openid}>"))
}

fn prefix_group_reply_text(
    message: &GroupMessage,
    text: &str,
    capability: &ReplyCapability,
) -> String {
    let Some(prefix) = group_reply_mention_prefix(message, capability) else {
        return text.to_owned();
    };
    if text.trim().is_empty() {
        prefix
    } else {
        format!("{prefix}\n{text}")
    }
}

fn prefix_group_reply_outbound(
    message: &GroupMessage,
    outbound: OutboundMessage,
    capability: &ReplyCapability,
) -> OutboundMessage {
    let Some(prefix) = group_reply_mention_prefix(message, capability) else {
        return outbound;
    };
    outbound.prefix_text(&prefix)
}

fn group_respond_error_texts(
    message: &GroupMessage,
    err: &crate::respond::RespondError,
    capability: &ReplyCapability,
) -> (String, String) {
    let log_text = respond_error_to_qq_text(err);
    // 群 at fallback 的实际 QQ 文本需要保留 <@openid>，但日志字段只能使用未加前缀的安全文案。
    let qq_text = prefix_group_reply_text(message, &log_text, capability);
    (qq_text, log_text)
}

// 群消息链路同样需要显式串起 QQ 回复、LLM 调用、去重、冷却和运行状态；
// 这里沿用私聊分支的做法保留展开参数，避免把跨层依赖藏进临时聚合对象。
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_group_message(
    mut message: GroupMessage,
    config: &AppConfig,
    respond: &RespondClient,
    api: &QqApiClient,
    dedupe: &MessageDedupe,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    group_cooldowns: &Arc<Mutex<GroupCooldowns>>,
    bot_identity: &SharedBotIdentity,
    runtime: &GatewayRuntimeStatus,
) -> anyhow::Result<()> {
    log_group_message_received(&message, config.verbose_log);
    let masked_group = mask_openid(&message.group_openid);
    let respond_content =
        crate::respond::build_group_respond_content(&message, &config.group_active_keywords);
    if should_ignore_group_message(&message, &respond_content, &masked_group) {
        return Ok(());
    }
    if dedupe.is_duplicate(&message.message_id) {
        info!(
            message_id = %message.message_id,
            group = %masked_group,
            "duplicate group message ignored"
        );
        return Ok(());
    }
    if !should_process_group_message(
        config.group_message_mode,
        &config.group_active_keywords,
        &message,
        &respond_content,
        bot_identity,
        group_outbound_cache,
    ) {
        let active_keyword_count = config.group_active_keywords.len();
        debug!(
            message_id = %message.message_id,
            group = %masked_group,
            event_type = message.event_type.as_respond_event_type(),
            mode = ?config.group_message_mode,
            active_keyword_count,
            "group message ignored by mode policy"
        );
        return Ok(());
    }
    if message.event_type == GroupEventType::GroupMessage
        && !group_cooldowns
            .lock()
            .unwrap()
            .check_and_mark(&message, Instant::now())
    {
        info!(
            message_id = %message.message_id,
            group = %masked_group,
            member = %message.member_openid.as_deref().map(mask_openid).unwrap_or_default(),
            "group message ignored by cooldown"
        );
        return Ok(());
    }

    fetch_qq_official_image_attachments(
        &reqwest::Client::new(),
        &MediaFetchContext {
            platform: "qq_official",
            app_id: config.app_id.clone(),
            peer_id: message.group_openid.clone(),
            root_dir: config.media_dir.clone(),
            timeout: config.media_download_timeout,
            max_bytes: config.media_max_bytes,
        },
        &message.message_id,
        &mut message.input_parts,
        &message.attachments,
    )
    .await;

    info!(
        message_id = %message.message_id,
        group = %masked_group,
        "calling respond backend for group"
    );
    let transport = match respond.respond_group(&message, respond_content).await {
        Ok(response) => {
            runtime.record_respond_success();
            response
        }
        Err(err) => {
            runtime.record_respond_failure(err.log_summary());
            let capability = ReplyCapability::qq_official_group(config);
            let (qq_text, log_text) = group_respond_error_texts(&message, &err, &capability);
            warn!(
                message_id = %message.message_id,
                group = %masked_group,
                error = %err.log_summary(),
                local_fallback = true,
                fallback_reason = "respond_error",
                qq_error_text = %log_text,
                "respond backend call failed; sending local group fallback"
            );
            let sent_message_id = send_group_text_with_status(
                api,
                runtime,
                &message.group_openid,
                Some(&message.message_id),
                &qq_text,
            )
            .await?;
            group_outbound_cache.lock().unwrap().insert(sent_message_id);
            return Ok(());
        }
    };

    match transport {
        RespondTransport::Complete(response) => {
            send_group_respond_response(
                api,
                runtime,
                config,
                group_outbound_cache,
                &message,
                &response,
            )
            .await?;
        }
        RespondTransport::Stream(stream) => {
            if let Some(response) = consume_respond_stream(stream).await {
                send_group_respond_response(
                    api,
                    runtime,
                    config,
                    group_outbound_cache,
                    &message,
                    &response,
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn send_group_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    config: &AppConfig,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    message: &GroupMessage,
    response: &RespondResponse,
) -> anyhow::Result<()> {
    let capability = ReplyCapability::qq_official_group(config);
    let Some(outbound) = render_respond_response_for_profile(response, &capability.render) else {
        debug!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            "respond backend produced no group reply text"
        );
        return Ok(());
    };
    let outbound = prefix_group_reply_outbound(message, outbound, &capability);
    let sender = RuntimeRecordingGroupSender {
        inner: api,
        runtime,
    };
    let target = ReplyTarget::qq_group(
        message.group_openid.clone(),
        Some(message.message_id.clone()),
    )
    .to_qq_group_target()
    .expect("QQ group reply target should adapt to QQ API target");
    let limits = ChunkLimits::new(
        config.markdown_chunk_soft_limit,
        config.text_chunk_soft_limit,
    );
    // 普通群回复统一走分段编排：每个成功发送并返回 message id 的分段写入
    // `BotOutboundCache`；失败分段不写，错误向上传递为 PartiallySent / NotSent。
    match send_group_outbound_chunked(
        &sender,
        &target,
        &outbound,
        &limits,
        |_, sent_message_id| {
            group_outbound_cache
                .lock()
                .unwrap()
                .insert(sent_message_id.clone());
        },
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(OutboundSendError::NotSent { source }) => Err(source.into()),
        Err(OutboundSendError::PartiallySent { source, .. }) => {
            // 已成功前段已写入 cache，这里只把底层错误向上传递，不伪造完整送达。
            Err(source.into())
        }
    }
}

async fn consume_respond_stream(
    mut stream: qq_maid_core::service::CoreResponseStream,
) -> Option<RespondResponse> {
    let output_policy = stream.output_policy();
    let mut status_event_count = 0_usize;
    let mut text_delta_count = 0_usize;
    while let Some(event) = stream.recv().await {
        match event {
            RespondEvent::Status(status) => {
                status_event_count += 1;
                debug!(
                    status_kind = status.kind.as_str(),
                    response_delivery_mode = "progress_status",
                    status_chars = status.text.chars().count(),
                    status_event_count,
                    "group stream status event recorded without group progress send"
                );
            }
            RespondEvent::TextDelta(delta) => {
                if !delta.is_empty() {
                    text_delta_count += 1;
                }
            }
            RespondEvent::Completed(response) => {
                debug!(
                    response_delivery_mode = output_policy.as_str(),
                    text_delta_count,
                    status_event_count,
                    "group stream collapsed into single Completed response"
                );
                return Some(response);
            }
            RespondEvent::Failed(failure) => {
                warn!(
                    kind = ?failure.kind,
                    retryable = failure.retryable,
                    response_delivery_mode = output_policy.as_str(),
                    text_delta_count,
                    status_event_count,
                    "core respond stream failed"
                );
                return None;
            }
        }
    }
    None
}

fn log_group_message_received(message: &GroupMessage, verbose_log: bool) {
    let summary = group_message_log_summary(message, verbose_log);
    if let Some(extracted_content) = summary.extracted_content.as_deref() {
        info!(
            message_id = %summary.message_id,
            group = %summary.masked_group,
            member = %summary.masked_member.as_deref().unwrap_or(""),
            event_type = summary.event_type,
            content_len = summary.content_len,
            mention_count = summary.mention_count,
            attachment_count = summary.attachment_count,
            is_ping = summary.is_ping,
            extracted_content = %extracted_content,
            "received group message"
        );
    } else {
        info!(
            message_id = %summary.message_id,
            group = %summary.masked_group,
            member = %summary.masked_member.as_deref().unwrap_or(""),
            event_type = summary.event_type,
            content_len = summary.content_len,
            mention_count = summary.mention_count,
            attachment_count = summary.attachment_count,
            is_ping = summary.is_ping,
            "received group message"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentTypingConfig, DEFAULT_CONVERSATION_QUEUE_CAPACITY, DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT,
        DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS, DEFAULT_MEDIA_MAX_BYTES,
        DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS, DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS,
        DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES, DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS,
        DEFAULT_MESSAGE_AGGREGATION_QUIET_MS, DEFAULT_TEXT_CHUNK_SOFT_LIMIT, GroupMessageMode,
        MessageAggregationConfig,
    };
    use crate::{api::QqApiClient, auth::AccessTokenManager};
    use axum::{Router, body::Bytes, routing::get};
    use qq_maid_common::input_part::{MessageInputPart, MessageMedia};
    use qq_maid_core::service::{
        CoreError, CoreHealthSnapshot, CoreInboundClassification, CoreRequest, CoreRespondOutput,
        CoreService, UpstreamStatusSnapshot,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tokio::net::TcpListener;

    fn group_message(content: &str, event_type: GroupEventType) -> GroupMessage {
        GroupMessage {
            message_id: "group-msg-1".to_owned(),
            group_openid: "group-1".to_owned(),
            member_openid: Some("member-1".to_owned()),
            member_role: None,
            content: content.to_owned(),
            mentions: Vec::new(),
            reply: None,
            timestamp: None,
            input_parts: if content.trim().is_empty() {
                Vec::new()
            } else {
                vec![qq_maid_common::input_part::MessageInputPart::text(content)]
            },
            attachments: Vec::new(),
            event_type,
            author_is_bot: false,
            author_is_self: false,
        }
    }

    fn test_config() -> AppConfig {
        AppConfig {
            app_id: "app".to_owned(),
            app_secret: "secret".to_owned(),
            bot_mention_ids: Vec::new(),
            sandbox: false,
            api_base: "https://example.test".to_owned(),
            token_refresh_margin: Duration::from_secs(60),
            enable_markdown: true,
            enable_image: false,
            enable_group_messages: true,
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
            c2c_final_reply_stream_enabled: false,
            c2c_visible_progress_status_enabled: true,
            agent_typing: AgentTypingConfig {
                enabled: false,
                delay: Duration::from_secs(1),
            },
            markdown_chunk_soft_limit: DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT,
            text_chunk_soft_limit: DEFAULT_TEXT_CHUNK_SOFT_LIMIT,
            media_dir: std::path::PathBuf::from("media/inbound"),
            media_download_timeout: Duration::from_secs(10),
            media_max_bytes: DEFAULT_MEDIA_MAX_BYTES,
            wechat_service: crate::config::WechatServiceConfig::default(),
        }
    }

    fn qq_group_capability() -> ReplyCapability {
        ReplyCapability::qq_official_group(&test_config())
    }

    struct MockCore {
        response: RespondResponse,
    }

    #[async_trait::async_trait]
    impl CoreService for MockCore {
        async fn respond(&self, _request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
            Ok(CoreRespondOutput::Complete(self.response.clone()))
        }

        async fn classify_inbound(
            &self,
            _request: CoreRequest,
        ) -> Result<CoreInboundClassification, CoreError> {
            unreachable!("group handler tests do not classify inbound")
        }

        async fn upstream_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        fn health_snapshot(&self) -> CoreHealthSnapshot {
            CoreHealthSnapshot {
                ok: true,
                provider: "mock".to_owned(),
                model: "mock".to_owned(),
                stream: false,
                upstream: UpstreamStatusSnapshot::default(),
            }
        }
    }

    fn respond_client() -> RespondClient {
        RespondClient::new(Arc::new(MockCore {
            response: RespondResponse {
                text: None,
                markdown: None,
                handled: Some(true),
                session_id: None,
                command: None,
                diagnostics: None,
            },
        }))
    }

    fn api_client() -> QqApiClient {
        QqApiClient::new(
            reqwest::Client::new(),
            "https://example.test",
            AccessTokenManager::new(
                reqwest::Client::new(),
                "app",
                "secret",
                Duration::from_secs(60),
            ),
        )
    }

    fn bot_identity() -> SharedBotIdentity {
        Arc::new(crate::gateway::bot_identity::BotIdentity::new("app", &[]))
    }

    fn media_message(
        message_id: &str,
        content: &str,
        event_type: GroupEventType,
        url: String,
    ) -> GroupMessage {
        let attachment = crate::event::Attachment {
            content_type: Some("image/jpeg".to_owned()),
            filename: Some("a.jpg".to_owned()),
            url: Some(url),
            size_bytes: None,
            media_id: None,
            file_id: None,
            attachment_id: None,
        };
        let mut message = group_message(content, event_type);
        message.message_id = message_id.to_owned();
        message.attachments = vec![attachment.clone()];
        message.input_parts = vec![
            MessageInputPart::text(content),
            MessageInputPart::image(MessageMedia {
                mime_type: attachment.content_type.clone(),
                filename: attachment.filename.clone(),
                url: attachment.url.clone(),
                status: qq_maid_common::input_part::MediaStatus::MissingReadableUrl,
                ..Default::default()
            }),
        ];
        message
    }

    fn media_file_count(root: &std::path::Path) -> usize {
        if !root.exists() {
            return 0;
        }
        let mut pending = vec![root.to_path_buf()];
        let mut count = 0;
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    count += 1;
                }
            }
        }
        count
    }

    async fn spawn_media_server() -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_for_route = hits.clone();
        let app = Router::new().route(
            "/a.jpg",
            get(move || {
                let hits = hits_for_route.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (
                        [(reqwest::header::CONTENT_TYPE.as_str(), "image/jpeg")],
                        Bytes::from_static(b"fake-jpeg"),
                    )
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/a.jpg"), hits)
    }

    #[test]
    fn group_at_reply_text_mentions_sender_when_member_openid_exists() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);
        let capability = qq_group_capability();

        assert_eq!(
            prefix_group_reply_text(&message, "回复正文", &capability),
            "<@member-1>\n回复正文"
        );
    }

    #[test]
    fn group_at_respond_error_log_text_keeps_member_openid_out() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);
        let error = crate::respond::RespondError::Core(qq_maid_core::service::CoreError::new(
            "internal_error",
            "respond",
            "backend down",
        ));
        let capability = qq_group_capability();

        let (qq_text, log_text) = group_respond_error_texts(&message, &error, &capability);

        assert!(qq_text.starts_with("<@member-1>\n"));
        assert!(!log_text.contains("member-1"));
        assert!(!log_text.contains("<@"));
    }

    #[test]
    fn group_reply_text_skips_mention_for_plain_group_message() {
        let message = group_message("hello", GroupEventType::GroupMessage);
        let capability = qq_group_capability();

        assert_eq!(
            prefix_group_reply_text(&message, "回复正文", &capability),
            "回复正文"
        );
    }

    #[test]
    fn group_at_reply_text_skips_mention_without_member_openid() {
        let mut message = group_message("hello", GroupEventType::GroupAtMessage);
        message.member_openid = None;
        let capability = qq_group_capability();

        assert_eq!(
            prefix_group_reply_text(&message, "回复正文", &capability),
            "回复正文"
        );
    }

    #[test]
    fn group_at_reply_outbound_mentions_sender() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);
        let capability = qq_group_capability();
        let outbound = OutboundMessage::Text {
            text: "回复正文".to_owned(),
        };

        assert_eq!(
            prefix_group_reply_outbound(&message, outbound, &capability),
            OutboundMessage::Text {
                text: "<@member-1>\n回复正文".to_owned(),
            }
        );
    }

    #[test]
    fn group_at_reply_respects_platform_mention_capability() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);
        let mut capability = qq_group_capability();
        capability.supports_at_mention = false;

        assert_eq!(
            prefix_group_reply_text(&message, "回复正文", &capability),
            "回复正文"
        );
    }

    #[tokio::test]
    async fn mode_policy_blocked_group_message_does_not_download_media() {
        let mut config = test_config();
        config.group_message_mode = GroupMessageMode::Off;
        config.media_dir =
            std::env::temp_dir().join(format!("qq-maid-group-mode-policy-{}", std::process::id()));
        let (url, hits) = spawn_media_server().await;
        let message = media_message("group-off", "普通聊天", GroupEventType::GroupMessage, url);

        handle_group_message(
            message,
            &config,
            &respond_client(),
            &api_client(),
            &crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60)),
            &Arc::new(Mutex::new(BotOutboundCache::default())),
            &Arc::new(Mutex::new(GroupCooldowns::default())),
            &bot_identity(),
            &GatewayRuntimeStatus::new(),
        )
        .await
        .unwrap();

        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert_eq!(media_file_count(&config.media_dir), 0);
    }

    #[tokio::test]
    async fn cooldown_and_dedupe_blocked_group_messages_do_not_download_media() {
        let mut config = test_config();
        config.group_message_mode = GroupMessageMode::Active;
        config.media_dir =
            std::env::temp_dir().join(format!("qq-maid-group-cooldown-{}", std::process::id()));
        let outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let cooldowns = Arc::new(Mutex::new(GroupCooldowns::default()));
        let dedupe = crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60));
        let respond = respond_client();
        let api = api_client();
        let runtime = GatewayRuntimeStatus::new();
        let identity = bot_identity();

        let (url_first, hits_first) = spawn_media_server().await;
        handle_group_message(
            media_message(
                "group-cooldown-1",
                "小女仆 看图",
                GroupEventType::GroupMessage,
                url_first,
            ),
            &config,
            &respond,
            &api,
            &dedupe,
            &outbound_cache,
            &cooldowns,
            &identity,
            &runtime,
        )
        .await
        .unwrap();
        assert_eq!(hits_first.load(Ordering::SeqCst), 1);

        let (url_second, hits_second) = spawn_media_server().await;
        handle_group_message(
            media_message(
                "group-cooldown-2",
                "小女仆 再看一次",
                GroupEventType::GroupMessage,
                url_second,
            ),
            &config,
            &respond,
            &api,
            &dedupe,
            &outbound_cache,
            &cooldowns,
            &identity,
            &runtime,
        )
        .await
        .unwrap();
        assert_eq!(hits_second.load(Ordering::SeqCst), 0);

        let (url_third, hits_third) = spawn_media_server().await;
        handle_group_message(
            media_message(
                "group-cooldown-1",
                "小女仆 重复消息",
                GroupEventType::GroupMessage,
                url_third,
            ),
            &config,
            &respond,
            &api,
            &dedupe,
            &outbound_cache,
            &cooldowns,
            &identity,
            &runtime,
        )
        .await
        .unwrap();
        assert_eq!(hits_third.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn processed_group_message_downloads_media_after_filters() {
        let mut config = test_config();
        config.group_message_mode = GroupMessageMode::Active;
        config.media_dir =
            std::env::temp_dir().join(format!("qq-maid-group-download-{}", std::process::id()));
        let (url, hits) = spawn_media_server().await;
        let message = media_message(
            "group-download",
            "小女仆 看图",
            GroupEventType::GroupMessage,
            url,
        );

        handle_group_message(
            message,
            &config,
            &respond_client(),
            &api_client(),
            &crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60)),
            &Arc::new(Mutex::new(BotOutboundCache::default())),
            &Arc::new(Mutex::new(GroupCooldowns::default())),
            &bot_identity(),
            &GatewayRuntimeStatus::new(),
        )
        .await
        .unwrap();

        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(media_file_count(&config.media_dir), 1);
    }
}

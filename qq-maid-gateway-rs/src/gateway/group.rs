//! 群消息处理管道。
//!
//! 这里串起群消息过滤、Core 调用、QQ 群回复发送和机器人 outbound id 回填。
//! 群触发策略与冷却的纯判定逻辑放在 `group_filter.rs`，避免处理管道继续膨胀。

use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::Context;
use qq_maid_core::service::CoreInboundKind;
use tracing::{debug, info, warn};

fn empty_group_reply_fallback_text(bot_display_name: &str) -> String {
    format!("唔，{bot_display_name}刚刚没整理出可用回复。可以再说一次。")
}

/// 群聊冷却命中但明确指向机器人时的轻量提示文案。
///
/// 不走 LLM，仅让用户知道“机器人听见了但要稍等”，避免静默吞掉造成“没听见”的体感
/// （#386）。称呼复用主动关键词首项，且不携带任何业务臆测。
fn group_cooldown_hint_text(bot_display_name: &str) -> String {
    format!("哦哦，刚刚在处理上一条消息，稍后再说一声{bot_display_name}就能继续了呢。")
}

use super::{
    bot_identity::SharedBotIdentity,
    cache::BotOutboundCache,
    dedupe::MessageDedupe,
    event::{GroupEventType, GroupMessage},
    group_filter::{
        GroupCooldowns, group_message_addresses_bot, mentions_current_bot,
        should_ignore_group_message, should_process_group_message,
    },
    logging::{group_message_log_summary, mask_openid},
    media_fetch::{MediaFetchContext, fetch_qq_official_image_attachments},
    outbound::{
        ReplyCapability, ReplyTarget, RuntimeRecordingGroupSender, send_group_text_with_status,
    },
    ping::GatewayRuntimeStatus,
    platform,
    ref_index::SharedRefIndex,
};
use crate::{
    api::{QqApiClient, SendMessageIds},
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
    // 只有官方确认提到当前机器人时，才在回复正文里 @ 回发起人；
    // 普通群命令、关键词触发和回复机器人消息继续只挂原消息 msg_id，避免额外打扰。
    if !capability.supports_at_mention {
        return None;
    }
    if !mentions_current_bot(message) {
        return None;
    }
    message
        .member_openid
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|member_openid| format!("<@{member_openid}>"))
}

fn prefix_group_reply_outbound(
    message: &GroupMessage,
    outbound: OutboundMessage,
    capability: &ReplyCapability,
) -> OutboundMessage {
    // QQ 官方群文本消息不会把 `<@openid>` 渲染成昵称，会直接暴露 openid。
    // 只有 markdown 出站消息保留显式 at；纯文本命令依靠 reply target 关联原消息。
    if !matches!(outbound, OutboundMessage::Markdown { .. }) {
        return outbound;
    }
    let Some(prefix) = group_reply_mention_prefix(message, capability) else {
        return outbound;
    };
    outbound.prefix_text(&prefix)
}

fn group_respond_error_texts(
    _message: &GroupMessage,
    err: &crate::respond::RespondError,
    _capability: &ReplyCapability,
) -> (String, String) {
    let log_text = respond_error_to_qq_text(err);
    // 本地错误 fallback 是纯文本发送，不能拼 `<@openid>`，否则 QQ 会原样展示 openid。
    let qq_text = log_text.clone();
    (qq_text, log_text)
}

/// 群聊冷却命中但明确指向机器人时发送轻量提示。
///
/// 这里只发一条普通文本，挂在被冷却消息的 `message_id` 上回复；不走 LLM、不调 Core，
/// 发送失败只记录运行状态日志，不阻断或重试，避免冷却路径本身被放大成新的负担。
async fn send_cooldown_hint(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    message: &GroupMessage,
    bot_display_name: &str,
) {
    let text = group_cooldown_hint_text(bot_display_name);
    let result = send_group_text_with_status(
        api,
        runtime,
        &message.group_openid,
        Some(&message.message_id),
        &text,
    )
    .await;
    if let Err(error) = result {
        warn!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            error = %error.log_summary(),
            "group cooldown hint send failed"
        );
    }
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
    ref_index: &SharedRefIndex,
) -> anyhow::Result<()> {
    log_group_message_received(&message, config.verbose_log);
    let masked_group = mask_openid(&message.group_openid);
    let respond_content =
        crate::respond::build_group_respond_content(&message, &config.group_active_keywords);
    observe_group_message_ref_index(&message, respond, ref_index);
    if should_ignore_group_message(
        &message,
        &respond_content,
        &masked_group,
        group_outbound_cache,
    ) {
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
    // 只用轻量、身份完整的 Core request 读取确定性命令和当前 actor 可见 Pending；
    // 分类不会下载媒体、补全成员信息或写入 ref index。Immediate 直接进入 Core，普通聊天
    // 才沿用群级/用户级冷却，避免 Gateway 复制 Memory/Todo 等业务词判断。
    let bypass_normal_chat_cooldown = if message.event_type == GroupEventType::GroupMessage {
        match respond
            .classify_group(
                &message,
                &config.group_active_keywords,
                respond_content.clone(),
            )
            .await
        {
            Ok(classification) => classification.kind == CoreInboundKind::Immediate,
            Err(error) => {
                warn!(
                    message_id = %message.message_id,
                    group = %masked_group,
                    member = %message.member_openid.as_deref().map(mask_openid).unwrap_or_default(),
                    error = %error.log_summary(),
                    "group inbound classification failed; preserving normal chat cooldown"
                );
                false
            }
        }
    } else {
        false
    };
    if message.event_type == GroupEventType::GroupMessage
        && !bypass_normal_chat_cooldown
        && !group_cooldowns
            .lock()
            .unwrap()
            .check_and_mark(&message, Instant::now())
    {
        if group_message_addresses_bot(&message, group_outbound_cache) {
            info!(
                message_id = %message.message_id,
                group = %masked_group,
                member = %message.member_openid.as_deref().map(mask_openid).unwrap_or_default(),
                "group message throttled by cooldown; sending lightweight hint"
            );
            send_cooldown_hint(api, runtime, &message, config.bot_display_name()).await;
        } else {
            info!(
                message_id = %message.message_id,
                group = %masked_group,
                member = %message.member_openid.as_deref().map(mask_openid).unwrap_or_default(),
                "group message ignored by cooldown"
            );
        }
        return Ok(());
    }

    fetch_qq_official_image_attachments(
        &reqwest::Client::new(),
        &MediaFetchContext {
            platform: "qq_official",
            app_id: config
                .app_id
                .clone()
                .context("QQ group handler requires a bound channel")?,
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

    let mut inbound = respond.prepare_inbound(crate::respond::normalized_group_inbound(
        &message,
        &config.group_active_keywords,
    ));
    {
        let mut index = ref_index.lock().unwrap();
        index.enrich_inbound(&mut inbound);
    }
    // 成员详情补全（#319）：best-effort 调用 #229 补全 actor / mention / 引用 sender
    // 的展示字段，失败降级 source=Event，不阻断主回复流程。补全后再 insert_inbound，
    // 让索引里存的是补全后的 sender。配置开关默认开启，可经环境变量关闭。
    if config.member_detail_enrich_enabled {
        platform::member_enrich::enrich_inbound_member_details(api, &mut inbound).await;
    }
    {
        let mut index = ref_index.lock().unwrap();
        index.insert_inbound(&inbound);
    }

    info!(
        message_id = %message.message_id,
        group = %masked_group,
        "calling respond backend for group"
    );
    let transport = match respond.respond_inbound(&inbound, respond_content).await {
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
            group_outbound_cache
                .lock()
                .unwrap()
                .insert(sent_message_id.message_id.clone());
            group_outbound_cache
                .lock()
                .unwrap()
                .insert_ref_index_id(sent_message_id.ref_index_id);
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
                ref_index,
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
                    ref_index,
                )
                .await?;
            }
        }
    }
    Ok(())
}

fn observe_group_message_ref_index(
    message: &GroupMessage,
    respond: &RespondClient,
    ref_index: &SharedRefIndex,
) {
    if message.author_is_self || message.author_is_bot || message.current_msg_idx.is_none() {
        return;
    }
    let inbound = respond.prepare_inbound(platform::qq_official::inbound_from_group(message));
    match ref_index.lock() {
        Ok(mut index) => index.insert_inbound(&inbound),
        Err(_) => warn!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            "group inbound ref_index observe skipped because index lock is poisoned"
        ),
    }
}

async fn send_group_respond_response(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    config: &AppConfig,
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    message: &GroupMessage,
    response: &RespondResponse,
    ref_index: &SharedRefIndex,
) -> anyhow::Result<()> {
    let capability = ReplyCapability::qq_official_group(config);
    let outbound = match render_respond_response_for_profile(response, &capability.render) {
        Some(outbound) => outbound,
        None => {
            warn!(
            message_id = %message.message_id,
            group = %mask_openid(&message.group_openid),
            fallback_reason = "empty_rendered_response",
            "respond backend produced no group reply text; sending local fallback"
            );
            OutboundMessage::Text {
                text: empty_group_reply_fallback_text(config.bot_display_name()),
            }
        }
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
    match send_group_outbound_chunked(&sender, &target, &outbound, &limits, |_, sent_ids| {
        record_group_bot_outbound_send(
            group_outbound_cache,
            ref_index,
            message,
            response,
            config,
            sent_ids,
            outbound.fallback_text(),
        );
    })
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

fn record_group_bot_outbound_send(
    group_outbound_cache: &Arc<Mutex<BotOutboundCache>>,
    ref_index: &SharedRefIndex,
    message: &GroupMessage,
    response: &RespondResponse,
    config: &AppConfig,
    sent_ids: &SendMessageIds,
    text: &str,
) {
    {
        let mut cache = group_outbound_cache.lock().unwrap();
        cache.insert(sent_ids.message_id.clone());
        cache.insert_ref_index_id(sent_ids.ref_index_id.clone());
    }
    let inbound = platform::qq_official::inbound_from_group(message);
    ref_index.lock().unwrap().insert_bot_outbound(
        platform::Platform::QqOfficial,
        config.app_id.as_deref(),
        &inbound.conversation,
        sent_ids.ref_index_lookup_id().map(str::to_owned),
        text,
        response.visible_entity_snapshot.clone(),
    );
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
                return Some(*response);
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

    #[test]
    fn local_group_hints_use_configured_bot_display_name() {
        assert_eq!(
            empty_group_reply_fallback_text("小助手"),
            "唔，小助手刚刚没整理出可用回复。可以再说一次。"
        );
        assert_eq!(
            group_cooldown_hint_text("小助手"),
            "哦哦，刚刚在处理上一条消息，稍后再说一声小助手就能继续了呢。"
        );
    }
    use crate::config::{
        AgentTypingConfig, DEFAULT_CONVERSATION_QUEUE_CAPACITY, DEFAULT_MARKDOWN_CHUNK_SOFT_LIMIT,
        DEFAULT_MAX_ACTIVE_CONVERSATION_WORKERS, DEFAULT_MEDIA_MAX_BYTES,
        DEFAULT_MESSAGE_AGGREGATION_MAX_ACTIVE_KEYS, DEFAULT_MESSAGE_AGGREGATION_MAX_CHARS,
        DEFAULT_MESSAGE_AGGREGATION_MAX_MESSAGES, DEFAULT_MESSAGE_AGGREGATION_MAX_WAIT_MS,
        DEFAULT_MESSAGE_AGGREGATION_QUIET_MS, DEFAULT_TEXT_CHUNK_SOFT_LIMIT, GroupMessageMode,
        MessageAggregationConfig,
    };
    use crate::{
        api::{ApiError, QqApiClient},
        auth::AccessTokenManager,
    };
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
            current_msg_idx: None,
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
            qq_official_enabled: true,
            app_id: Some("app".to_owned()),
            app_secret: Some("secret".to_owned()),
            bot_mention_ids: Vec::new(),
            sandbox: false,
            api_base: "http://127.0.0.1:1".to_owned(),
            token_refresh_margin: Duration::from_secs(60),
            enable_markdown: true,
            enable_image: false,
            enable_group_messages: true,
            verbose_log: false,
            member_detail_enrich_enabled: false,
            group_message_mode: GroupMessageMode::Mention,
            bot_display_name: "小女仆".to_owned(),
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
            onebot11: crate::config::OneBot11Config::default(),
        }
    }

    fn qq_group_capability() -> ReplyCapability {
        ReplyCapability::qq_official_group(&test_config())
    }

    struct MockCore {
        response: RespondResponse,
        respond_calls: Arc<AtomicUsize>,
        classify_calls: Arc<AtomicUsize>,
        immediate_inputs: Vec<String>,
    }

    #[async_trait::async_trait]
    impl CoreService for MockCore {
        async fn respond(&self, _request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
            self.respond_calls.fetch_add(1, Ordering::SeqCst);
            Ok(CoreRespondOutput::Complete(Box::new(self.response.clone())))
        }

        async fn classify_inbound(
            &self,
            request: CoreRequest,
        ) -> Result<CoreInboundClassification, CoreError> {
            self.classify_calls.fetch_add(1, Ordering::SeqCst);
            Ok(CoreInboundClassification {
                kind: if self
                    .immediate_inputs
                    .iter()
                    .any(|input| input == &request.text)
                {
                    CoreInboundKind::Immediate
                } else {
                    CoreInboundKind::NormalChat
                },
            })
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
        respond_client_with_counter(Arc::new(AtomicUsize::new(0)))
    }

    fn respond_client_with_counter(respond_calls: Arc<AtomicUsize>) -> RespondClient {
        respond_client_with_classification(respond_calls, Arc::new(AtomicUsize::new(0)), Vec::new())
    }

    fn respond_client_with_classification(
        respond_calls: Arc<AtomicUsize>,
        classify_calls: Arc<AtomicUsize>,
        immediate_inputs: Vec<&str>,
    ) -> RespondClient {
        RespondClient::new(Arc::new(MockCore {
            response: RespondResponse {
                output: None,
                handled: Some(true),
                session_id: None,
                command: None,
                diagnostics: None,
                visible_entity_snapshot: None,
            },
            respond_calls,
            classify_calls,
            immediate_inputs: immediate_inputs.into_iter().map(str::to_owned).collect(),
        }))
    }

    fn api_client() -> QqApiClient {
        QqApiClient::new(
            reqwest::Client::new(),
            "http://127.0.0.1:1",
            AccessTokenManager::new(
                reqwest::Client::new(),
                "app",
                "secret",
                Duration::from_secs(60),
            ),
        )
    }

    fn assert_group_send_error(err: anyhow::Error) {
        assert!(
            matches!(
                err.downcast_ref::<ApiError>(),
                Some(ApiError::Auth(_) | ApiError::Http(_) | ApiError::Status { .. })
            ),
            "expected QQ send/auth error from fake API endpoint, got: {err:#}"
        );
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

    fn unique_media_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "qq-maid-group-{name}-{}-{nanos}",
            std::process::id()
        ))
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
    fn group_at_respond_error_log_text_keeps_member_openid_out() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);
        let error = crate::respond::RespondError::Core(qq_maid_core::service::CoreError::new(
            "internal_error",
            "respond",
            "backend down",
        ));
        let capability = qq_group_capability();

        let (qq_text, log_text) = group_respond_error_texts(&message, &error, &capability);

        assert!(!qq_text.contains("member-1"));
        assert!(!qq_text.contains("<@"));
        assert!(!log_text.contains("member-1"));
        assert!(!log_text.contains("<@"));
    }

    #[test]
    fn group_at_reply_text_outbound_keeps_plain_text_without_openid_mention() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);
        let capability = qq_group_capability();
        let outbound = OutboundMessage::Text {
            text: "回复正文".to_owned(),
        };

        assert_eq!(
            prefix_group_reply_outbound(&message, outbound, &capability),
            OutboundMessage::Text {
                text: "回复正文".to_owned(),
            }
        );
    }

    #[test]
    fn group_at_reply_markdown_outbound_mentions_sender() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);
        let capability = qq_group_capability();
        let outbound = OutboundMessage::Markdown {
            markdown: crate::markdown::MarkdownPayload::new("**回复正文**"),
            fallback_text: "回复正文".to_owned(),
        };

        assert_eq!(
            prefix_group_reply_outbound(&message, outbound, &capability),
            OutboundMessage::Markdown {
                markdown: crate::markdown::MarkdownPayload::new("<@member-1>\n**回复正文**"),
                fallback_text: "<@member-1>\n回复正文".to_owned(),
            }
        );
    }

    #[test]
    fn structured_group_mention_markdown_reply_mentions_sender_like_at_event() {
        let mut message = group_message("hello", GroupEventType::GroupMessage);
        message.mentions = vec![crate::gateway::event::GroupMention {
            is_you: true,
            member_role: None,
            target_id: None,
        }];
        let capability = qq_group_capability();
        let outbound = OutboundMessage::Markdown {
            markdown: crate::markdown::MarkdownPayload::new("**回复正文**"),
            fallback_text: "回复正文".to_owned(),
        };

        assert_eq!(
            prefix_group_reply_outbound(&message, outbound, &capability),
            OutboundMessage::Markdown {
                markdown: crate::markdown::MarkdownPayload::new("<@member-1>\n**回复正文**"),
                fallback_text: "<@member-1>\n回复正文".to_owned(),
            }
        );
    }

    #[test]
    fn group_at_reply_respects_platform_mention_capability() {
        let message = group_message("hello", GroupEventType::GroupAtMessage);
        let mut capability = qq_group_capability();
        capability.supports_at_mention = false;
        let outbound = OutboundMessage::Markdown {
            markdown: crate::markdown::MarkdownPayload::new("**回复正文**"),
            fallback_text: "回复正文".to_owned(),
        };

        assert_eq!(
            prefix_group_reply_outbound(&message, outbound, &capability),
            OutboundMessage::Markdown {
                markdown: crate::markdown::MarkdownPayload::new("**回复正文**"),
                fallback_text: "回复正文".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn mode_policy_blocked_group_message_does_not_download_media() {
        let mut config = test_config();
        config.group_message_mode = GroupMessageMode::Off;
        config.media_dir = unique_media_dir("mode-policy");
        let (url, hits) = spawn_media_server().await;
        let message = media_message("group-off", "普通聊天", GroupEventType::GroupMessage, url);
        let ref_index = crate::gateway::ref_index::ref_index();

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
            &ref_index,
        )
        .await
        .unwrap();

        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert_eq!(media_file_count(&config.media_dir), 0);
    }

    #[tokio::test]
    async fn plain_group_message_observes_ref_index_before_mode_policy_without_core_call() {
        let config = test_config();
        let mut message = group_message("普通群友消息", GroupEventType::GroupMessage);
        message.message_id = "group-observed".to_owned();
        message.current_msg_idx = Some("REFIDX_user_observed".to_owned());
        let respond_calls = Arc::new(AtomicUsize::new(0));
        let ref_index = crate::gateway::ref_index::ref_index();

        handle_group_message(
            message,
            &config,
            &respond_client_with_counter(respond_calls.clone()),
            &api_client(),
            &crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60)),
            &Arc::new(Mutex::new(BotOutboundCache::default())),
            &Arc::new(Mutex::new(GroupCooldowns::default())),
            &bot_identity(),
            &GatewayRuntimeStatus::new(),
            &ref_index,
        )
        .await
        .unwrap();

        assert_eq!(respond_calls.load(Ordering::SeqCst), 0);

        let mut quoted = group_message("查看这条", GroupEventType::GroupAtMessage);
        quoted.message_id = "group-quote".to_owned();
        quoted.reply = Some(crate::gateway::event::MessageReply {
            message_id: "qq_reply_payload_id".to_owned(),
            ref_msg_idx: Some("REFIDX_user_observed".to_owned()),
            content: None,
            input_parts: Vec::new(),
            media_summaries: Vec::new(),
        });
        let mut inbound =
            respond_client().prepare_inbound(platform::qq_official::inbound_from_group(&quoted));
        ref_index.lock().unwrap().enrich_inbound(&mut inbound);

        let quoted_context = inbound.quoted.as_ref().unwrap();
        assert!(quoted_context.lookup_found);
        assert_eq!(quoted_context.text_summary.as_deref(), Some("普通群友消息"));
        assert_eq!(quoted_context.from_bot, Some(false));
    }

    #[tokio::test]
    async fn cooldown_and_dedupe_blocked_group_messages_do_not_download_media() {
        let mut config = test_config();
        config.group_message_mode = GroupMessageMode::Active;
        config.media_dir = unique_media_dir("cooldown");
        let outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let cooldowns = Arc::new(Mutex::new(GroupCooldowns::default()));
        let dedupe = crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60));
        let respond = respond_client();
        let api = api_client();
        let runtime = GatewayRuntimeStatus::new();
        let identity = bot_identity();
        let ref_index = crate::gateway::ref_index::ref_index();

        let (url_first, hits_first) = spawn_media_server().await;
        let first_err = handle_group_message(
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
            &ref_index,
        )
        .await
        .unwrap_err();
        assert_group_send_error(first_err);

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
            &ref_index,
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
            &ref_index,
        )
        .await
        .unwrap();

        assert_eq!(hits_third.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn normal_chat_mention_during_cooldown_skips_core_and_sends_hint() {
        // #386：用户明确 @ 机器人但在群冷却窗口内时，不能吞掉也不走 LLM，
        // 只发一条轻量提示。这里用 fake API endpoint 验证：第一条 @ 消息会调 Core
        // 并因发送失败报错；第二条 @ 消息在冷却窗口内，不调 Core、返回 Ok。
        let mut config = test_config();
        config.group_message_mode = GroupMessageMode::Mention;
        let outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let cooldowns = Arc::new(Mutex::new(GroupCooldowns::default()));
        let dedupe = crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60));
        let respond_calls = Arc::new(AtomicUsize::new(0));
        let respond = respond_client_with_counter(respond_calls.clone());
        let api = api_client();
        let runtime = GatewayRuntimeStatus::new();
        let identity = bot_identity();
        let ref_index = crate::gateway::ref_index::ref_index();

        let mut first = group_message("总结一下", GroupEventType::GroupMessage);
        first.message_id = "group-mention-1".to_owned();
        first.mentions = vec![crate::gateway::event::GroupMention {
            is_you: true,
            member_role: None,
            target_id: None,
        }];

        handle_group_message(
            first,
            &config,
            &respond,
            &api,
            &dedupe,
            &outbound_cache,
            &cooldowns,
            &identity,
            &runtime,
            &ref_index,
        )
        .await
        .unwrap_err();
        assert_eq!(respond_calls.load(Ordering::SeqCst), 1);

        let mut second = group_message("再总结一下", GroupEventType::GroupMessage);
        second.message_id = "group-mention-2".to_owned();
        second.mentions = vec![crate::gateway::event::GroupMention {
            is_you: true,
            member_role: None,
            target_id: None,
        }];

        handle_group_message(
            second,
            &config,
            &respond,
            &api,
            &dedupe,
            &outbound_cache,
            &cooldowns,
            &identity,
            &runtime,
            &ref_index,
        )
        .await
        .unwrap();

        // 冷却命中 + 明确指向机器人 = 不调 LLM，只发轻量提示。
        assert_eq!(respond_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn immediate_group_reply_bypasses_cooldown_without_sending_hint() {
        let mut config = test_config();
        config.group_message_mode = GroupMessageMode::Mention;
        let outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let cooldowns = Arc::new(Mutex::new(GroupCooldowns::default()));
        let dedupe = crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60));
        let respond_calls = Arc::new(AtomicUsize::new(0));
        let classify_calls = Arc::new(AtomicUsize::new(0));
        let respond = respond_client_with_classification(
            respond_calls.clone(),
            classify_calls.clone(),
            vec!["确认"],
        );
        let api = api_client();
        let runtime = GatewayRuntimeStatus::new();
        let identity = bot_identity();
        let ref_index = crate::gateway::ref_index::ref_index();

        let mut first = group_message("@小女仆 先处理这一条", GroupEventType::GroupMessage);
        first.message_id = "group-immediate-1".to_owned();
        first.mentions = vec![crate::gateway::event::GroupMention {
            is_you: true,
            member_role: None,
            target_id: None,
        }];
        let first_err = handle_group_message(
            first,
            &config,
            &respond,
            &api,
            &dedupe,
            &outbound_cache,
            &cooldowns,
            &identity,
            &runtime,
            &ref_index,
        )
        .await
        .unwrap_err();
        assert_group_send_error(first_err);

        let mut second = group_message("@小女仆 确认", GroupEventType::GroupMessage);
        second.message_id = "group-immediate-2".to_owned();
        second.mentions = vec![crate::gateway::event::GroupMention {
            is_you: true,
            member_role: None,
            target_id: None,
        }];
        let second_err = handle_group_message(
            second,
            &config,
            &respond,
            &api,
            &dedupe,
            &outbound_cache,
            &cooldowns,
            &identity,
            &runtime,
            &ref_index,
        )
        .await
        .unwrap_err();
        assert_group_send_error(second_err);

        // 第二条仍在冷却窗口内，但规范化正文被 Core 判为 Immediate，因此继续进入
        // respond；若错误地走 cooldown hint，处理器会吞掉发送错误并返回 Ok。
        assert_eq!(classify_calls.load(Ordering::SeqCst), 2);
        assert_eq!(respond_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn immediate_quoted_group_reply_bypasses_cooldown() {
        let mut config = test_config();
        config.group_message_mode = GroupMessageMode::Mention;
        let outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        outbound_cache
            .lock()
            .unwrap()
            .insert(Some("bot-pending-message".to_owned()));
        let cooldowns = Arc::new(Mutex::new(GroupCooldowns::default()));
        let dedupe = crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60));
        let respond_calls = Arc::new(AtomicUsize::new(0));
        let classify_calls = Arc::new(AtomicUsize::new(0));
        let respond = respond_client_with_classification(
            respond_calls.clone(),
            classify_calls.clone(),
            vec!["确认"],
        );
        let api = api_client();
        let runtime = GatewayRuntimeStatus::new();
        let identity = bot_identity();
        let ref_index = crate::gateway::ref_index::ref_index();

        let mut first = group_message("@小女仆 先处理这一条", GroupEventType::GroupMessage);
        first.message_id = "group-quoted-immediate-1".to_owned();
        first.mentions = vec![crate::gateway::event::GroupMention {
            is_you: true,
            member_role: None,
            target_id: None,
        }];
        let first_err = handle_group_message(
            first,
            &config,
            &respond,
            &api,
            &dedupe,
            &outbound_cache,
            &cooldowns,
            &identity,
            &runtime,
            &ref_index,
        )
        .await
        .unwrap_err();
        assert_group_send_error(first_err);

        let mut second = group_message("确认", GroupEventType::GroupMessage);
        second.message_id = "group-quoted-immediate-2".to_owned();
        second.reply = Some(crate::gateway::event::MessageReply {
            message_id: "bot-pending-message".to_owned(),
            ref_msg_idx: None,
            content: Some("待删除：待确认删除的群记忆".to_owned()),
            input_parts: Vec::new(),
            media_summaries: Vec::new(),
        });
        let second_err = handle_group_message(
            second,
            &config,
            &respond,
            &api,
            &dedupe,
            &outbound_cache,
            &cooldowns,
            &identity,
            &runtime,
            &ref_index,
        )
        .await
        .unwrap_err();
        assert_group_send_error(second_err);

        assert_eq!(classify_calls.load(Ordering::SeqCst), 2);
        assert_eq!(respond_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn processed_group_message_downloads_media_after_filters() {
        let mut config = test_config();
        config.group_message_mode = GroupMessageMode::Active;
        config.media_dir = unique_media_dir("download");
        let (url, hits) = spawn_media_server().await;
        let message = media_message(
            "group-download",
            "小女仆 看图",
            GroupEventType::GroupMessage,
            url,
        );
        let ref_index = crate::gateway::ref_index::ref_index();

        let err = handle_group_message(
            message,
            &config,
            &respond_client(),
            &api_client(),
            &crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60)),
            &Arc::new(Mutex::new(BotOutboundCache::default())),
            &Arc::new(Mutex::new(GroupCooldowns::default())),
            &bot_identity(),
            &GatewayRuntimeStatus::new(),
            &ref_index,
        )
        .await
        .unwrap_err();
        assert_group_send_error(err);

        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(media_file_count(&config.media_dir), 1);
    }

    #[test]
    fn group_send_records_message_id_for_cache_and_refidx_for_ref_index() {
        let config = test_config();
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let ref_index = crate::gateway::ref_index::ref_index();
        let message = group_message("小女仆 你好", GroupEventType::GroupMessage);
        let response = RespondResponse {
            output: Some(qq_maid_core::service::AssistantOutput::markdown(
                "机器人回复",
                "机器人回复",
            )),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };
        let sent_ids = SendMessageIds {
            message_id: Some("qq_msg_1".to_owned()),
            ref_index_id: Some("REFIDX_1".to_owned()),
        };

        record_group_bot_outbound_send(
            &cache,
            &ref_index,
            &message,
            &response,
            &config,
            &sent_ids,
            "机器人回复",
        );

        assert!(cache.lock().unwrap().contains("qq_msg_1"));
        assert!(!cache.lock().unwrap().contains("REFIDX_1"));
        assert!(cache.lock().unwrap().contains_ref_index_id("REFIDX_1"));

        let mut quoted = group_message("继续", GroupEventType::GroupMessage);
        quoted.reply = Some(crate::gateway::event::MessageReply {
            message_id: "qq_reply_payload_id".to_owned(),
            ref_msg_idx: Some("REFIDX_1".to_owned()),
            content: None,
            input_parts: Vec::new(),
            media_summaries: Vec::new(),
        });
        assert!(should_process_group_message(
            crate::config::GroupMessageMode::Mention,
            &[],
            &quoted,
            &quoted.content,
            &bot_identity(),
            &cache
        ));

        let mut inbound = platform::qq_official::inbound_from_group(&quoted);
        inbound.account_id = config.app_id.clone();
        ref_index.lock().unwrap().enrich_inbound(&mut inbound);
        let quoted_context = inbound.quoted.as_ref().unwrap();
        assert!(quoted_context.lookup_found);
        assert_eq!(quoted_context.text_summary.as_deref(), Some("机器人回复"));
        assert_eq!(quoted_context.from_bot, Some(true));
    }

    #[test]
    fn group_send_records_rendered_fallback_when_output_text_field_is_empty() {
        let config = test_config();
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let ref_index = crate::gateway::ref_index::ref_index();
        let message = group_message("小女仆 看图", GroupEventType::GroupMessage);
        let response = RespondResponse {
            output: Some(qq_maid_core::service::AssistantOutput {
                text_fallback: String::new(),
                markdown: None,
                parts: vec![qq_maid_core::service::OutputPart::Image {
                    media: qq_maid_core::service::OutputMedia {
                        fallback_text: Some("图片：天气雷达".to_owned()),
                        ..qq_maid_core::service::OutputMedia::default()
                    },
                }],
            }),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };

        record_group_bot_outbound_send(
            &cache,
            &ref_index,
            &message,
            &response,
            &config,
            &SendMessageIds {
                message_id: Some("qq_msg_1".to_owned()),
                ref_index_id: Some("REFIDX_rendered".to_owned()),
            },
            "图片：天气雷达",
        );

        let mut quoted = group_message("继续", GroupEventType::GroupMessage);
        quoted.reply = Some(crate::gateway::event::MessageReply {
            message_id: "qq_reply_payload_id".to_owned(),
            ref_msg_idx: Some("REFIDX_rendered".to_owned()),
            content: None,
            input_parts: Vec::new(),
            media_summaries: Vec::new(),
        });
        let mut inbound = platform::qq_official::inbound_from_group(&quoted);
        inbound.account_id = config.app_id.clone();
        ref_index.lock().unwrap().enrich_inbound(&mut inbound);

        let quoted_context = inbound.quoted.as_ref().unwrap();
        assert!(quoted_context.lookup_found);
        assert_eq!(
            quoted_context.text_summary.as_deref(),
            Some("图片：天气雷达")
        );
    }

    #[test]
    fn group_send_does_not_cross_use_message_id_and_refidx_when_one_is_missing() {
        let config = test_config();
        let response = RespondResponse {
            output: Some(qq_maid_core::service::AssistantOutput::text("机器人回复")),
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
            visible_entity_snapshot: None,
        };

        let message_only_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let message_only_index = crate::gateway::ref_index::ref_index();
        let message = group_message("小女仆 你好", GroupEventType::GroupMessage);
        record_group_bot_outbound_send(
            &message_only_cache,
            &message_only_index,
            &message,
            &response,
            &config,
            &SendMessageIds {
                message_id: Some("qq_msg_only".to_owned()),
                ref_index_id: None,
            },
            "机器人回复",
        );
        assert!(message_only_cache.lock().unwrap().contains("qq_msg_only"));
        assert!(
            !message_only_cache
                .lock()
                .unwrap()
                .contains_ref_index_id("qq_msg_only")
        );
        let mut message_only_quote = platform::qq_official::inbound_from_group(&message);
        message_only_quote.account_id = config.app_id.clone();
        message_only_quote.quoted = Some(qq_maid_common::input_part::QuotedMessageContext {
            ref_msg_idx: Some("qq_msg_only".to_owned()),
            ..Default::default()
        });
        message_only_index
            .lock()
            .unwrap()
            .enrich_inbound(&mut message_only_quote);
        assert!(!message_only_quote.quoted.as_ref().unwrap().lookup_found);

        let refidx_only_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let refidx_only_index = crate::gateway::ref_index::ref_index();
        record_group_bot_outbound_send(
            &refidx_only_cache,
            &refidx_only_index,
            &message,
            &response,
            &config,
            &SendMessageIds {
                message_id: None,
                ref_index_id: Some("REFIDX_only".to_owned()),
            },
            "机器人回复",
        );
        assert!(!refidx_only_cache.lock().unwrap().contains("REFIDX_only"));
        assert!(
            refidx_only_cache
                .lock()
                .unwrap()
                .contains_ref_index_id("REFIDX_only")
        );
    }
}

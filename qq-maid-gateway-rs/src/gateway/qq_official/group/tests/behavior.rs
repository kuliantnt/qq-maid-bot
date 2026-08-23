use super::*;
mod outbound;
#[test]
fn group_at_respond_error_mentions_sender_but_log_text_keeps_member_openid_out() {
    let message = group_message("hello", GroupEventType::GroupAtMessage);
    let error = crate::respond::RespondError::Core(qq_maid_core::service::CoreError::new(
        "internal_error",
        "respond",
        "backend down",
    ));
    let capability = qq_group_capability();

    let (outbound, log_text) = group_respond_error_outbound(&message, &error, &capability);

    assert_eq!(
        outbound,
        OutboundMessage::Markdown {
            markdown: crate::markdown::MarkdownPayload::new("<@member-1>\n处理失败：backend down"),
            fallback_text: "处理失败：backend down".to_owned(),
        }
    );
    assert!(!log_text.contains("member-1"));
    assert!(!log_text.contains("<@"));
}

#[test]
fn group_at_reply_text_outbound_forces_markdown_mention_when_render_disabled() {
    let message = group_message("hello", GroupEventType::GroupAtMessage);
    let mut capability = qq_group_capability();
    capability.render.supports_markdown = false;
    let outbound = OutboundMessage::Text {
        text: "回复正文".to_owned(),
    };

    assert_eq!(
        prefix_group_reply_outbound(&message, outbound, &capability),
        OutboundMessage::Markdown {
            markdown: crate::markdown::MarkdownPayload::new("<@member-1>\n回复正文"),
            fallback_text: "回复正文".to_owned(),
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
            fallback_text: "回复正文".to_owned(),
        }
    );
}

#[test]
fn group_without_bot_mention_also_mentions_sender() {
    let message = group_message("/r d20", GroupEventType::GroupMessage);
    let capability = qq_group_capability();
    let outbound = OutboundMessage::Text {
        text: "🎲 掷出了 12 / 20".to_owned(),
    };

    assert_eq!(
        prefix_group_reply_outbound(&message, outbound, &capability),
        OutboundMessage::Markdown {
            markdown: crate::markdown::MarkdownPayload::new("<@member-1>\n🎲 掷出了 12 / 20"),
            fallback_text: "🎲 掷出了 12 / 20".to_owned(),
        }
    );
}

#[test]
fn structured_group_mention_markdown_reply_mentions_sender_like_at_event() {
    let mut message = group_message("hello", GroupEventType::GroupMessage);
    message.mentions = vec![crate::gateway::event::GroupMention {
        is_current_bot: true,
        member_role: None,
        target_id: Some("app".to_owned()),
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
            fallback_text: "回复正文".to_owned(),
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
    let mut message = media_message("group-off", "普通聊天", GroupEventType::GroupMessage, url);
    message.current_msg_idx = Some("REFIDX_ignored_image".to_owned());
    let ref_index = crate::gateway::ref_index::ref_index();

    handle_group_message_for_test(
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

    // 被动观察允许保留媒体摘要，但索引中不得保存 QQ 临时 URL。
    let mut quoted = group_message("查看图片", GroupEventType::GroupAtMessage);
    quoted.reply = Some(crate::gateway::event::MessageReply {
        message_id: "quoted-ignored-image".to_owned(),
        ref_msg_idx: Some("REFIDX_ignored_image".to_owned()),
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });
    let mut inbound =
        respond_client().prepare_inbound(platform::qq_official::inbound_from_group(&quoted));
    ref_index.lock().unwrap().enrich_inbound(&mut inbound);
    let media = inbound.quoted.as_ref().unwrap().input_parts[1]
        .media()
        .expect("passively observed image");
    assert_eq!(media.url, None);
    assert_eq!(media.local_path, None);
    assert_eq!(media.status, MediaStatus::MissingReadableUrl);
}

#[tokio::test]
async fn plain_group_message_ignored_by_mode_policy_remains_quotable() {
    // mode policy 忽略的普通群消息不进入 Core，但仍应轻量写入 RefIndex，
    // 否则后续 @机器人引用这条消息时会产生无法恢复的 miss。
    let config = test_config();
    let mut message = group_message("普通群友消息", GroupEventType::GroupMessage);
    message.message_id = "group-observed".to_owned();
    message.current_msg_idx = Some("REFIDX_user_observed".to_owned());
    let respond_calls = Arc::new(AtomicUsize::new(0));
    let ref_index = crate::gateway::ref_index::ref_index();

    handle_group_message_for_test(
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

    // mode policy 忽略后不调用 Core。
    assert_eq!(respond_calls.load(Ordering::SeqCst), 0);

    // 后续引用能够恢复被忽略消息的标准化正文。
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
    assert_eq!(quoted_context.fallback_reason, None);
    assert_eq!(
        quoted_context
            .input_parts
            .iter()
            .filter_map(MessageInputPart::text_content)
            .collect::<Vec<_>>(),
        vec!["普通群友消息"]
    );
}

#[tokio::test]
async fn quoted_downloaded_image_survives_passive_ref_index_hit() {
    let mut config = test_config();
    config.group_message_mode = GroupMessageMode::Off;
    config.media_dir = unique_media_dir("passive-quote-download");
    let (url, hits) = spawn_media_server().await;
    let mut original = media_message(
        "group-passive-image",
        "被忽略的图片",
        GroupEventType::GroupMessage,
        url.clone(),
    );
    original.current_msg_idx = Some("REFIDX_passive_image".to_owned());
    let ref_index = crate::gateway::ref_index::ref_index();

    handle_group_message_for_test(
        original,
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

    let mut quoted = group_message("查看这张图", GroupEventType::GroupAtMessage);
    quoted.message_id = "group-quote-passive-image".to_owned();
    quoted.reply = Some(crate::gateway::event::MessageReply {
        message_id: "quoted-passive-image".to_owned(),
        ref_msg_idx: Some("REFIDX_passive_image".to_owned()),
        content: Some("payload 展示正文".to_owned()),
        input_parts: vec![
            MessageInputPart::text("payload 展示正文"),
            MessageInputPart::image(MessageMedia {
                mime_type: Some("image/jpeg".to_owned()),
                filename: Some("a.jpg".to_owned()),
                url: Some(url),
                status: MediaStatus::Available,
                ..Default::default()
            }),
        ],
        media_summaries: Vec::new(),
    });
    let media_context = MediaFetchContext {
        platform: "qq_official",
        app_id: config.app_id.clone().unwrap(),
        peer_id: quoted.group_openid.clone(),
        root_dir: config.media_dir.clone(),
        timeout: config.media_download_timeout,
        max_bytes: config.media_max_bytes,
    };
    fetch_qq_official_quoted_images(
        &qq_maid_common::http_client::client(),
        &media_context,
        &quoted.message_id,
        quoted.reply.as_mut(),
    )
    .await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let mut inbound =
        respond_client().prepare_inbound(crate::respond::normalized_group_inbound_with_prefix(
            &quoted,
            &config.group_active_keywords,
            config.command_prefix,
        ));
    ref_index.lock().unwrap().enrich_inbound(&mut inbound);

    let quoted_context = inbound.quoted.as_ref().unwrap();
    assert_eq!(quoted_context.text_summary.as_deref(), Some("被忽略的图片"));
    assert_eq!(
        quoted_context.input_parts[0].text_content(),
        Some("被忽略的图片")
    );
    let media = quoted_context.input_parts[1]
        .media()
        .expect("downloaded quoted image");
    let local_path = media.local_path.as_deref().expect("downloaded local path");
    assert!(std::path::Path::new(local_path).is_file());
    assert_eq!(media.status, MediaStatus::Available);
    assert_eq!(media.url, None);
}

struct GroupHandlerHarness {
    config: AppConfig,
    respond: RespondClient,
    api: QqApiClient,
    dedupe: crate::gateway::dedupe::MessageDedupe,
    outbound_cache: Arc<Mutex<BotOutboundCache>>,
    cooldowns: Arc<Mutex<GroupCooldowns>>,
    identity: crate::gateway::bot_identity::SharedBotIdentity,
    runtime: GatewayRuntimeStatus,
    ref_index: crate::gateway::ref_index::SharedRefIndex,
    respond_calls: Arc<AtomicUsize>,
}

impl GroupHandlerHarness {
    fn new(mode: GroupMessageMode) -> Self {
        let mut config = test_config();
        config.group_message_mode = mode;
        let respond_calls = Arc::new(AtomicUsize::new(0));
        Self {
            config,
            respond: respond_client_with_counter(respond_calls.clone()),
            api: api_client(),
            dedupe: crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60)),
            outbound_cache: Arc::new(Mutex::new(BotOutboundCache::default())),
            cooldowns: Arc::new(Mutex::new(GroupCooldowns::default())),
            identity: bot_identity(),
            runtime: GatewayRuntimeStatus::new(),
            ref_index: crate::gateway::ref_index::ref_index(),
            respond_calls,
        }
    }

    async fn handle(&self, message: GroupMessage) -> anyhow::Result<()> {
        handle_group_message_for_test(
            message,
            &self.config,
            &self.respond,
            &self.api,
            &self.dedupe,
            &self.outbound_cache,
            &self.cooldowns,
            &self.identity,
            &self.runtime,
            &self.ref_index,
        )
        .await
    }
}

#[tokio::test]
async fn raw_like_group_mention_uses_target_id_and_rejects_other_bot() {
    let harness = GroupHandlerHarness::new(GroupMessageMode::Mention);

    // 贴近 QQ raw event：解析层保留平台结构化身份，handler 根据 target_id 和官方标记归一化。
    let mut current_bot = group_message("请帮我看看", GroupEventType::GroupMessage);
    current_bot.message_id = "raw-like-current-bot".to_owned();
    current_bot.mentions = vec![crate::gateway::event::GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: Some("app".to_owned()),
    }];
    assert_group_send_error(harness.handle(current_bot).await.unwrap_err());
    assert_eq!(harness.respond_calls.load(Ordering::SeqCst), 1);

    let mut other_bot = group_message("请帮我看看", GroupEventType::GroupMessage);
    other_bot.message_id = "raw-like-other-bot".to_owned();
    other_bot.mentions = vec![crate::gateway::event::GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: Some("other-bot".to_owned()),
    }];
    harness.handle(other_bot).await.unwrap();

    let mut ordinary = group_message("请帮我看看", GroupEventType::GroupMessage);
    ordinary.message_id = "raw-like-ordinary".to_owned();
    harness.handle(ordinary).await.unwrap();

    assert_eq!(harness.respond_calls.load(Ordering::SeqCst), 1);

    let unresolved_harness = GroupHandlerHarness::new(GroupMessageMode::Mention);
    let mut official_mark = group_message(
        "[@汐雨](mqqapi://markdown/mention?at_type=1&at_tinyid=unresolved-target) 原始数据",
        GroupEventType::GroupMessage,
    );
    official_mark.message_id = "raw-like-official-mark".to_owned();
    official_mark.mentions = vec![crate::gateway::event::GroupMention {
        is_current_bot: true,
        member_role: None,
        target_id: Some("unresolved-target".to_owned()),
    }];
    assert_group_send_error(unresolved_harness.handle(official_mark).await.unwrap_err());
    assert_eq!(unresolved_harness.respond_calls.load(Ordering::SeqCst), 1);

    // QQ 专门的 GROUP_AT_MESSAGE_CREATE 事件本身就是当前机器人被 @，不能因 mention
    // 的 target_id 缺失或暂时无法匹配而丢弃整条事件。
    let at_harness = GroupHandlerHarness::new(GroupMessageMode::Mention);
    let mut at_event = group_message("请帮我看看", GroupEventType::GroupAtMessage);
    at_event.message_id = "raw-like-at-event".to_owned();
    at_event.mentions = vec![crate::gateway::event::GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: Some("unresolved-target".to_owned()),
    }];
    assert_group_send_error(at_harness.handle(at_event).await.unwrap_err());
    assert_eq!(at_harness.respond_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn active_wake_word_survives_normalization_and_dedupes_combined_trigger() {
    let harness = GroupHandlerHarness::new(GroupMessageMode::Active);

    // 唤醒词被剥离后正文仍可能为空，消息必须先通过空内容过滤，再由 active 策略命中。
    let mut wake_only = group_message("小女仆", GroupEventType::GroupMessage);
    wake_only.message_id = "wake-only".to_owned();
    assert_group_send_error(harness.handle(wake_only).await.unwrap_err());
    assert_eq!(harness.respond_calls.load(Ordering::SeqCst), 1);

    // @ 与唤醒词同时存在时仍只走一次 handler；重复投递由复合去重键拦截。
    let combined_harness = GroupHandlerHarness::new(GroupMessageMode::Active);
    let mut combined = group_message("小女仆 请继续", GroupEventType::GroupMessage);
    combined.message_id = "wake-and-mention".to_owned();
    combined.mentions = vec![crate::gateway::event::GroupMention {
        is_current_bot: false,
        member_role: None,
        target_id: Some("app".to_owned()),
    }];
    assert_group_send_error(combined_harness.handle(combined.clone()).await.unwrap_err());
    combined_harness.handle(combined).await.unwrap();

    assert_eq!(combined_harness.respond_calls.load(Ordering::SeqCst), 1);
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
    let first_err = handle_group_message_for_test(
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
    handle_group_message_for_test(
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
    handle_group_message_for_test(
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
async fn slash_candidates_reach_core_and_explicit_suppression_sends_nothing() {
    let mut config = test_config();
    config.group_message_mode = GroupMessageMode::Active;
    let outbound_cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let cooldowns = Arc::new(Mutex::new(GroupCooldowns::default()));
    let dedupe = crate::gateway::dedupe::MessageDedupe::new(Duration::from_secs(60));
    let respond_calls = Arc::new(AtomicUsize::new(0));
    let classify_calls = Arc::new(AtomicUsize::new(0));
    let respond = respond_client_with_response(
        respond_calls.clone(),
        classify_calls.clone(),
        vec!["/help"],
        CoreResponse {
            output: None,
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: Some(serde_json::json!({
                "suppressed": true,
                "reason": "test_gateway_suppressed_response",
            })),
            visible_entity_snapshot: None,
            delivery_hint: None,
        },
    );
    let api = api_client();
    let runtime = GatewayRuntimeStatus::new();
    let identity = bot_identity();
    let ref_index = crate::gateway::ref_index::ref_index();

    let mut direct = group_message("/help", GroupEventType::GroupMessage);
    direct.message_id = "group-direct-command".to_owned();
    handle_group_message_for_test(
        direct,
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

    let mut mentioned = group_message(
        "[@汐雨](mqqapi://markdown/mention?at_type=1&at_tinyid=app) /help",
        GroupEventType::GroupMessage,
    );
    mentioned.message_id = "group-mentioned-command".to_owned();
    mentioned.mentions = vec![crate::gateway::event::GroupMention {
        is_current_bot: true,
        member_role: None,
        target_id: Some("app".to_owned()),
    }];
    handle_group_message_for_test(
        mentioned,
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

    // 测试 API 地址不可达；两次调用均成功返回，证明显式 suppressed 响应未进入发送链路。
    let mut ordinary = group_message("路过", GroupEventType::GroupMessage);
    ordinary.message_id = "group-unwoken-ordinary".to_owned();
    handle_group_message_for_test(
        ordinary,
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

    assert_eq!(classify_calls.load(Ordering::SeqCst), 2);
    assert_eq!(respond_calls.load(Ordering::SeqCst), 2);
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
        is_current_bot: true,
        member_role: None,
        target_id: Some("app".to_owned()),
    }];

    handle_group_message_for_test(
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
        is_current_bot: true,
        member_role: None,
        target_id: Some("app".to_owned()),
    }];

    handle_group_message_for_test(
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
        is_current_bot: true,
        member_role: None,
        target_id: Some("app".to_owned()),
    }];
    let first_err = handle_group_message_for_test(
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
        is_current_bot: true,
        member_role: None,
        target_id: Some("app".to_owned()),
    }];
    let second_err = handle_group_message_for_test(
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
        is_current_bot: true,
        member_role: None,
        target_id: Some("app".to_owned()),
    }];
    let first_err = handle_group_message_for_test(
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
    let second_err = handle_group_message_for_test(
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

    let err = handle_group_message_for_test(
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

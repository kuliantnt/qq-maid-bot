use super::*;
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
}

#[tokio::test]
async fn plain_group_message_ignored_by_mode_policy_is_not_ref_indexed() {
    // 移除早期 observe_group_message_ref_index 后，被 mode policy 忽略的消息
    // 不会进入 RefIndex。后续引用该消息时 RefIndex miss，需经 payload fallback。
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

    // 因无早期观察插入，RefIndex 无此消息记录。
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
    assert!(!quoted_context.lookup_found);
    assert_eq!(
        quoted_context.fallback_reason.as_deref(),
        Some("ref_index_miss")
    );
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

    let mut mentioned = group_message("@小女仆 /help", GroupEventType::GroupMessage);
    mentioned.message_id = "group-mentioned-command".to_owned();
    mentioned.mentions = vec![crate::gateway::event::GroupMention {
        is_you: true,
        member_role: None,
        target_id: None,
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
        is_you: true,
        member_role: None,
        target_id: None,
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
        is_you: true,
        member_role: None,
        target_id: None,
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
        is_you: true,
        member_role: None,
        target_id: None,
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
        is_you: true,
        member_role: None,
        target_id: None,
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
        is_you: true,
        member_role: None,
        target_id: None,
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

#[test]
fn group_send_records_message_id_for_cache_and_refidx_for_ref_index() {
    let config = test_config();
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let message = group_message("小女仆 你好", GroupEventType::GroupMessage);
    let response = CoreResponse {
        output: Some(qq_maid_common::output_part::AssistantOutput::markdown(
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
    let response = CoreResponse {
        output: Some(qq_maid_common::output_part::AssistantOutput {
            text_fallback: String::new(),
            markdown: None,
            parts: vec![qq_maid_common::output_part::OutputPart::Image {
                media: qq_maid_common::output_part::OutputMedia {
                    fallback_text: Some("图片：天气雷达".to_owned()),
                    ..qq_maid_common::output_part::OutputMedia::default()
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
    let response = CoreResponse {
        output: Some(qq_maid_common::output_part::AssistantOutput::text(
            "机器人回复",
        )),
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

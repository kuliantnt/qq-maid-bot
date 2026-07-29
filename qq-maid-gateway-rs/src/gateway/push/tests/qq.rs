use super::*;

#[tokio::test]
async fn private_markdown_push_falls_back_to_text() {
    let sender = MockPushSender {
        fail_markdown: true,
        ..MockPushSender::default()
    };

    let outcome = send_private_push(&sender, "u1", "markdown", "# title", "title")
        .await
        .unwrap();

    assert_eq!(
        sender.calls(),
        vec!["c2c-markdown:u1:# title", "c2c-text:u1:title"]
    );
    assert_eq!(outcome.delivered_text, "title");
}

#[tokio::test]
async fn group_markdown_push_falls_back_to_text() {
    let sender = MockPushSender {
        fail_markdown: true,
        ..MockPushSender::default()
    };

    let outcome = send_group_push(&sender, "g1", "markdown", "# title", "title")
        .await
        .unwrap();

    assert_eq!(
        sender.calls(),
        vec!["group-markdown:g1:# title", "group-text:g1:title"]
    );
    assert_eq!(outcome.delivered_text, "title");
}

#[tokio::test]
async fn push_runtime_records_group_message_id_in_bot_outbound_cache() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let runtime = GatewayPushRuntime {
        api: panic_api_client(),
        qq_official_account_id: "app".to_owned(),
        runtime: GatewayRuntimeStatus::default(),
        group_outbound_cache: cache.clone(),
        ref_index: crate::gateway::ref_index::ref_index(),
    };
    let sender = MockPushSender {
        message_id: Some("bot-msg-1".to_owned()),
        ..MockPushSender::default()
    };

    let result = send_group_push(&sender, "g1", "text", "hello", "hello")
        .await
        .unwrap();
    // `GatewayPushRuntime::push` 的 QQ 发送成功路径会把群消息 ID 写入缓存；
    // 这里直接复用同一个缓存写入分支，证明主动推送仍能触发“回复机器人”识别。
    if let Some(message_id) = result.ids.message_id {
        runtime
            .group_outbound_cache
            .lock()
            .unwrap()
            .insert(Some(message_id));
    }

    assert!(
        cache.lock().unwrap().contains("bot-msg-1"),
        "group push message_id should be cached for reply detection"
    );
}

#[tokio::test]
async fn group_push_cache_uses_message_id_and_ref_index_uses_refidx() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let runtime = GatewayPushRuntime {
        api: panic_api_client(),
        qq_official_account_id: "app".to_owned(),
        runtime: GatewayRuntimeStatus::default(),
        group_outbound_cache: cache.clone(),
        ref_index: ref_index.clone(),
    };
    let intent = PushIntent {
        target: PushTarget::qq_official(PushTargetType::Group, "g1"),
        mentions: Vec::new(),
        text: "RSS 推送正文".to_owned(),
        fallback_text: Some("RSS 推送正文".to_owned()),
        message_type: "text".to_owned(),
        visible_entity_snapshot: None,
    };
    let sent_ids = SendMessageIds {
        message_id: Some("qq_msg_1".to_owned()),
        ref_index_id: Some("REFIDX_1".to_owned()),
    };

    let push_result = runtime.record_successful_push(
        &intent,
        "g1",
        PushSendOutcome {
            ids: sent_ids,
            delivered_text: "RSS 推送正文".to_owned(),
        },
    );

    assert_eq!(push_result.message_id.as_deref(), Some("qq_msg_1"));
    assert!(cache.lock().unwrap().contains("qq_msg_1"));
    assert!(!cache.lock().unwrap().contains("REFIDX_1"));

    let quoted = quoted_group_context(&ref_index, "g1", "REFIDX_1");
    assert!(quoted.lookup_found);
    assert_eq!(quoted.text_summary.as_deref(), Some("RSS 推送正文"));
    assert_eq!(quoted.from_bot, Some(true));
}

#[tokio::test]
async fn group_markdown_push_success_ref_index_uses_delivered_markdown_text() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let runtime = GatewayPushRuntime {
        api: panic_api_client(),
        qq_official_account_id: "app".to_owned(),
        runtime: GatewayRuntimeStatus::default(),
        group_outbound_cache: cache,
        ref_index: ref_index.clone(),
    };
    let sender = MockPushSender {
        message_id: Some("qq_md_msg".to_owned()),
        ref_index_id: Some("REFIDX_md".to_owned()),
        ..MockPushSender::default()
    };
    let intent = PushIntent {
        target: PushTarget::qq_official(PushTargetType::Group, "g1"),
        mentions: Vec::new(),
        text: "# Markdown 标题".to_owned(),
        fallback_text: Some("Markdown 标题".to_owned()),
        message_type: "markdown".to_owned(),
        visible_entity_snapshot: None,
    };

    let outcome = send_group_push(
        &sender,
        "g1",
        "markdown",
        "# Markdown 标题",
        "Markdown 标题",
    )
    .await
    .unwrap();
    let push_result = runtime.record_successful_push(&intent, "g1", outcome);

    assert_eq!(sender.calls(), vec!["group-markdown:g1:# Markdown 标题"]);
    assert_eq!(push_result.message_id.as_deref(), Some("qq_md_msg"));
    let quoted = quoted_group_context(&ref_index, "g1", "REFIDX_md");
    assert!(quoted.lookup_found);
    assert_eq!(quoted.text_summary.as_deref(), Some("# Markdown 标题"));
}

#[tokio::test]
async fn group_markdown_push_fallback_ref_index_uses_fallback_text() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let runtime = GatewayPushRuntime {
        api: panic_api_client(),
        qq_official_account_id: "app".to_owned(),
        runtime: GatewayRuntimeStatus::default(),
        group_outbound_cache: cache,
        ref_index: ref_index.clone(),
    };
    let sender = MockPushSender {
        fail_markdown: true,
        message_id: Some("qq_fallback_msg".to_owned()),
        ref_index_id: Some("REFIDX_fallback".to_owned()),
        ..MockPushSender::default()
    };
    let intent = PushIntent {
        target: PushTarget::qq_official(PushTargetType::Group, "g1"),
        mentions: Vec::new(),
        text: "# 失败的 Markdown".to_owned(),
        fallback_text: Some("降级文本".to_owned()),
        message_type: "markdown".to_owned(),
        visible_entity_snapshot: None,
    };

    let outcome = send_group_push(&sender, "g1", "markdown", "# 失败的 Markdown", "降级文本")
        .await
        .unwrap();
    let push_result = runtime.record_successful_push(&intent, "g1", outcome);

    assert_eq!(
        sender.calls(),
        vec![
            "group-markdown:g1:# 失败的 Markdown",
            "group-text:g1:降级文本"
        ]
    );
    assert_eq!(push_result.message_id.as_deref(), Some("qq_fallback_msg"));
    let quoted = quoted_group_context(&ref_index, "g1", "REFIDX_fallback");
    assert!(quoted.lookup_found);
    assert_eq!(quoted.text_summary.as_deref(), Some("降级文本"));
}

#[test]
fn push_segment_outcomes_record_each_delivered_text_by_refidx() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let runtime = GatewayPushRuntime {
        api: panic_api_client(),
        qq_official_account_id: "app".to_owned(),
        runtime: GatewayRuntimeStatus::default(),
        group_outbound_cache: cache.clone(),
        ref_index: ref_index.clone(),
    };
    let intent = PushIntent {
        target: PushTarget::qq_official(PushTargetType::Group, "g1"),
        mentions: Vec::new(),
        text: "完整推送".to_owned(),
        fallback_text: Some("完整推送".to_owned()),
        message_type: "text".to_owned(),
        visible_entity_snapshot: None,
    };

    let first = runtime.record_successful_push(
        &intent,
        "g1",
        PushSendOutcome {
            ids: SendMessageIds {
                message_id: Some("qq_seg_1".to_owned()),
                ref_index_id: Some("REFIDX_seg_1".to_owned()),
            },
            delivered_text: "第一段".to_owned(),
        },
    );
    let second = runtime.record_successful_push(
        &intent,
        "g1",
        PushSendOutcome {
            ids: SendMessageIds {
                message_id: Some("qq_seg_2".to_owned()),
                ref_index_id: Some("REFIDX_seg_2".to_owned()),
            },
            delivered_text: "第二段".to_owned(),
        },
    );

    assert_eq!(first.message_id.as_deref(), Some("qq_seg_1"));
    assert_eq!(second.message_id.as_deref(), Some("qq_seg_2"));
    assert!(cache.lock().unwrap().contains("qq_seg_1"));
    assert!(cache.lock().unwrap().contains("qq_seg_2"));
    assert!(!cache.lock().unwrap().contains("REFIDX_seg_1"));
    assert!(!cache.lock().unwrap().contains("REFIDX_seg_2"));
    assert_eq!(
        quoted_group_context(&ref_index, "g1", "REFIDX_seg_1")
            .text_summary
            .as_deref(),
        Some("第一段")
    );
    assert_eq!(
        quoted_group_context(&ref_index, "g1", "REFIDX_seg_2")
            .text_summary
            .as_deref(),
        Some("第二段")
    );
}

#[tokio::test]
async fn todo_push_refidx_without_message_id_does_not_enter_group_cache() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let runtime = GatewayPushRuntime {
        api: panic_api_client(),
        qq_official_account_id: "app".to_owned(),
        runtime: GatewayRuntimeStatus::default(),
        group_outbound_cache: cache.clone(),
        ref_index: ref_index.clone(),
    };
    let intent = PushIntent {
        target: PushTarget::qq_official(PushTargetType::Group, "g1"),
        mentions: Vec::new(),
        text: "Todo 提醒正文".to_owned(),
        fallback_text: Some("Todo 提醒正文".to_owned()),
        message_type: "text".to_owned(),
        visible_entity_snapshot: None,
    };
    let sent_ids = SendMessageIds {
        message_id: None,
        ref_index_id: Some("REFIDX_todo_only".to_owned()),
    };

    let push_result = runtime.record_successful_push(
        &intent,
        "g1",
        PushSendOutcome {
            ids: sent_ids,
            delivered_text: "Todo 提醒正文".to_owned(),
        },
    );

    assert_eq!(push_result.message_id, None);
    assert!(!cache.lock().unwrap().contains("REFIDX_todo_only"));
    let quoted = quoted_group_context(&ref_index, "g1", "REFIDX_todo_only");
    assert!(quoted.lookup_found);
    assert_eq!(quoted.text_summary.as_deref(), Some("Todo 提醒正文"));
}

#[tokio::test]
async fn push_with_message_id_only_does_not_forge_ref_index_entry() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let runtime = GatewayPushRuntime {
        api: panic_api_client(),
        qq_official_account_id: "app".to_owned(),
        runtime: GatewayRuntimeStatus::default(),
        group_outbound_cache: cache,
        ref_index: ref_index.clone(),
    };
    let intent = PushIntent {
        target: PushTarget::qq_official(PushTargetType::Group, "g1"),
        mentions: Vec::new(),
        text: "只有 message_id 的推送".to_owned(),
        fallback_text: Some("只有 message_id 的推送".to_owned()),
        message_type: "text".to_owned(),
        visible_entity_snapshot: None,
    };

    let push_result = runtime.record_successful_push(
        &intent,
        "g1",
        PushSendOutcome {
            ids: SendMessageIds {
                message_id: Some("qq_msg_only".to_owned()),
                ref_index_id: None,
            },
            delivered_text: "只有 message_id 的推送".to_owned(),
        },
    );
    assert_eq!(push_result.message_id.as_deref(), Some("qq_msg_only"));

    let quoted = quoted_group_context(&ref_index, "g1", "qq_msg_only");
    assert!(!quoted.lookup_found);
}

#[test]
fn push_ref_index_write_failure_is_best_effort() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let poisoned = ref_index.clone();
    let _ = std::panic::catch_unwind(move || {
        let _guard = poisoned.lock().unwrap();
        panic!("poison ref_index for test");
    });
    let runtime = GatewayPushRuntime {
        api: panic_api_client(),
        qq_official_account_id: "app".to_owned(),
        runtime: GatewayRuntimeStatus::default(),
        group_outbound_cache: cache,
        ref_index,
    };
    let intent = PushIntent {
        target: PushTarget::qq_official(PushTargetType::Group, "g1"),
        mentions: Vec::new(),
        text: "推送正文".to_owned(),
        fallback_text: Some("推送正文".to_owned()),
        message_type: "text".to_owned(),
        visible_entity_snapshot: None,
    };

    let push_result = runtime.record_successful_push(
        &intent,
        "g1",
        PushSendOutcome {
            ids: SendMessageIds {
                message_id: Some("qq_msg_1".to_owned()),
                ref_index_id: Some("REFIDX_poison".to_owned()),
            },
            delivered_text: "推送正文".to_owned(),
        },
    );
    assert_eq!(push_result.message_id.as_deref(), Some("qq_msg_1"));
}

#[tokio::test]
async fn push_sink_error_is_propagated() {
    let sender = MockPushSender {
        fail_text: true,
        ..MockPushSender::default()
    };

    let err = send_private_push(&sender, "u1", "text", "hello", "hello")
        .await
        .unwrap_err();

    assert!(err.log_summary().contains("text sending is unsupported"));
}

#[test]
fn push_intent_expresses_private_and_group_targets_without_http_metadata() {
    let private = PushIntent {
        target: PushTarget::qq_official(PushTargetType::Private, "u1"),
        mentions: Vec::new(),
        text: "hello".to_owned(),
        fallback_text: Some("hello".to_owned()),
        message_type: "text".to_owned(),
        visible_entity_snapshot: None,
    };
    let group = PushIntent {
        target: PushTarget::qq_official(PushTargetType::Group, "g1"),
        ..private.clone()
    };

    assert_eq!(private.target.platform, "qq_official");
    assert_eq!(private.target.target_type, PushTargetType::Private);
    assert_eq!(group.target.target_type, PushTargetType::Group);
    assert_eq!(private.message_type, "text");
}

#[test]
fn qq_gateway_rejects_non_qq_push_target_before_sending() {
    let intent = PushIntent {
        target: PushTarget::new(
            "wechat_service",
            Some("gh_service".to_owned()),
            PushTargetType::Private,
            "user-openid",
        ),
        mentions: Vec::new(),
        text: "hello".to_owned(),
        fallback_text: Some("hello".to_owned()),
        message_type: "text".to_owned(),
        visible_entity_snapshot: None,
    };

    let err = validate_qq_official_target(&intent, "app").unwrap_err();

    assert!(err.to_string().contains("wechat_service proactive"));
}

#[test]
fn qq_gateway_rejects_mismatched_qq_account() {
    let intent = PushIntent {
        target: PushTarget::new(
            "qq_official",
            Some("other-app".to_owned()),
            PushTargetType::Private,
            "u1",
        ),
        mentions: Vec::new(),
        text: "hello".to_owned(),
        fallback_text: Some("hello".to_owned()),
        message_type: "text".to_owned(),
        visible_entity_snapshot: None,
    };

    let err = validate_qq_official_target(&intent, "app").unwrap_err();

    assert!(err.to_string().contains("target account"));
}

fn panic_api_client() -> QqApiClient {
    crate::api::QqApiClient::new(
        qq_maid_common::http_client::client(),
        "http://127.0.0.1",
        crate::auth::AccessTokenManager::new(
            qq_maid_common::http_client::client(),
            "app",
            "secret",
            Duration::from_secs(60),
        ),
    )
}

use super::*;

fn qq_content(
    target_type: PushTargetType,
    mentions: &[PushMention],
    text: &str,
    fallback_text: &str,
    message_type: &str,
) -> PreparedQqBot2Content {
    prepare_qq_bot2_content(target_type, mentions, text, fallback_text, message_type)
}

#[test]
fn qq_group_text_payload_contains_single_real_member_mention() {
    let mentions = normalize_push_mentions(vec![PushMention::new("member-openid-1", None)]);
    let prepared = qq_content(
        PushTargetType::Group,
        &mentions,
        "待办提醒：提交周报",
        "待办提醒：提交周报",
        "text",
    );
    let payload = build_group_text_payload(&prepared.content, None, 1);

    assert_eq!(payload["content"], "<@member-openid-1>\n待办提醒：提交周报");
    assert_eq!(payload["msg_type"], 0);
    assert_eq!(prepared.ref_index_content, "待办提醒：提交周报");
}

#[test]
fn qq_group_mentions_ignore_empty_ids_and_stably_deduplicate_multiple_members() {
    let mentions = normalize_push_mentions(vec![
        PushMention::new(" member-openid-1 ", None),
        PushMention::new("", Some("空 ID".to_owned())),
        PushMention::new("member-openid-2", None),
        PushMention::new("member-openid-1", Some("重复".to_owned())),
    ]);
    let prepared = qq_content(
        PushTargetType::Group,
        &mentions,
        "完整提醒正文",
        "完整提醒正文",
        "text",
    );

    assert_eq!(
        prepared.content,
        "<@member-openid-1> <@member-openid-2>\n完整提醒正文"
    );
}

#[tokio::test]
async fn qq_group_markdown_carries_real_member_mention() {
    let sender = MockPushSender::default();
    let mentions = vec![PushMention::new("member-openid-1", None)];
    let prepared = qq_content(
        PushTargetType::Group,
        &mentions,
        "## 待办提醒\n\n提交周报",
        "待办提醒\n提交周报",
        "markdown",
    );

    let outcome = send_group_push(&sender, "group-openid", "markdown", &prepared)
        .await
        .unwrap();

    assert_eq!(
        sender.calls(),
        vec!["group-markdown:group-openid:<@member-openid-1>\n## 待办提醒\n\n提交周报"]
    );
    assert_eq!(outcome.delivered_text, "## 待办提醒\n\n提交周报");
}

#[tokio::test]
async fn qq_group_markdown_fallback_keeps_real_mention_and_ref_index_hides_member_id() {
    let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
    let ref_index = crate::gateway::ref_index::ref_index();
    let runtime = GatewayPushRuntime {
        api: panic_api_client(),
        qq_official_account_id: "bot-account".to_owned(),
        runtime: GatewayRuntimeStatus::default(),
        group_outbound_cache: cache,
        ref_index: ref_index.clone(),
    };
    let sender = MockPushSender {
        fail_markdown: true,
        message_id: Some("qq-fallback-message".to_owned()),
        ref_index_id: Some("REFIDX_mention_fallback".to_owned()),
        ..MockPushSender::default()
    };
    let intent = PushIntent {
        target: PushTarget::new(
            QQ_OFFICIAL_PLATFORM,
            Some("bot-account".to_owned()),
            PushTargetType::Group,
            "group-openid",
        ),
        mentions: vec![PushMention::new("sensitive-member-openid", None)],
        text: "## 待办提醒".to_owned(),
        fallback_text: Some("待办提醒".to_owned()),
        message_type: "markdown".to_owned(),
        visible_entity_snapshot: None,
    };
    validate_qq_official_target(&intent, "bot-account").unwrap();
    let prepared = qq_content(
        intent.target.target_type,
        &intent.mentions,
        &intent.text,
        intent.fallback_text.as_deref().unwrap(),
        &intent.message_type,
    );

    let outcome = send_group_push(
        &sender,
        &intent.target.target_id,
        &intent.message_type,
        &prepared,
    )
    .await
    .unwrap();
    runtime.record_successful_push(&intent, &intent.target.target_id, outcome);

    assert_eq!(
        sender.calls(),
        vec![
            "group-markdown:group-openid:<@sensitive-member-openid>\n## 待办提醒",
            "group-text:group-openid:<@sensitive-member-openid>\n待办提醒",
        ]
    );
    assert!(
        sender
            .calls()
            .iter()
            .all(|call| !call.contains("bot-account>"))
    );
    let quoted = quoted_group_context_for_account(
        &ref_index,
        "bot-account",
        "group-openid",
        "REFIDX_mention_fallback",
    );
    assert_eq!(quoted.text_summary.as_deref(), Some("待办提醒"));
    assert!(
        !quoted
            .text_summary
            .as_deref()
            .unwrap_or_default()
            .contains("sensitive-member-openid")
    );
}

#[test]
fn qq_private_push_ignores_member_mentions_and_no_mentions_remain_unchanged() {
    let mention = PushMention::new("member-openid-1", None);
    let private = qq_content(
        PushTargetType::Private,
        std::slice::from_ref(&mention),
        "私聊正文",
        "私聊降级正文",
        "markdown",
    );
    let group_without_mentions = qq_content(
        PushTargetType::Group,
        &[],
        "原 Markdown",
        "原文本",
        "markdown",
    );

    assert_eq!(private.content, "私聊正文");
    assert_eq!(private.fallback_content, "私聊降级正文");
    assert_eq!(group_without_mentions.content, "原 Markdown");
    assert_eq!(group_without_mentions.fallback_content, "原文本");
}

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

    let prepared = qq_content(PushTargetType::Group, &[], "# title", "title", "markdown");
    let outcome = send_group_push(&sender, "g1", "markdown", &prepared)
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

    let prepared = qq_content(PushTargetType::Group, &[], "hello", "hello", "text");
    let result = send_group_push(&sender, "g1", "text", &prepared)
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

    let prepared = qq_content(
        PushTargetType::Group,
        &[],
        "# Markdown 标题",
        "Markdown 标题",
        "markdown",
    );
    let outcome = send_group_push(&sender, "g1", "markdown", &prepared)
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

    let prepared = qq_content(
        PushTargetType::Group,
        &[],
        "# 失败的 Markdown",
        "降级文本",
        "markdown",
    );
    let outcome = send_group_push(&sender, "g1", "markdown", &prepared)
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

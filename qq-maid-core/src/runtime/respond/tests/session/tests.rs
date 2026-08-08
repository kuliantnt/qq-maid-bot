use super::*;

#[tokio::test]
async fn group_members_with_same_display_name_still_use_distinct_actor_refs() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    for (user_id, text) in [("u1", "A 的消息"), ("u2", "B 的消息")] {
        service
            .respond(message_with_actor_context(
                text,
                "group:g1",
                "g1",
                user_id,
                "同名成员",
            ))
            .await
            .unwrap();
    }

    let request = inspector
        .requests()
        .into_iter()
        .rev()
        .find(|request| request.metadata.get("purpose").map(String::as_str) == Some("chat"))
        .expect("missing B chat request");
    let actor_a = request
        .messages
        .iter()
        .find(|message| message.content.contains("A 的消息"))
        .and_then(|message| history_actor_ref(&message.content))
        .expect("A actor_ref should be present");
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let actor_b = session.history[2]
        .turn_actor
        .as_ref()
        .and_then(|actor| actor.actor_ref.as_deref())
        .expect("B actor_ref should be present");

    assert_ne!(actor_a, actor_b);
    let current_context = request
        .messages
        .last()
        .and_then(|message| message.content_parts.first())
        .expect("current MessageContext should be present")
        .fallback_text();
    assert!(current_context.contains("昵称=同名成员"));
    assert!(current_context.contains(&format!("current_actor_ref={actor_b}")));
}

#[tokio::test]
async fn compacted_group_summary_keeps_actor_ownership_for_next_member() {
    let provider = MockProvider::new();
    let inspector = provider.clone();
    let service = test_service_with_provider(provider.clone());

    service
        .respond(message_with_actor_context(
            "/set 昵称 初墨",
            "group:g1",
            "g1",
            "u1",
            "平台A",
        ))
        .await
        .unwrap();
    let actor_a = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap()
        .history[0]
        .turn_actor
        .as_ref()
        .and_then(|actor| actor.actor_ref.clone())
        .expect("A actor_ref should be persisted");
    let summary = format!(
        "当前话题：身份确认\n公共内容：无\n成员事实：\n- actor_ref={actor_a}：展示名为初墨\n待处理事项：无\n回复偏好：无"
    );
    provider.push_compact_reply(summary.clone());

    service
        .respond(message_with_actor_context(
            "/compact", "group:g1", "g1", "u1", "平台A",
        ))
        .await
        .unwrap();
    let compacted = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(compacted.summary, summary);
    assert!(
        compacted
            .summary
            .contains(&format!("actor_ref={actor_a}：展示名为初墨"))
    );

    service
        .respond(message_with_actor_context(
            "我是谁？",
            "group:g1",
            "g1",
            "u2",
            "平台B",
        ))
        .await
        .unwrap();
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    let actor_b = session
        .history
        .iter()
        .rev()
        .find(|message| message.role == "user" && message.content == "我是谁？")
        .and_then(|message| message.turn_actor.as_ref())
        .and_then(|actor| actor.actor_ref.as_deref())
        .expect("B actor_ref should be persisted");
    assert_ne!(actor_a, actor_b);

    let request = inspector
        .requests()
        .into_iter()
        .rev()
        .find(|request| request.metadata.get("purpose").map(String::as_str) == Some("chat"))
        .expect("missing B chat request");
    let request_text = request_text(&request);
    assert!(request_text.contains(&format!("actor_ref={actor_a}：展示名为初墨")));
    assert!(request_text.contains(&format!("current_actor_ref={actor_b}")));
    assert!(request_text.contains("actor_ref 不同或 unknown 时，不得把对应事实归给当前发言人"));
}

#[tokio::test]
async fn guild_channel_members_use_distinct_actor_refs_and_current_mapping() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    service
        .respond(guild_message_with_actor_context(
            "A 的频道消息",
            "u1",
            "同名成员",
        ))
        .await
        .unwrap();
    service
        .respond(guild_message_with_actor_context(
            "B 的频道消息",
            "u2",
            "同名成员",
        ))
        .await
        .unwrap();

    let request = inspector
        .requests()
        .into_iter()
        .rev()
        .find(|request| request.metadata.get("purpose").map(String::as_str) == Some("chat"))
        .expect("missing guild B chat request");
    let actor_a = request
        .messages
        .iter()
        .find(|message| message.content.contains("A 的频道消息"))
        .and_then(|message| history_actor_ref(&message.content))
        .expect("guild A actor_ref should be present");
    let meta = SessionMeta::new(
        "guild:guild-1:channel-1",
        Some("u2".to_owned()),
        None,
        Some("guild-1".to_owned()),
        Some("channel-1".to_owned()),
        "qq_official",
    );
    let session = service.session_store.get_or_create_active(&meta).unwrap();
    let actor_b = session.history[2]
        .turn_actor
        .as_ref()
        .and_then(|actor| actor.actor_ref.as_deref())
        .expect("guild B actor_ref should be present");

    assert_ne!(actor_a, actor_b);
    let request_text = request_text(&request);
    assert!(request_text.contains("[历史发言人：actor_ref="));
    assert!(request_text.contains(&format!("current_actor_ref={actor_b}")));
}

#[tokio::test]
async fn private_chat_does_not_inject_actor_refs_or_history_labels() {
    let inspector = MockProvider::new();
    let service = test_service_with_provider(inspector.clone());

    service
        .respond(private_message("第一条私聊"))
        .await
        .unwrap();
    service
        .respond(private_message("第二条私聊"))
        .await
        .unwrap();

    let request = inspector
        .requests()
        .into_iter()
        .rev()
        .find(|request| request.metadata.get("purpose").map(String::as_str) == Some("chat"))
        .expect("missing private chat request");
    let request_text = request_text(&request);
    assert!(request_text.contains("第一条私聊"));
    assert!(!request_text.contains("current_actor_ref="));
    assert!(!request_text.contains("[历史发言人："));
    assert!(!request_text.contains("[机器人当时回复给："));
}

pub(super) fn message_with_actor_context(
    text: &str,
    scope_key: &str,
    group_id: &str,
    user_id: &str,
    platform_name: &str,
) -> crate::runtime::respond::RespondRequest {
    let mut req = message_in_scope(text, scope_key, user_id, group_id);
    req.message_context = Some(qq_maid_common::identity_context::MessageContext {
        current_actor_ref: None,
        actor: Some(qq_maid_common::identity_context::MessageActorContext {
            user_id: Some(user_id.to_owned()),
            display_name: Some(platform_name.to_owned()),
            display_name_source: Some("event".to_owned()),
            source: qq_maid_common::identity_context::IdentitySource::Event,
            ..Default::default()
        }),
        mentions: Vec::new(),
        conversation: qq_maid_common::identity_context::ConversationContext {
            kind: "group".to_owned(),
            id: Some(group_id.to_owned()),
            platform: Some("qq_official".to_owned()),
            account_id: None,
        },
    });
    req
}

fn guild_message_with_actor_context(
    text: &str,
    user_id: &str,
    platform_name: &str,
) -> crate::runtime::respond::RespondRequest {
    let mut req = crate::runtime::respond::RespondRequest {
        content: text.to_owned(),
        scope_key: "guild:guild-1:channel-1".to_owned(),
        conversation_kind: qq_maid_common::identity_context::ConversationKind::Channel,
        conversation_id: Some("channel-1".to_owned()),
        user_id: Some(user_id.to_owned()),
        guild_id: Some("guild-1".to_owned()),
        channel_id: Some("channel-1".to_owned()),
        platform: "qq_official".to_owned(),
        event_type: "FakeEvent".to_owned(),
        ..crate::runtime::respond::common::empty_respond_request()
    };
    req.message_context = Some(qq_maid_common::identity_context::MessageContext {
        current_actor_ref: None,
        actor: Some(qq_maid_common::identity_context::MessageActorContext {
            user_id: Some(user_id.to_owned()),
            display_name: Some(platform_name.to_owned()),
            display_name_source: Some("event".to_owned()),
            source: qq_maid_common::identity_context::IdentitySource::Event,
            ..Default::default()
        }),
        mentions: Vec::new(),
        conversation: qq_maid_common::identity_context::ConversationContext {
            kind: "channel".to_owned(),
            id: Some("channel-1".to_owned()),
            platform: Some("qq_official".to_owned()),
            account_id: None,
        },
    });
    req
}

pub(super) fn last_chat_request_text(inspector: &MockProvider) -> String {
    request_text(
        inspector
            .requests()
            .iter()
            .rev()
            .find(|req| req.metadata.get("purpose").map(String::as_str) != Some("session_title"))
            .expect("missing chat request"),
    )
}

pub(super) fn history_actor_ref(content: &str) -> Option<&str> {
    let (_, tail) = content.lines().next()?.split_once("actor_ref=")?;
    tail.split(['，', ']']).next()
}

fn request_text(request: &ChatRequest) -> String {
    request
        .messages
        .iter()
        .flat_map(|message| {
            let mut texts = vec![message.content.as_str()];
            for part in &message.content_parts {
                if let qq_maid_common::input_part::MessageInputPart::Text { text, .. } = part {
                    texts.push(text.as_str());
                }
            }
            texts
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn assert_unimplemented_rss_commands_absent(text: &str) {
    for command in ["/rss refresh", "/rss enable", "/rss disable", "/rss edit"] {
        assert!(
            !text.contains(command),
            "unimplemented RSS command leaked into help: {command}"
        );
    }
}

#[tokio::test]
async fn resume_without_argument_lists_recent_sessions() {
    let service = test_service();
    service.respond(message("/new 旧话题")).await.unwrap();
    service.respond(message("/new 新话题")).await.unwrap();

    let response = service.respond(message("/resume")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.contains("最近会话"));
    assert!(text.contains("旧话题"));
    assert!(text.contains("使用 /resume 1 恢复"));
    assert!(markdown.contains("最近会话"));
    assert!(markdown.contains("1. 旧话题"));
}

#[tokio::test]
async fn resume_number_restores_selected_session() {
    let service = test_service();
    service.respond(message("/new 旧话题")).await.unwrap();
    service.respond(message("/new 新话题")).await.unwrap();

    let response = service.respond(message("/resume 1")).await.unwrap();

    assert!(response.text.unwrap().contains("已恢复会话：旧话题"));
    assert!(
        response
            .markdown
            .as_deref()
            .is_some_and(|markdown| markdown.contains("- 话题："))
    );
    assert_eq!(response.command.as_deref(), Some("resume"));
}

#[tokio::test]
async fn chinese_resume_alias_matches_resume() {
    let service = test_service();
    service.respond(message("/new 旧话题")).await.unwrap();
    service.respond(message("/new 新话题")).await.unwrap();

    let response = service.respond(message("/恢复")).await.unwrap();

    assert!(response.text.unwrap().contains("旧话题"));
}

#[tokio::test]
async fn list_is_deprecated_alias() {
    let service = test_service();
    service.respond(message("/new 旧话题")).await.unwrap();
    service.respond(message("/new 新话题")).await.unwrap();

    let response = service.respond(message("/list")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.contains("最近会话"));
    assert!(text.contains("已不推荐"));
    assert!(markdown.contains("提示：/list 已不推荐"));
}

#[tokio::test]
async fn new_without_argument_creates_default_title() {
    let service = test_service();

    service.respond(message("/new")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, DEFAULT_SESSION_TITLE);
    assert!(session.state.get("current_topic").is_none());
}

#[tokio::test]
async fn new_with_argument_keeps_user_title() {
    let service = test_service();

    service.respond(message("/new 示例材料")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, "示例材料");
    assert_eq!(
        session.state.get("current_topic").and_then(Value::as_str),
        Some("示例材料")
    );
}

#[tokio::test]
async fn first_chat_does_not_use_raw_user_text_as_title() {
    let service = test_service();
    let user_text = "整理一下今天的部署方案，顺便确认启动脚本和环境变量说明";

    service.respond(message(user_text)).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, DEFAULT_SESSION_TITLE);
    assert!(session.state.get("current_topic").is_none());
}

#[tokio::test]
async fn rename_without_title_override_uses_scene_aux_model() {
    let provider = MockProvider::with_title_replies(vec![Ok("辅助标题")]);
    let inspector = provider.clone();
    let agent_config = test_agent_config(false, false).with_scene_models_for_test(
        "private-main",
        Some("private-aux"),
        "group-main",
        Some("group-aux"),
    );
    let (service, _) =
        test_service_with_title_provider_and_agent_config(provider, None, agent_config);

    service
        .respond(private_message("讨论私聊部署日志"))
        .await
        .unwrap();
    let rename = service.respond(private_message("/rename")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert_eq!(session.title, "辅助标题");
    assert_eq!(rename.text.as_deref(), Some("已重命名为：辅助标题"));
    let title_request = inspector
        .requests()
        .into_iter()
        .find(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
        .unwrap();
    assert_eq!(title_request.model.as_deref(), Some("private-aux"));
}

#[tokio::test]
async fn rename_resolves_private_and_group_aux_models_independently() {
    let provider = MockProvider::with_title_replies(vec![Ok("私聊标题"), Ok("群聊标题")]);
    let inspector = provider.clone();
    let agent_config = test_agent_config(false, false).with_scene_models_for_test(
        "private-main",
        Some("private-aux"),
        "group-main",
        Some("group-aux"),
    );
    let (service, _) =
        test_service_with_title_provider_and_agent_config(provider, None, agent_config);

    service.respond(private_message("私聊内容")).await.unwrap();
    service.respond(private_message("/rename")).await.unwrap();
    service.respond(message("群聊内容")).await.unwrap();
    service.respond(message("/rename")).await.unwrap();

    let title_models = inspector
        .requests()
        .into_iter()
        .filter(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
        .map(|req| req.model.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(title_models, vec!["private-aux", "group-aux"]);
}

#[tokio::test]
async fn rename_falls_back_to_scene_main_model_when_aux_route_is_absent() {
    let provider = MockProvider::with_title_replies(vec![Ok("主路线标题")]);
    let inspector = provider.clone();
    let agent_config = test_agent_config(false, false).with_scene_models_for_test(
        "private-main",
        None,
        "group-main",
        None,
    );
    let (service, _) =
        test_service_with_title_provider_and_agent_config(provider, None, agent_config);

    service.respond(message("群聊内容")).await.unwrap();
    service.respond(message("/rename")).await.unwrap();

    let title_request = inspector
        .requests()
        .into_iter()
        .find(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
        .unwrap();
    assert_eq!(title_request.model.as_deref(), Some("group-main"));
}

#[tokio::test]
async fn streaming_and_non_streaming_auto_title_use_current_scene_aux_model() {
    let provider = MockProvider::with_title_replies(vec![Ok("群聊自动标题"), Ok("私聊自动标题")])
        .with_stream_enabled(true);
    let inspector = provider.clone();
    let agent_config = test_agent_config(false, false).with_scene_models_for_test(
        "private-main",
        Some("private-aux"),
        "group-main",
        Some("group-aux"),
    );
    let (service, _) =
        test_service_with_title_provider_and_agent_config(provider, None, agent_config);

    service.respond(message("群聊第一条")).await.unwrap();
    service.respond(message("群聊第二条")).await.unwrap();
    wait_for_session_title(&service, "群聊自动标题").await;

    service
        .respond_stream(private_message("私聊第一条"), |_| {
            Box::pin(async { Ok(()) })
        })
        .await
        .unwrap();
    service
        .respond_stream(private_message("私聊第二条"), |_| {
            Box::pin(async { Ok(()) })
        })
        .await
        .unwrap();
    wait_for_session_title_for_meta(&service, &private_test_meta(), "私聊自动标题").await;

    let title_models = inspector
        .requests()
        .into_iter()
        .filter(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
        .map(|req| req.model.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(title_models, vec!["group-aux", "private-aux"]);
}

#[tokio::test]
async fn auto_title_retries_after_failure_and_uses_per_call_model() {
    let provider =
        MockProvider::with_title_replies(vec![Ok(DEFAULT_SESSION_TITLE), Ok("部署排障")]);
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("第一条部署问题")).await.unwrap();
    service.respond(message("第二条日志线索")).await.unwrap();
    assert_eq!(
        service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap()
            .title,
        DEFAULT_SESSION_TITLE
    );

    service.respond(message("第三条确认方案")).await.unwrap();
    wait_for_session_title(&service, "部署排障").await;
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, "部署排障");
    assert!(
        !serde_json::to_string(&session)
            .unwrap()
            .contains("title-model")
    );

    let requests = inspector.requests();
    let title_requests = requests
        .iter()
        .filter(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
        .collect::<Vec<_>>();
    assert_eq!(title_requests.len(), 2);
    assert!(
        title_requests
            .iter()
            .all(|req| req.model.as_deref() == Some("title-model"))
    );
    assert!(
        requests
            .iter()
            .filter(|req| req.metadata.get("purpose").map(String::as_str) == Some("chat"))
            .all(|req| req.model.as_deref() == Some("mock-model"))
    );
}

#[tokio::test]
async fn auto_title_delay_does_not_block_chat_response() {
    let provider = MockProvider::with_title_replies(vec![Ok("后台标题")])
        .with_title_delay(Duration::from_millis(300));
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("第一条")).await.unwrap();
    let response = tokio::time::timeout(
        Duration::from_millis(100),
        service.respond(message("第二条触发标题")),
    )
    .await
    .expect("chat response should not wait for auto title")
    .unwrap();

    assert_eq!(response.text.as_deref(), Some("回复：第二条触发标题"));
    assert_eq!(
        service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap()
            .title,
        DEFAULT_SESSION_TITLE
    );
    wait_for_session_title(&service, "后台标题").await;
}

#[tokio::test]
async fn delayed_auto_title_preserves_messages_saved_after_snapshot() {
    let provider =
        MockProvider::with_title_replies(vec![Ok("后台标题"), Ok("后台标题"), Ok("后台标题")])
            .with_title_delay(Duration::from_millis(250));
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("第一条")).await.unwrap();
    service.respond(message("第二条触发标题")).await.unwrap();
    service.respond(message("第三条继续聊天")).await.unwrap();
    service.respond(message("第四条继续聊天")).await.unwrap();

    wait_for_session_title(&service, "后台标题").await;
    sleep(Duration::from_millis(300)).await;
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();

    assert_eq!(session.title, "后台标题");
    assert_eq!(
        session
            .history
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec![
            "第一条",
            "回复：第一条",
            "第二条触发标题",
            "回复：第二条触发标题",
            "第三条继续聊天",
            "回复：第三条继续聊天",
            "第四条继续聊天",
            "回复：第四条继续聊天",
        ]
    );
}

#[tokio::test]
async fn delayed_auto_title_does_not_overwrite_manual_rename() {
    let provider = MockProvider::with_title_replies(vec![Ok("后台标题")])
        .with_title_delay(Duration::from_millis(250));
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("第一条")).await.unwrap();
    service.respond(message("第二条触发标题")).await.unwrap();
    wait_for_title_request_count(&inspector, 1).await;
    service.respond(message("/rename 手工标题")).await.unwrap();

    sleep(Duration::from_millis(350)).await;
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();

    assert_eq!(session.title, "手工标题");
}

#[tokio::test]
async fn auto_title_failure_does_not_fail_chat_response() {
    let provider = MockProvider::with_title_replies(vec![Err(LlmError::provider(
        "title blocked",
        "provider",
    ))]);
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("第一条")).await.unwrap();
    let response = service.respond(message("第二条")).await.unwrap();

    assert_eq!(response.text.as_deref(), Some("回复：第二条"));
    wait_for_title_request_count(&inspector, 1).await;
    assert_eq!(
        service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap()
            .title,
        DEFAULT_SESSION_TITLE
    );
}

#[tokio::test]
async fn internal_flows_use_scene_aux_model_when_explicit_overrides_are_absent() {
    let provider = MockProvider::new();
    let inspector = provider.clone();
    let agent_config = test_agent_config(false, false).with_scene_models_for_test(
        "private-main",
        Some("private-aux"),
        "group-main",
        Some("group-aux"),
    );
    let (service, _) =
        test_service_with_title_provider_and_agent_config(provider, None, agent_config);

    service
        .respond(message_in_scope("/记 喜欢清淡口味", "group:g2", "u2", "g2"))
        .await
        .unwrap();

    let compact_meta = SessionMeta::new(
        "group:g3",
        Some("u3".to_owned()),
        Some("g3".to_owned()),
        None,
        None,
        "qq_official",
    );
    let mut session = service
        .session_store
        .get_or_create_active(&compact_meta)
        .unwrap();
    service
        .session_store
        .append_exchange(&mut session, "上一轮用户消息", "上一轮助手回复")
        .unwrap();
    service
        .respond(message_in_scope("/compact", "group:g3", "u3", "g3"))
        .await
        .unwrap();
    service
        .respond(message_in_scope("/翻译 hello", "group:g4", "u4", "g4"))
        .await
        .unwrap();

    let requests = inspector.requests();
    for purpose in ["memory_draft", "compact", "translation"] {
        assert!(requests.iter().any(|req| {
            req.metadata.get("purpose").map(String::as_str) == Some(purpose)
                && req.model.as_deref() == Some("group-aux")
        }));
    }
}

#[tokio::test]
async fn auto_title_stops_after_fourth_user_message() {
    let provider = MockProvider::with_title_replies(vec![
        Ok(DEFAULT_SESSION_TITLE),
        Ok(DEFAULT_SESSION_TITLE),
        Ok(DEFAULT_SESSION_TITLE),
    ]);
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    for text in ["第一条", "第二条", "第三条", "第四条", "第五条"] {
        service.respond(message(text)).await.unwrap();
    }
    wait_for_title_request_count(&inspector, 3).await;

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, DEFAULT_SESSION_TITLE);
    assert_eq!(
        inspector
            .requests()
            .iter()
            .filter(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
            .count(),
        3
    );
}

#[tokio::test]
async fn auto_title_does_not_overwrite_manual_title() {
    let provider = MockProvider::with_title_replies(Vec::<Result<&str, LlmError>>::new());
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("/new 手动标题")).await.unwrap();
    service.respond(message("第一条部署问题")).await.unwrap();
    service.respond(message("第二条日志线索")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.title, "手动标题");
    assert!(
        inspector.requests().iter().all(|req| {
            req.metadata.get("purpose").map(String::as_str) != Some("session_title")
        })
    );
}

#[tokio::test]
async fn rename_without_argument_can_generate_and_overwrite_title() {
    let provider = MockProvider::with_title_replies(vec![Ok("自动新标题")]);
    let inspector = provider.clone();
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("/new 手动标题")).await.unwrap();
    service.respond(message("讨论部署日志")).await.unwrap();
    let response = service.respond(message("/rename")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(response.text.as_deref(), Some("已重命名为：自动新标题"));
    assert_eq!(session.title, "自动新标题");
    assert_eq!(
        session.state.get("current_topic").and_then(Value::as_str),
        Some("自动新标题")
    );
    let title_request = inspector
        .requests()
        .into_iter()
        .find(|req| req.metadata.get("purpose").map(String::as_str) == Some("session_title"))
        .unwrap();
    assert_eq!(title_request.model.as_deref(), Some("title-model"));
    assert!(title_request.messages.iter().any(|message| {
        message.content.contains("用户：讨论部署日志")
            && message.content.contains("助手：回复：讨论部署日志")
    }));
}

#[tokio::test]
async fn rename_without_argument_keeps_title_on_generation_failure() {
    let provider = MockProvider::with_title_replies(vec![Ok(DEFAULT_SESSION_TITLE)]);
    let (service, _) = test_service_with_title_provider(provider);

    service.respond(message("/new 手动标题")).await.unwrap();
    service.respond(message("讨论部署日志")).await.unwrap();
    let response = service.respond(message("/rename")).await.unwrap();

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(
        response.text.as_deref(),
        Some("当前内容还不够生成标题，先保持原标题。")
    );
    assert_eq!(session.title, "手动标题");
}

#[tokio::test]
async fn resume_list_displays_default_for_dirty_titles() {
    let service = test_service();
    let meta = test_meta();
    for title in [
        "<faceType=1 faceId=2>",
        "faceId=123",
        r#"ext="eyJxxx""#,
        "[CQ:face,id=1]",
    ] {
        let mut session = service.session_store.create(&meta, "旧会话", true).unwrap();
        session.title = title.to_owned();
        service.session_store.save(&mut session).unwrap();
    }
    service.respond(message("/new 当前会话")).await.unwrap();

    let text = service
        .respond(message("/resume"))
        .await
        .unwrap()
        .text
        .unwrap();

    assert!(text.matches(DEFAULT_SESSION_TITLE).count() >= 4);
    assert!(!text.contains("faceType"));
    assert!(!text.contains("faceId"));
    assert!(!text.contains("ext=\"eyJ"));
    assert!(!text.contains("[CQ:"));
}

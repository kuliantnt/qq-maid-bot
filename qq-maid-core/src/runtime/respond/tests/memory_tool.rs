use qq_maid_common::identity_context::ConversationKind;
use qq_maid_llm::provider::ToolCallingProtocol;

use super::{super::interaction_state::respond_interaction_meta, support::*};
use crate::runtime::{
    pending::PreparedActionExecutionContext,
    session::now_iso_cn,
    tools::memory::{
        MemoryActor, MemoryOperations, MemoryPendingPayload, MemoryQuery, MemoryTarget,
        SAVE_MEMORY_TOOL_NAME, prepare_memory_draft,
    },
};

fn actor(user: &str, personal: &str, group: Option<&str>, admin: bool) -> MemoryActor {
    MemoryActor::from_context(
        Some(user.to_owned()),
        Some(personal.to_owned()),
        group.map(str::to_owned),
        admin,
    )
    .unwrap()
}

fn active_count(
    service: &crate::runtime::respond::RustRespondService,
    actor: &MemoryActor,
    target: MemoryTarget,
) -> usize {
    MemoryOperations::new(service.memory_store.clone())
        .list(actor, MemoryQuery::active(target))
        .unwrap()
        .len()
}

fn memory_provider(arguments: &str) -> MockProvider {
    MockProvider::new()
        .with_tool_protocol(ToolCallingProtocol::OpenAiResponses)
        .with_tool_call_json(SAVE_MEMORY_TOOL_NAME, arguments, "模型声称已经记住")
}

#[tokio::test]
async fn private_explicit_memory_intent_exposes_tool_and_writes_directly() {
    let inspector = memory_provider(r#"{"content":"你喜欢简短回复","scope":"personal"}"#);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("记住我喜欢简短回复"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("memory"));
    let text = response.text.unwrap();
    assert!(text.contains("🧠 已记住"));
    assert!(text.contains("范围：个人记忆"));
    assert!(text.contains("内容：你喜欢简短回复"));
    assert!(!text.contains("模型声称"));

    let request = inspector.tool_requests().remove(0);
    let metadata = request.tools.metadata();
    let tool = metadata
        .iter()
        .find(|tool| tool.name == SAVE_MEMORY_TOOL_NAME)
        .unwrap();
    assert!(tool.description.contains("普通陈述"));
    assert!(tool.description.contains("最终范围由服务端"));
    assert_eq!(
        tool.parameters["properties"]["scope"]["enum"],
        serde_json::json!(["auto", "personal", "profile"])
    );

    let user = actor("u1", "u1", None, false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        1
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
}

#[tokio::test]
async fn plain_statement_exposes_tool_but_does_not_call_or_write() {
    let inspector = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
    let service = test_service_with_provider_and_tool_calling(inspector.clone(), true);

    let response = service
        .respond(private_message("我最近在学 Rust"))
        .await
        .unwrap();

    let request = inspector.tool_requests().remove(0);
    assert!(
        request
            .tools
            .metadata()
            .iter()
            .any(|tool| tool.name == SAVE_MEMORY_TOOL_NAME)
    );
    assert!(
        response.diagnostics.unwrap()["agent_executed_tools"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let user = actor("u1", "u1", None, false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        0
    );
}

#[tokio::test]
async fn non_fixed_explicit_phrases_can_call_save_memory() {
    for (source, content) in [
        ("把这个作为我的长期偏好保存下来", "你偏好长期保留设置"),
        ("以后称呼我初墨", "以后称呼你初墨"),
        ("把这条放进我的个人资料里", "个人资料包含这条信息"),
    ] {
        let provider = memory_provider(
            &serde_json::json!({"content": content, "scope": "personal"}).to_string(),
        );
        let service = test_service_with_provider_and_tool_calling(provider, true);

        let response = service.respond(private_message(source)).await.unwrap();
        assert!(
            response.text.as_deref().unwrap().contains("🧠 已记住"),
            "{source}"
        );
        let user = actor("u1", "u1", None, false);
        assert_eq!(
            active_count(&service, &user, MemoryTarget::personal("u1")),
            1,
            "{source}"
        );
    }
}

#[tokio::test]
async fn explicit_negation_rejects_mistaken_tool_call() {
    let provider = memory_provider(r#"{"content":"这句话","scope":"personal"}"#);
    let service = test_service_with_provider_and_tool_calling(provider, true);

    let response = service
        .respond(private_message("不要保存这句话"))
        .await
        .unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("本次未保存"));
    assert!(!text.contains("已记住"));
    let user = actor("u1", "u1", None, false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        0
    );
}

#[tokio::test]
async fn default_group_route_exposes_memory_only() {
    let inspector = memory_provider(r#"{"content":"在这个群叫你初墨","scope":"profile"}"#);
    let service = test_service_with_provider_and_group_tool_calling(inspector.clone(), true, false);

    let response = service
        .respond(message("以后在这个群称呼我初墨"))
        .await
        .unwrap();
    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("范围：当前群画像")
    );
    let request = inspector.tool_requests().remove(0);
    let exposed = request
        .tools
        .metadata()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert_eq!(exposed, [SAVE_MEMORY_TOOL_NAME]);
    assert_eq!(
        response.diagnostics.unwrap()["agent_mode"],
        serde_json::json!("memory_only")
    );
}

#[tokio::test]
async fn scope_suggestion_conflict_requires_clarification() {
    let inspector = memory_provider(r#"{"content":"在这个群叫你初墨","scope":"personal"}"#);
    let service = test_service_with_provider_and_group_tool_calling(inspector, true, false);

    let response = service
        .respond(message("以后在这个群称呼我初墨"))
        .await
        .unwrap();
    let text = response.text.as_deref().unwrap();
    assert!(text.contains("个人记忆"));
    assert!(text.contains("当前群画像"));
    assert!(!text.contains("群组"));
    let session = service
        .session_store
        .get_active(&respond_interaction_meta(&message("范围检查")))
        .unwrap()
        .unwrap();
    assert_eq!(
        session.pending_operation.unwrap().display_snapshot()["choices"],
        serde_json::json!(["personal", "group_profile"])
    );
    let user = actor("u1", "u1", Some("g1"), false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::group_profile("g1", "u1")),
        0
    );
}

#[tokio::test]
async fn group_profile_tool_uses_current_actor_and_group() {
    let inspector = memory_provider(r#"{"content":"在这个群叫你棒冰","scope":"profile"}"#);
    let service = test_service_with_provider_and_group_tool_calling_tools(
        inspector,
        true,
        true,
        Some(vec![SAVE_MEMORY_TOOL_NAME.to_owned()]),
    );

    let response = service.respond(message("在这个群叫我棒冰")).await.unwrap();
    assert!(response.text.unwrap().contains("范围：当前群画像"));

    let user = actor("u1", "u1", Some("g1"), false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::group_profile("g1", "u1")),
        1
    );
}

#[tokio::test]
async fn group_rule_discussion_stays_in_normal_chat() {
    for source in ["这是本群规则吗？", "你觉得这是本群规则还是临时通知？"] {
        let provider = MockProvider::new().with_tool_protocol(ToolCallingProtocol::OpenAiResponses);
        let service =
            test_service_with_provider_and_group_tool_calling(provider.clone(), true, false);

        let response = service.respond(message(source)).await.unwrap();
        assert!(response.command.is_none(), "source={source}");
        assert!(response.text.as_deref().unwrap().contains(source));
        assert_eq!(provider.tool_call_count(), 1, "source={source}");
        assert_eq!(provider.tool_requests().len(), 1, "source={source}");
        assert_eq!(
            response.diagnostics.as_ref().unwrap()["agent_result"],
            "direct_answer",
            "source={source}"
        );
        let admin = actor("u1", "u1", Some("g1"), true);
        assert_eq!(
            active_count(&service, &admin, MemoryTarget::group("g1")),
            0,
            "source={source}"
        );
    }
}

#[tokio::test]
async fn save_memory_group_scope_returns_stable_command_only_error() {
    let provider = memory_provider(r#"{"content":"你喜欢简短回复","scope":"group"}"#);
    let service = test_service_with_provider_and_tool_calling(provider, true);

    let response = service
        .respond(private_message("记住我喜欢简短回复"))
        .await
        .unwrap();
    assert!(response.text.as_deref().unwrap().contains("`/memory`"));
    assert_eq!(
        response.diagnostics.as_ref().unwrap()["tool_outcomes"][0]["error_code"],
        "group_memory_command_only"
    );
    let user = actor("u1", "u1", None, false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::personal("u1")),
        0
    );
}

#[tokio::test]
async fn source_text_inferred_as_group_is_rejected_by_save_memory_tool() {
    let provider = memory_provider(r#"{"content":"每周五开周会","scope":"auto"}"#);
    let service = test_service_with_provider_and_group_tool_calling_tools(
        provider.clone(),
        true,
        true,
        Some(vec![SAVE_MEMORY_TOOL_NAME.to_owned()]),
    );

    let response = service
        .respond(message("记住这个群每周五开周会"))
        .await
        .unwrap();

    assert_eq!(provider.tool_requests().len(), 1);
    assert!(response.text.as_deref().unwrap().contains("`/memory`"));
    assert_eq!(
        response.diagnostics.as_ref().unwrap()["tool_outcomes"][0]["error_code"],
        "group_memory_command_only"
    );
    let admin = actor("u1", "u1", Some("g1"), true);
    assert_eq!(active_count(&service, &admin, MemoryTarget::group("g1")), 0);
}

#[tokio::test]
async fn group_scope_choice_is_rejected_and_clears_clarification() {
    let provider = memory_provider(r#"{"content":"周五开会","scope":"auto"}"#);
    let service = test_service_with_provider_and_group_tool_calling_tools(
        provider,
        true,
        true,
        Some(vec![SAVE_MEMORY_TOOL_NAME.to_owned()]),
    );

    service.respond(message("记住周五开会")).await.unwrap();
    let rejected = service.respond(message("群组")).await.unwrap();
    assert_eq!(
        rejected.command.as_deref(),
        Some("group_memory_command_only")
    );
    assert!(rejected.text.as_deref().unwrap().contains("`/memory`"));
    let interaction_meta = respond_interaction_meta(&message("群组"));
    assert!(
        service
            .session_store
            .get_active(&interaction_meta)
            .unwrap()
            .unwrap()
            .pending_operation
            .is_none()
    );
    let user = actor("u1", "u1", Some("g1"), true);
    assert_eq!(active_count(&service, &user, MemoryTarget::group("g1")), 0);
}

#[tokio::test]
async fn legacy_group_save_pending_is_rejected_before_confirm_revise_or_failed_retry() {
    for (failed, reply) in [(false, "确认"), (false, "改成新的群规则"), (true, "确认")] {
        let service = test_service();
        let request = message(reply);
        let interaction_meta = respond_interaction_meta(&request);
        let mut session = service
            .session_store
            .get_or_create_active(&interaction_meta)
            .unwrap();
        let draft = prepare_memory_draft(
            MemoryTarget::group("g1"),
            "旧自然语言群记忆".to_owned(),
            "群里记一下".to_owned(),
            None,
            "create",
        );
        let mut pending = MemoryPendingPayload::Save {
            initiator_user_id: "u1".to_owned(),
            owner_key: "u1".to_owned(),
            draft,
            created_at: now_iso_cn(),
        }
        .into_prepared_action(&session.scope_key);
        if failed {
            let revision = pending.revision();
            let now = now_iso_cn();
            pending
                .begin_execution(&PreparedActionExecutionContext {
                    initiator_user_id: Some("u1"),
                    owner_key: Some("u1"),
                    scope_key: &session.scope_key,
                    expected_revision: revision,
                    now: &now,
                })
                .unwrap();
            pending.mark_failed(revision).unwrap();
        }
        session.pending_operation = Some(pending);
        service.session_store.save(&mut session).unwrap();

        let response = service.respond(request).await.unwrap();
        assert_eq!(
            response.command.as_deref(),
            Some("group_memory_command_only"),
            "failed={failed}, reply={reply}"
        );
        assert!(response.text.as_deref().unwrap().contains("`/memory`"));
        assert!(
            service
                .session_store
                .get_active(&interaction_meta)
                .unwrap()
                .unwrap()
                .pending_operation
                .is_none()
        );
        let admin = actor("u1", "u1", Some("g1"), true);
        assert_eq!(active_count(&service, &admin, MemoryTarget::group("g1")), 0);
    }
}

#[tokio::test]
async fn group_profile_opt_out_and_sensitive_content_are_rejected() {
    let opted_out_provider = memory_provider(r#"{"content":"在这个群叫你雪糕","scope":"profile"}"#);
    let opted_out_service = test_service_with_provider_and_group_tool_calling_tools(
        opted_out_provider,
        true,
        true,
        Some(vec![SAVE_MEMORY_TOOL_NAME.to_owned()]),
    );
    let user = actor("u1", "u1", Some("g1"), false);
    let target = MemoryTarget::group_profile("g1", "u1");
    MemoryOperations::new(opted_out_service.memory_store.clone())
        .set_group_profile_enabled(&user, &target, false)
        .unwrap();
    let response = opted_out_service
        .respond(message("在这个群叫我雪糕"))
        .await
        .unwrap();
    assert!(response.text.unwrap().contains("已停止当前群保存群内画像"));
    assert_eq!(active_count(&opted_out_service, &user, target), 0);

    let sensitive_provider =
        memory_provider(r#"{"content":"身份证号 11010519491231002X","scope":"personal"}"#);
    let sensitive_service = test_service_with_provider_and_tool_calling(sensitive_provider, true);
    let response = sensitive_service
        .respond(private_message("记住我的身份证号 11010519491231002X"))
        .await
        .unwrap();
    let text = response.text.unwrap();
    assert!(text.contains("敏感信息"));
    assert!(!text.contains("已记住"));
}

#[tokio::test]
async fn ambiguous_group_scope_creates_clarification_then_writes_directly() {
    let inspector = memory_provider(r#"{"content":"周五开会","scope":"auto"}"#);
    let service = test_service_with_provider_and_group_tool_calling_tools(
        inspector,
        true,
        true,
        Some(vec![SAVE_MEMORY_TOOL_NAME.to_owned()]),
    );

    let clarify = service.respond(message("记住周五开会")).await.unwrap();
    let clarify_text = clarify.text.unwrap();
    assert!(clarify_text.contains("个人记忆"));
    assert!(clarify_text.contains("当前群画像"));
    assert!(!clarify_text.contains("群组"));

    let user = actor("u1", "u1", Some("g1"), false);
    assert_eq!(
        active_count(&service, &user, MemoryTarget::group_profile("g1", "u1")),
        0
    );
    let saved = service.respond(message("画像")).await.unwrap();
    let saved_text = saved.text.unwrap();
    assert!(
        saved_text.contains("范围：当前群画像"),
        "saved={saved_text}"
    );
    assert_eq!(
        active_count(&service, &user, MemoryTarget::group_profile("g1", "u1")),
        1
    );
}

#[tokio::test]
async fn missing_actor_and_database_failure_never_return_success_receipt() {
    let missing_provider = memory_provider(r#"{"content":"你喜欢简短回复","scope":"personal"}"#);
    let missing_service = test_service_with_provider_and_tool_calling(missing_provider, true);
    let mut missing_request = private_message("记住我喜欢简短回复");
    missing_request.user_id = None;
    let missing = missing_service.respond(missing_request).await.unwrap();
    let text = missing.text.unwrap();
    assert!(text.contains("缺少稳定用户身份"));
    assert!(!text.contains("已记住"));

    let failed_provider = memory_provider(r#"{"content":"你喜欢简短回复","scope":"personal"}"#);
    let failed_service = test_service_with_provider_and_tool_calling(failed_provider, true);
    failed_service
        .memory_store
        .abort_memory_insert_for_test()
        .unwrap();
    let failed = failed_service
        .respond(private_message("记住我喜欢简短回复"))
        .await
        .unwrap();
    let text = failed.text.unwrap();
    assert!(text.contains("写入失败"));
    assert!(!text.contains("已记住"));
}

#[tokio::test]
async fn onebot_group_tool_uses_account_namespaced_memory_scope() {
    let provider = memory_provider(r#"{"content":"在这个群叫你棒冰","scope":"profile"}"#);
    let service = test_service_with_provider_and_group_tool_calling_tools(
        provider,
        true,
        true,
        Some(vec![SAVE_MEMORY_TOOL_NAME.to_owned()]),
    );
    let mut request = message_in_scope(
        "在这个群叫我棒冰",
        "platform:onebot11:account:bot-a:group:g1",
        "u1",
        "g1",
    );
    request.platform = "onebot11".to_owned();
    request.account_id = Some("bot-a".to_owned());
    request.conversation_kind = ConversationKind::Group;
    request.conversation_id = Some("g1".to_owned());

    let response = service.respond(request).await.unwrap();
    assert!(response.text.unwrap().contains("范围：当前群画像"));

    let personal = "platform:onebot11:account:bot-a:private:u1";
    let group = "platform:onebot11:account:bot-a:group:g1";
    let user = actor("u1", personal, Some(group), false);
    assert_eq!(
        active_count(
            &service,
            &user,
            MemoryTarget::group_profile(group, personal)
        ),
        1
    );
}

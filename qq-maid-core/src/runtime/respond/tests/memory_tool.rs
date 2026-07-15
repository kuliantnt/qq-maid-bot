use qq_maid_common::identity_context::ConversationKind;
use qq_maid_llm::provider::ToolCallingProtocol;

use crate::runtime::tools::memory::{
    MemoryActor, MemoryOperations, MemoryQuery, MemoryTarget, SAVE_MEMORY_TOOL_NAME,
};

use super::support::*;

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
    assert!(response.text.as_deref().unwrap().contains("对所有聊天生效"));
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
async fn group_public_tool_allows_admin_and_rejects_member() {
    let admin_provider = memory_provider(r#"{"content":"每周五开周会","scope":"group"}"#);
    let admin_service = test_service_with_provider_and_group_tool_calling_tools(
        admin_provider,
        true,
        true,
        Some(vec![SAVE_MEMORY_TOOL_NAME.to_owned()]),
    );
    let admin_response = admin_service
        .respond(message("记住这个群每周五开周会"))
        .await
        .unwrap();
    assert!(
        admin_response
            .text
            .unwrap()
            .contains("范围：当前群公共记忆")
    );
    let admin = actor("u1", "u1", Some("g1"), true);
    assert_eq!(
        active_count(&admin_service, &admin, MemoryTarget::group("g1")),
        1
    );

    let member_provider = memory_provider(r#"{"content":"每周五开周会","scope":"group"}"#);
    let member_service = test_service_with_provider_and_group_tool_calling_tools(
        member_provider,
        true,
        true,
        Some(vec![SAVE_MEMORY_TOOL_NAME.to_owned()]),
    );
    let mut request = message("记住这个群每周五开周会");
    request.group_member_role = Some("member".to_owned());
    let member_response = member_service.respond(request).await.unwrap();
    let text = member_response.text.unwrap();
    assert!(text.contains("只能由群主或管理员"));
    assert!(!text.contains("已记住"));
    let member = actor("u1", "u1", Some("g1"), false);
    assert_eq!(
        active_count(&member_service, &member, MemoryTarget::group("g1")),
        0
    );
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
    assert!(clarify.text.unwrap().contains("对所有聊天生效"));

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

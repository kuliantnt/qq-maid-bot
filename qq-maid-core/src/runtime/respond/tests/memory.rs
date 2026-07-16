use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use qq_maid_common::input_part::QuotedMessageContext;

use super::{super::memory_flow::short_memory_id, support::*};
use crate::runtime::{
    respond::RespondRequest,
    tools::memory::{
        CreateMemoryRequest, CreateScopedMemoryRequest, ListMemoryQuery, MemoryScopeType,
        ScopedMemoryQuery,
    },
};
use crate::service::CoreInboundKind;

fn group_member_message(text: &str, role: Option<&str>) -> RespondRequest {
    let mut req = message(text);
    req.user_id = Some("u2".to_owned());
    req.group_member_role = role.map(str::to_owned);
    req
}

#[tokio::test]
async fn memory_create_is_direct_while_update_and_delete_require_confirmation() {
    let service = test_service();

    let created = service
        .respond(private_message(
            "/memory personal 回复技术方案时，请先给结论",
        ))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(created.contains("🧠 已记住"));
    let record = service
        .memory_store
        .list(ListMemoryQuery::default())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let old_id = record.id.clone();
    service.respond(private_message("/memory")).await.unwrap();

    let update = service
        .respond(private_message("/memory edit 1 技术方案回复先给结论"))
        .await
        .unwrap();
    assert!(
        update
            .text
            .as_deref()
            .unwrap()
            .contains("预期变更：replace")
    );
    assert!(service.memory_store.get(&old_id).is_ok());
    assert!(
        service
            .respond(private_message("确认"))
            .await
            .unwrap()
            .text
            .unwrap()
            .contains("已纠正记忆")
    );
    assert!(
        service
            .memory_store
            .list(ListMemoryQuery::default())
            .unwrap()
            .iter()
            .any(|record| record.content == "技术方案回复先给结论")
    );

    service.respond(private_message("/memory")).await.unwrap();
    let before_delete = service
        .memory_store
        .list(ListMemoryQuery::default())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let delete = service
        .respond(private_message("/memory delete 1"))
        .await
        .unwrap();
    assert!(delete.text.as_deref().unwrap().contains("待删除"));
    assert!(
        service
            .respond(private_message("确认"))
            .await
            .unwrap()
            .text
            .unwrap()
            .contains("已删除这条记忆")
    );
    assert!(service.memory_store.get(&before_delete.id).is_err());
}

#[tokio::test]
async fn personal_memory_write_does_not_require_group_admin_role() {
    let service = test_service();

    let created = service
        .respond(group_member_message(
            "/memory personal 回复技术方案时，请先给结论",
            Some("member"),
        ))
        .await
        .unwrap();
    assert!(created.text.as_deref().unwrap().contains("个人记忆"));
    assert!(
        service
            .memory_store
            .list(ListMemoryQuery::default())
            .unwrap()
            .iter()
            .any(|record| record.content.contains("先给结论"))
    );
}

#[tokio::test]
async fn group_memory_is_visible_to_group_but_only_admin_or_owner_can_manage() {
    let service = test_service();

    service
        .memory_store
        .create_scoped(CreateScopedMemoryRequest {
            scope_type: MemoryScopeType::Group,
            scope_id: "g1".to_owned(),
            created_by_user_id: "u1".to_owned(),
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "群规则：回复要简洁".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let member_list = service
        .respond(group_member_message("/memory group", Some("member")))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(member_list.contains("群规则：回复要简洁"));

    let denied = service
        .respond(group_member_message(
            "/memory group edit 1 群规则：回复要更简洁",
            Some("member"),
        ))
        .await
        .unwrap();
    assert_eq!(denied.command.as_deref(), Some("group_admin_required"));
    assert!(denied.text.unwrap().contains("群主或管理员"));

    let admin_list = service
        .respond(group_member_message("/memory group", Some("admin")))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(admin_list.contains("群规则：回复要简洁"));
    let edit = service
        .respond(group_member_message(
            "/memory group edit 1 群规则：回复要更简洁",
            Some("admin"),
        ))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(edit.contains("预期变更：replace"));
    let edit = service
        .respond(group_member_message("确认", Some("admin")))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(edit.contains("已纠正记忆"));

    let records = service
        .memory_store
        .list_scoped(ScopedMemoryQuery {
            scope_type: MemoryScopeType::Group,
            scope_id: "g1".to_owned(),
            limit: Some(10),
            q: None,
            scope: None,
            memory_type: None,
        })
        .unwrap();
    assert_eq!(records[0].content, "群规则：回复要更简洁");
}

#[tokio::test]
async fn empty_group_memory_list_shows_scoped_quick_actions() {
    let service = test_service();

    let response = service
        .respond(group_member_message("/记忆 群", Some("member")))
        .await
        .unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert_eq!(response.command.as_deref(), Some("memory_list"));
    assert!(text.contains("当前群公共记忆"));
    assert!(text.contains("当前还没有保存内容"));
    assert!(text.contains("群主或管理员添加：/记忆 群 添加 要记住的内容"));
    assert!(markdown.starts_with("# 🧠 当前群公共记忆"));
    assert!(markdown.contains("`/记忆 群 添加 要记住的内容`"));
}

#[tokio::test]
async fn group_memory_delete_accepts_normalized_mention_body_and_quoted_context_once() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(provider_calls.clone()));
    let record = service
        .memory_store
        .create_scoped(CreateScopedMemoryRequest {
            scope_type: MemoryScopeType::Group,
            scope_id: "g1".to_owned(),
            created_by_user_id: "u2".to_owned(),
            user_id: Some("u2".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "待确认删除的群记忆".to_owned(),
            source_text: "test seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let list = service
        .respond(group_member_message("/记忆 群", Some("admin")))
        .await
        .unwrap();
    assert!(list.text.as_deref().unwrap().contains("待确认删除的群记忆"));
    let prepared = service
        .respond(group_member_message("/记忆 群 delete 1", Some("admin")))
        .await
        .unwrap();
    assert!(prepared.text.as_deref().unwrap().contains("待删除"));
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);

    let classification = service
        .classify_inbound(group_member_message("确认", Some("admin")))
        .unwrap();
    assert_eq!(classification.kind, CoreInboundKind::Immediate);

    // 同群其他 actor 的确认不能看到或消费发起人的 interaction pending。
    service.respond(message("确认")).await.unwrap();
    assert!(service.memory_store.get(&record.id).is_ok());

    // Gateway 会把“@机器人 确认”规范化为“确认”；引用信息继续单独放在 quoted，
    // 不参与 PendingReplyKind 的正文分类。
    let mut confirmed_request = group_member_message("确认", Some("admin"));
    confirmed_request.quoted = Some(QuotedMessageContext {
        lookup_found: true,
        from_bot: Some(true),
        text_summary: Some("待删除：待确认删除的群记忆".to_owned()),
        ..QuotedMessageContext::default()
    });
    let confirmed = service.respond(confirmed_request).await.unwrap();
    assert!(
        confirmed
            .text
            .as_deref()
            .unwrap()
            .contains("已删除这条记忆")
    );
    assert!(service.memory_store.get(&record.id).is_err());

    let repeated = service
        .respond(group_member_message("确认", Some("admin")))
        .await
        .unwrap();
    assert!(!repeated.text.as_deref().unwrap().contains("已删除这条记忆"));
    assert!(service.memory_store.get(&record.id).is_err());
}

#[tokio::test]
async fn group_memory_delete_accepts_normalized_mention_cancel_without_llm() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(provider_calls.clone()));
    let record = service
        .memory_store
        .create_scoped(CreateScopedMemoryRequest {
            scope_type: MemoryScopeType::Group,
            scope_id: "g1".to_owned(),
            created_by_user_id: "u2".to_owned(),
            user_id: Some("u2".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "取消后保留的群记忆".to_owned(),
            source_text: "test seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    service
        .respond(group_member_message("/记忆 群", Some("admin")))
        .await
        .unwrap();
    service
        .respond(group_member_message("/记忆 群 delete 1", Some("admin")))
        .await
        .unwrap();

    // Gateway 会把“@机器人 取消”规范化为“取消”。
    let cancelled = service
        .respond(group_member_message("取消", Some("admin")))
        .await
        .unwrap();
    assert_eq!(
        cancelled.command.as_deref(),
        Some("memory_pending_cancelled")
    );
    assert!(service.memory_store.get(&record.id).is_ok());
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn malformed_group_memory_delete_returns_usage_without_llm() {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(provider_calls.clone()));

    let response = service
        .respond(group_member_message("/记忆 群 delete", Some("admin")))
        .await
        .unwrap();

    assert!(
        response
            .text
            .as_deref()
            .unwrap()
            .contains("用法：/memory group delete")
    );
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn group_memory_management_requires_owner_or_admin() {
    let service = test_service();

    let denied_create = service
        .respond(group_member_message(
            "/memory group add 群规则：回复要简洁",
            Some("member"),
        ))
        .await
        .unwrap();
    assert_eq!(
        denied_create.command.as_deref(),
        Some("group_admin_required")
    );
    assert!(denied_create.text.unwrap().contains("群主或管理员"));

    service
        .memory_store
        .create_scoped(CreateScopedMemoryRequest {
            scope_type: MemoryScopeType::Group,
            scope_id: "g1".to_owned(),
            created_by_user_id: "u1".to_owned(),
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "群规则：回复要简洁".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let list = service
        .respond(group_member_message("/memory group", Some("member")))
        .await
        .unwrap();
    assert!(list.text.unwrap().contains("群规则：回复要简洁"));

    let denied_edit = service
        .respond(group_member_message(
            "/memory group edit 1 群规则：回复要更简洁",
            Some("member"),
        ))
        .await
        .unwrap();
    assert_eq!(denied_edit.command.as_deref(), Some("group_admin_required"));
    let active = service
        .session_store
        .get_active(&test_meta())
        .unwrap()
        .unwrap();
    assert!(active.pending_operation.is_none());
}

#[tokio::test]
async fn legacy_group_keyword_is_search_and_explicit_add_writes() {
    let service = test_service();
    service
        .memory_store
        .create_scoped(CreateScopedMemoryRequest {
            scope_type: MemoryScopeType::Group,
            scope_id: "g1".to_owned(),
            created_by_user_id: "u1".to_owned(),
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "Rust 项目统一使用 workspace Cargo.lock".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let before = service
        .memory_store
        .list_scoped(ScopedMemoryQuery {
            scope_type: MemoryScopeType::Group,
            scope_id: "g1".to_owned(),
            limit: Some(10),
            q: None,
            scope: None,
            memory_type: None,
        })
        .unwrap()
        .len();
    let searched = service
        .respond(message("/memory group Rust"))
        .await
        .unwrap();
    assert_eq!(searched.command.as_deref(), Some("memory_list"));
    assert!(
        searched
            .text
            .as_deref()
            .unwrap()
            .contains("Rust 项目统一使用 workspace Cargo.lock")
    );
    assert_eq!(
        service
            .memory_store
            .list_scoped(ScopedMemoryQuery {
                scope_type: MemoryScopeType::Group,
                scope_id: "g1".to_owned(),
                limit: Some(10),
                q: None,
                scope: None,
                memory_type: None,
            })
            .unwrap()
            .len(),
        before,
        "兼容搜索不得新增群记忆"
    );
    let explicit_search = service
        .respond(message("/memory group list Rust"))
        .await
        .unwrap();
    assert_eq!(explicit_search.command.as_deref(), Some("memory_list"));
    assert!(
        explicit_search
            .text
            .as_deref()
            .unwrap()
            .contains("Rust 项目统一使用 workspace Cargo.lock")
    );

    let added = service
        .respond(message("/memory group add Rust"))
        .await
        .unwrap();
    assert_eq!(added.command.as_deref(), Some("memory_saved"));
    assert!(
        added
            .text
            .as_deref()
            .unwrap()
            .contains("范围：当前群公共记忆")
    );
    assert_eq!(
        service
            .memory_store
            .list_scoped(ScopedMemoryQuery {
                scope_type: MemoryScopeType::Group,
                scope_id: "g1".to_owned(),
                limit: Some(10),
                q: None,
                scope: None,
                memory_type: None,
            })
            .unwrap()
            .len(),
        before + 1
    );
}

#[tokio::test]
async fn group_memory_commands_reject_private_chat() {
    let service = test_service();

    let response = service
        .respond(RespondRequest {
            content: "/memory group".to_owned(),
            scope_key: "private:u1".to_owned(),
            user_id: Some("u1".to_owned()),
            group_id: None,
            platform: "qq_official".to_owned(),
            event_type: "FakeEvent".to_owned(),
            ..RespondRequest::default()
        })
        .await
        .unwrap();

    assert_eq!(
        response.text.as_deref(),
        Some("群记忆只能在群聊中查看或管理。")
    );
}

#[tokio::test]
async fn memory_list_index_from_group_scope_does_not_fall_back_to_id_prefix() {
    let service = test_service();
    service
        .memory_store
        .create_scoped(CreateScopedMemoryRequest {
            scope_type: MemoryScopeType::Group,
            scope_id: "g1".to_owned(),
            created_by_user_id: "u1".to_owned(),
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "群列表里的记忆".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    service.respond(message("/记忆 群")).await.unwrap();
    let response = service
        .respond(message("/记忆 查看 1"))
        .await
        .unwrap()
        .text
        .unwrap();

    assert!(response.contains("请前往私聊"));
    assert!(!response.contains("memory id prefix"));
}

#[tokio::test]
async fn memory_create_rejects_invalid_structured_output_without_pending() {
    for input in [
        "/memory invalid-memory-create",
        "/memory null-memory-create",
        "/memory empty-memory-create",
    ] {
        let service = test_service();
        let response = service.respond(message(input)).await.unwrap();
        assert_eq!(
            response.text.as_deref(),
            Some("唔，这条记忆草稿没整理成功，或者内容不适合写入长期记忆。")
        );
        let session = service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap();
        assert!(session.pending_operation.is_none());
        assert!(
            service
                .memory_store
                .list(ListMemoryQuery::default())
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn memory_create_database_error_does_not_return_success() {
    let service = test_service();
    service.memory_store.drop_schema_for_test().unwrap();

    let err = service
        .respond(message("/memory personal 回复技术方案时，请先给结论"))
        .await
        .unwrap_err();

    assert_eq!(err.stage, "memory");
    assert!(err.message.contains("memory store failed"));
    assert!(!err.message.contains("已记下"));
}

#[tokio::test]
async fn memory_edit_database_error_does_not_return_success_or_scope_error() {
    let service = test_service();
    let old = service
        .memory_store
        .create_scoped(CreateScopedMemoryRequest {
            scope_type: MemoryScopeType::Personal,
            scope_id: "u1".to_owned(),
            created_by_user_id: "u1".to_owned(),
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "旧记忆".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    service.respond(private_message("/memory")).await.unwrap();
    service.memory_store.abort_memory_insert_for_test().unwrap();
    let prepared = service
        .respond(private_message("/memory edit 1 新记忆"))
        .await
        .unwrap();
    assert!(
        prepared
            .text
            .as_deref()
            .unwrap()
            .contains("预期变更：replace")
    );
    let failed = service.respond(private_message("确认")).await.unwrap();
    assert!(failed.text.as_deref().unwrap().contains("执行失败"));
    assert!(!failed.text.as_deref().unwrap().contains("已纠正记忆"));
    assert_eq!(service.memory_store.get(&old.id).unwrap().content, "旧记忆");
}

#[tokio::test]
async fn chat_memory_context_database_error_does_not_fallback_to_success() {
    let service = test_service();
    service.memory_store.drop_schema_for_test().unwrap();

    let err = service.respond(message("普通聊天")).await.unwrap_err();

    assert_eq!(err.stage, "memory");
    assert!(err.message.contains("memory store failed"));
}

#[tokio::test]
async fn missing_legacy_memory_json_file_does_not_affect_sqlite_memory() {
    let (service, base) = test_service_with_base();
    assert!(!base.join("memories.jsonl").exists());

    service
        .respond(message("/memory personal 回复技术方案时，请先给结论"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    let records = service
        .memory_store
        .list(ListMemoryQuery::default())
        .unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].content.contains("技术方案回复"));
    assert!(!base.join("memories.jsonl").exists());
}

#[tokio::test]
async fn legacy_memory_phrase_only_hints_without_writing() {
    let service = test_service();

    let response = service.respond(message("记一下这个玩笑")).await.unwrap();

    assert_eq!(response.command.as_deref(), Some("memory_legacy_hint"));
    assert!(response.text.unwrap().contains("/memory"));
    assert!(
        service
            .memory_store
            .list(ListMemoryQuery::default())
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn memory_create_accepts_fenced_json_but_saves_content_only() {
    let service = test_service();

    let created = service
        .respond(message("/memory personal fenced-memory-create"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(created.contains("🧠 已记住"));
    assert!(created.contains("技术方案回复时先给结论和风险"));
    assert!(!created.contains("```"));
    assert!(!created.contains("\"content\""));
    let record = service
        .memory_store
        .list(ListMemoryQuery::default())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(record.content, "技术方案回复时先给结论和风险");
    assert!(!record.content.contains("```"));
    assert!(!record.content.contains("\"content\""));
}

#[tokio::test]
async fn memory_root_aliases_list_records_without_llm() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(calls.clone()));

    for command in ["/memory", "/记忆", "/记"] {
        let response = service.respond(private_message(command)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some("memory_list"));
        let text = response.text.unwrap();
        assert!(text.contains("当前还没有保存内容"));
        assert!(text.contains("快速操作"));
        assert!(text.contains("/记忆 要记住的内容"));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "日常聊天中不要只用编号称呼成员".to_owned(),
            source_text: "/memory 日常聊天中不要只用编号称呼成员".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let text = service
        .respond(private_message("/记忆"))
        .await
        .unwrap()
        .text
        .unwrap();
    let populated = service.respond(private_message("/记忆")).await.unwrap();
    assert!(text.contains("🧠 个人记忆"));
    assert!(text.contains("共 1 条"));
    assert!(text.contains("日常聊天中不要只用编号称呼成员"));
    assert!(text.contains("/记忆 查看 1"));
    assert!(populated.markdown.as_deref().unwrap().contains("1. "));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn memory_management_uses_recent_list_index() {
    let service = test_service();
    service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "第一条记忆".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();
    let _second = service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "第二条记忆".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let list = service
        .respond(private_message("/memory"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("1 "));
    assert!(list.contains("第二条记忆"));

    let detail = service
        .respond(private_message("/memory show 1"))
        .await
        .unwrap();
    assert!(detail.text.as_deref().unwrap().contains("第二条记忆"));
    assert!(detail.markdown.as_deref().unwrap().contains("内容："));

    let edit = service
        .respond(private_message("/memory edit 1 第二条记忆已更新"))
        .await
        .unwrap();
    assert!(edit.text.as_deref().unwrap().contains("预期变更：replace"));
    let edit = service.respond(private_message("确认")).await.unwrap();
    assert!(edit.text.as_deref().unwrap().contains("已纠正记忆"));
    assert!(
        service
            .memory_store
            .list(ListMemoryQuery::default())
            .unwrap()
            .iter()
            .any(|record| record.content == "第二条记忆已更新")
    );

    service.respond(private_message("/memory")).await.unwrap();
    let before_delete = service
        .memory_store
        .list(ListMemoryQuery::default())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let delete = service
        .respond(private_message("/memory delete 1"))
        .await
        .unwrap();
    assert!(delete.text.as_deref().unwrap().contains("待删除"));
    let delete = service.respond(private_message("确认")).await.unwrap();
    assert!(delete.text.as_deref().unwrap().contains("已删除这条记忆"));
    assert!(service.memory_store.get(&before_delete.id).is_err());
}

#[tokio::test]
async fn memory_list_then_show_parses_visible_index() {
    let service = test_service();
    service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "列表后按序号查看的记忆".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let list = service
        .respond(private_message("/memory list"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("1 "));
    assert!(list.contains("列表后按序号查看的记忆"));

    let detail = service
        .respond(private_message("/memory show 1"))
        .await
        .unwrap();
    let detail_text = detail.text.as_deref().unwrap();
    assert_eq!(detail.command.as_deref(), Some("memory_show"));
    assert!(detail_text.contains("列表后按序号查看的记忆"));
}

#[tokio::test]
async fn memory_management_rejects_id_target_and_requires_list_index() {
    let service = test_service();
    let record = service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "只能通过列表序号管理".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "note".to_owned(),
            scope: "general".to_owned(),
        })
        .unwrap();

    let without_list = service
        .respond(private_message("/memory delete 1"))
        .await
        .unwrap();
    assert!(
        without_list
            .text
            .as_deref()
            .unwrap()
            .contains("请先发送 /memory")
    );

    service.respond(private_message("/memory")).await.unwrap();
    let by_id = service
        .respond(private_message(&format!(
            "/memory delete {}",
            short_memory_id(&record.id)
        )))
        .await
        .unwrap();
    assert!(by_id.text.as_deref().unwrap().contains("请先发送 /memory"));
    assert!(service.memory_store.get(&record.id).is_ok());
}

#[tokio::test]
async fn memory_update_command_hints_edit_without_creating_pending() {
    let service = test_service();

    let response = service
        .respond(private_message("/memory update 1 新内容"))
        .await
        .unwrap();

    assert_eq!(
        response.text.as_deref(),
        Some("记忆修改请使用：/memory edit 列表序号 新内容")
    );
    let session = service
        .session_store
        .get_or_create_active(&private_test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
}

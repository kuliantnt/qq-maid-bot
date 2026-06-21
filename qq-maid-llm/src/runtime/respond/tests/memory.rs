use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::{super::memory_flow::short_memory_id, support::*};
use crate::runtime::{
    memory::{CreateMemoryRequest, ListMemoryQuery},
    pending::PendingOperation,
};

#[tokio::test]
async fn memory_create_update_and_delete_use_confirmation() {
    let service = test_service();

    let draft = service
        .respond(message("/memory 如果不确定前台，请礼貌询问"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(draft.contains("整理成这条记忆草稿"));
    assert!(
        service
            .memory_store
            .list(ListMemoryQuery::default())
            .unwrap()
            .is_empty()
    );

    let created = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(created.contains("已记下"));
    let record = service
        .memory_store
        .list(ListMemoryQuery::default())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let memory_id = short_memory_id(&record.id);
    service.respond(message("/memory")).await.unwrap();

    let update = service
        .respond(message("/memory edit 1 前台不确定时先询问"))
        .await
        .unwrap();
    assert!(update.text.as_deref().unwrap().contains("待确认修改记忆"));
    assert!(update.markdown.as_deref().unwrap().contains("- 原内容："));
    assert_eq!(
        service.memory_store.get(&memory_id).unwrap().content,
        record.content
    );

    let updated = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(updated.contains("已更新记忆"));
    assert_eq!(
        service.memory_store.get(&memory_id).unwrap().content,
        "前台不确定时先询问"
    );

    service.respond(message("/memory")).await.unwrap();
    let delete = service.respond(message("/memory delete 1")).await.unwrap();
    assert!(delete.text.as_deref().unwrap().contains("确认删除这条记忆"));
    assert!(delete.markdown.as_deref().unwrap().contains("- 内容："));
    assert!(service.memory_store.get(&memory_id).is_ok());

    let deleted = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(deleted.contains("已删除记忆"));
    assert!(service.memory_store.get(&memory_id).is_err());
}

#[tokio::test]
async fn memory_confirm_database_error_does_not_return_success() {
    let service = test_service();

    service
        .respond(message("/memory 如果不确定前台，请礼貌询问"))
        .await
        .unwrap();
    service.memory_store.drop_schema_for_test().unwrap();

    let err = service.respond(message("确认")).await.unwrap_err();

    assert_eq!(err.stage, "memory");
    assert!(err.message.contains("memory store failed"));
    assert!(!err.message.contains("已记下"));
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
        .respond(message("/memory 如果不确定前台，请礼貌询问"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    let records = service
        .memory_store
        .list(ListMemoryQuery::default())
        .unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].content.contains("前台不确定"));
    assert!(!base.join("memories.jsonl").exists());
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
async fn memory_create_accepts_fenced_json_but_saves_content_only() {
    let service = test_service();

    let draft = service
        .respond(message("/memory fenced-memory-create"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(draft.contains("前台不确定时先询问本人再记录"));
    assert!(!draft.contains("```"));
    assert!(!draft.contains("\"content\""));

    service.respond(message("确认")).await.unwrap();
    let record = service
        .memory_store
        .list(ListMemoryQuery::default())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(record.content, "前台不确定时先询问本人再记录");
    assert!(!record.content.contains("```"));
    assert!(!record.content.contains("\"content\""));
}

#[tokio::test]
async fn memory_pending_create_revision_updates_draft_before_confirmation() {
    let service = test_service();

    let draft = service
        .respond(message("/memory 如果不确定前台，请礼貌询问"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(draft.contains("整理成这条记忆草稿"));
    assert!(draft.contains("前台不确定时请礼貌询问"));

    let revised = service
        .respond(message("不对，改成前台不确定时先询问本人再记录"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(revised.contains("整理成这条记忆草稿"));
    assert!(revised.contains("前台不确定时先询问本人再记录"));

    service.respond(message("确认")).await.unwrap();
    let record = service
        .memory_store
        .list(ListMemoryQuery::default())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(record.content, "前台不确定时先询问本人再记录");
    assert_eq!(
        record.source_text,
        "/memory 如果不确定前台，请礼貌询问\n不对，改成前台不确定时先询问本人再记录"
    );
}

#[tokio::test]
async fn memory_pending_create_plain_revision_and_failure_keep_pending() {
    let service = test_service();

    service
        .respond(message("/memory 如果不确定前台，请礼貌询问"))
        .await
        .unwrap();

    let revised = service
        .respond(message("先询问本人再记录"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(revised.contains("整理成这条记忆草稿"));
    assert!(revised.contains("前台不确定时先询问本人再记录"));

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::MemoryCreate { memory }) => {
            assert_eq!(memory.content, "前台不确定时先询问本人再记录");
            assert_eq!(
                memory.source_text,
                "/memory 如果不确定前台，请礼貌询问\n先询问本人再记录"
            );
        }
        other => panic!("expected memory create pending, got {other:?}"),
    }

    let failed = service
        .respond(message("invalid-memory-revision"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(
        failed,
        "这次没整理成功，当前草稿已保留。可以换个说法，或回复“确认 / 取消”。"
    );

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::MemoryCreate { memory }) => {
            assert_eq!(memory.content, "前台不确定时先询问本人再记录");
            assert_eq!(
                memory.source_text,
                "/memory 如果不确定前台，请礼貌询问\n先询问本人再记录"
            );
        }
        other => panic!("expected memory create pending, got {other:?}"),
    }
}

#[tokio::test]
async fn memory_pending_update_revision_updates_draft_before_confirmation() {
    let service = test_service();
    let record = service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "前台不确定时请礼貌询问".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "preference".to_owned(),
            scope: "front_detection".to_owned(),
        })
        .unwrap();
    let memory_id = short_memory_id(&record.id);
    service.respond(message("/memory")).await.unwrap();

    let update = service
        .respond(message("/memory edit 1 前台不确定时先询问"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(update.contains("待确认修改记忆"));
    assert!(update.contains("前台不确定时先询问"));

    let revised = service
        .respond(message("不对，改成前台不确定时先询问本人再记录"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(revised.contains("待确认修改记忆"));
    assert!(revised.contains("前台不确定时先询问本人再记录"));

    service.respond(message("确认")).await.unwrap();
    assert_eq!(
        service.memory_store.get(&memory_id).unwrap().content,
        "前台不确定时先询问本人再记录"
    );
}

#[tokio::test]
async fn memory_pending_update_plain_revision_uses_full_draft_revision() {
    let service = test_service();
    service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "前台不确定时请礼貌询问".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "preference".to_owned(),
            scope: "front_detection".to_owned(),
        })
        .unwrap();
    service.respond(message("/memory")).await.unwrap();

    service
        .respond(message("/memory edit 1 前台不确定时先询问"))
        .await
        .unwrap();
    let revised = service
        .respond(message("先询问本人再记录"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(revised.contains("待确认修改记忆"));
    assert!(revised.contains("前台不确定时先询问本人再记录"));

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    match session.pending_operation {
        Some(PendingOperation::MemoryUpdate { update }) => {
            assert_eq!(update.content, "前台不确定时先询问本人再记录");
            assert_eq!(update.memory_type, "preference");
            assert_eq!(update.scope, "front_detection");
        }
        other => panic!("expected memory update pending, got {other:?}"),
    }
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
async fn memory_update_and_delete_cancel_do_not_change_record() {
    let service = test_service();
    service
        .respond(message("/memory 如果不确定前台，请礼貌询问"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    let record = service
        .memory_store
        .list(ListMemoryQuery::default())
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let memory_id = short_memory_id(&record.id);
    service.respond(message("/memory")).await.unwrap();

    service
        .respond(message("/memory edit 1 前台不确定时先询问"))
        .await
        .unwrap();
    let cancelled_update = service
        .respond(message("取消"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(cancelled_update, "已取消，不修改记忆。");
    assert_eq!(
        service.memory_store.get(&memory_id).unwrap().content,
        record.content
    );

    service.respond(message("/memory")).await.unwrap();
    service.respond(message("/memory delete 1")).await.unwrap();
    let cancelled_delete = service
        .respond(message("取消"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(cancelled_delete, "已取消，不删除记忆。");
    assert!(service.memory_store.get(&memory_id).is_ok());
}

#[tokio::test]
async fn memory_delete_pending_waits_on_plain_text_without_chat() {
    let service = test_service();
    let record = service
        .memory_store
        .create(CreateMemoryRequest {
            user_id: Some("u1".to_owned()),
            group_id: Some("g1".to_owned()),
            content: "前台不确定时请礼貌询问".to_owned(),
            source_text: "seed".to_owned(),
            memory_type: "preference".to_owned(),
            scope: "front_detection".to_owned(),
        })
        .unwrap();
    let memory_id = short_memory_id(&record.id);

    service
        .respond(message(&format!("/memory delete {memory_id}")))
        .await
        .unwrap();
    let wait = service
        .respond(message("随便聊一下"))
        .await
        .unwrap()
        .text
        .unwrap();

    assert!(wait.contains("记忆删除还在等待确认"));
    assert!(!wait.contains("回复："));
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(matches!(
        session.pending_operation,
        Some(PendingOperation::MemoryDelete { .. })
    ));
    assert!(service.memory_store.get(&memory_id).is_ok());
}

#[tokio::test]
async fn memory_root_aliases_list_records_without_llm() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(calls.clone()));

    for command in ["/memory", "/记忆", "/记"] {
        let response = service.respond(message(command)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some("memory_list"));
        assert_eq!(response.text.unwrap(), "当前没有长期记忆。");
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
        .respond(message("/记忆"))
        .await
        .unwrap()
        .text
        .unwrap();
    let populated = service.respond(message("/记忆")).await.unwrap();
    assert!(text.contains("长期记忆："));
    assert!(text.contains("日常聊天中不要只用编号称呼成员"));
    assert!(text.contains("操作：/memory show 1"));
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
    let second = service
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
        .respond(message("/memory"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("1 "));
    assert!(list.contains("第二条记忆"));

    let detail = service.respond(message("/memory show 1")).await.unwrap();
    assert!(detail.text.as_deref().unwrap().contains("第二条记忆"));
    assert!(detail.markdown.as_deref().unwrap().contains("- 内容："));

    let edit = service
        .respond(message("/memory edit 1 第二条记忆已更新"))
        .await
        .unwrap();
    assert!(edit.text.as_deref().unwrap().contains("待确认修改记忆"));
    assert!(edit.markdown.as_deref().unwrap().contains("- 新内容："));
    service.respond(message("确认")).await.unwrap();
    assert_eq!(
        service.memory_store.get(&second.id).unwrap().content,
        "第二条记忆已更新"
    );

    service.respond(message("/memory")).await.unwrap();
    let delete = service.respond(message("/memory delete 1")).await.unwrap();
    assert!(delete.text.as_deref().unwrap().contains("确认删除这条记忆"));
    assert!(delete.markdown.as_deref().unwrap().contains("- 内容："));
}

#[tokio::test]
async fn memory_update_command_hints_edit_without_creating_pending() {
    let service = test_service();

    let response = service
        .respond(message("/memory update 1 新内容"))
        .await
        .unwrap();

    assert_eq!(
        response.text.as_deref(),
        Some("记忆修改请使用：/memory edit 列表序号 新内容")
    );
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
}

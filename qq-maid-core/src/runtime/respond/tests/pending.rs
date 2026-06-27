use super::support::*;
use crate::runtime::todo::{TodoItemDraft, TodoStore, TodoTimePrecision};

#[tokio::test]
async fn pending_operation_allows_safe_session_commands() {
    for (input, expected_command, expected_text) in [
        ("/help", "help", "女仆长助手"),
        ("/state", "state", "当前"),
        ("/resume", "resume", "最近没有可恢复的旧会话"),
        ("/list", "list", "最近没有可恢复的旧会话"),
    ] {
        let service = test_service();
        service
            .respond(message("/todo add 无时间买牛奶"))
            .await
            .unwrap();

        let response = service.respond(message(input)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some(expected_command));
        assert!(response.text.unwrap().contains(expected_text));
        let session = service
            .session_store
            .get_or_create_active(&test_meta())
            .unwrap();
        assert!(session.pending_operation.is_some());
    }

    let service = test_service();
    service
        .respond(message("/todo add 无时间买牛奶"))
        .await
        .unwrap();
    let response = service.respond(message("/clear")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("clear"));
    assert!(response.text.unwrap().contains("当前上下文已清空"));
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());

    let service = test_service();
    service
        .respond(message("/todo add 无时间买牛奶"))
        .await
        .unwrap();
    let old_session_id = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap()
        .session_id;
    let response = service.respond(message("/new 新话题")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("new"));
    assert!(response.text.as_deref().unwrap().contains("新会话已开"));
    let new_session_id = response.session_id.unwrap();
    assert_ne!(new_session_id, old_session_id);
    let active = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(active.session_id, new_session_id);
    assert!(active.pending_operation.is_none());
}

#[tokio::test]
async fn pending_operation_keeps_confirm_cancel_and_revision_priority() {
    let service = test_service();

    service
        .respond(message("/todo add 无时间买牛奶"))
        .await
        .unwrap();
    let confirmed = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(confirmed.contains("已新增待办：买牛奶"));
    assert!(!confirmed.contains("[1]"));

    service
        .respond(message("/todo delete 买牛奶"))
        .await
        .unwrap();
    let waiting = service
        .respond(message("先等等"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(waiting.contains("删除操作还在等待确认"));
    service.respond(message("取消")).await.unwrap();

    service
        .respond(message("/todo add 无时间买牛奶"))
        .await
        .unwrap();
    let cancelled = service
        .respond(message("取消"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(cancelled, "已取消，不新增待办。");

    service
        .respond(message("/todo add 检查服务器"))
        .await
        .unwrap();
    let revised = service
        .respond(message("改成明天检查服务"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(revised.contains("待确认新增待办"));
    assert!(revised.contains("检查服务"));
}

#[tokio::test]
async fn pending_operation_rejects_other_group_member_for_todo() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

    service
        .respond(message("/todo add 无时间买牛奶"))
        .await
        .unwrap();

    let rejected = service
        .respond(message_in_scope("确认", "group:g1", "u2", "g1"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(rejected.contains("由其他成员发起"));
    assert!(service.todo_store.list_pending(&owner).unwrap().is_empty());
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(session.pending_operation.is_some());

    let confirmed = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(confirmed.contains("已新增待办：买牛奶"));
    assert_eq!(service.todo_store.list_pending(&owner).unwrap().len(), 1);
}

#[tokio::test]
async fn pending_operation_blocks_business_slash_commands_until_resolved() {
    let service = test_service();

    service
        .respond(message("/todo add 无时间买牛奶"))
        .await
        .unwrap();
    for command in ["/todo", "/memory", "/天气杭州"] {
        let response = service.respond(message(command)).await.unwrap();
        assert!(response.text.unwrap().contains("还在等待确认"));
    }

    service.respond(message("取消")).await.unwrap();
    let list = service.respond(message("/todo")).await.unwrap();
    assert!(list.text.unwrap().contains("当前没有未完成待办"));
}

#[tokio::test]
async fn pending_delete_reply_classification_prefers_cancel_and_avoids_loose_confirm() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

    service
        .respond(message("/todo add 无时间买牛奶"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    service
        .respond(message("/todo delete 买牛奶"))
        .await
        .unwrap();
    let not_confirmed = service
        .respond(message("好像不是这个"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(not_confirmed.contains("删除操作还在等待确认"));
    let pending_items = service.todo_store.list_pending(&owner).unwrap();
    assert_eq!(pending_items[0].title, "买牛奶");

    let cancelled = service
        .respond(message("算了不要执行"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(cancelled, "已取消，不删除待办。");
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());

    service
        .respond(message("/todo delete 买牛奶"))
        .await
        .unwrap();
    let confirmed = service
        .respond(message("可以，就这个了"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(confirmed.contains("已取消待办：买牛奶"));
    assert!(!confirmed.contains("[1]"));
    assert!(service.todo_store.list_pending(&owner).unwrap().is_empty());
}

#[tokio::test]
async fn pending_delete_bulk_and_candidate_wait_on_plain_text() {
    let (service, _base) = test_service_with_base();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查服务器".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查数据库".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service
        .respond(message("/todo delete 数据库"))
        .await
        .unwrap();
    let wait = service
        .respond(message("先等等"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(wait.contains("删除操作还在等待确认"));
    service.respond(message("取消")).await.unwrap();

    seed_completed_time_todos(&service.todo_store);
    service
        .respond(message("/todo 删除 昨天之前完成"))
        .await
        .unwrap();
    let wait = service
        .respond(message("先等等"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(wait.contains("这批待办删除操作还在等待确认"));
    service.respond(message("取消")).await.unwrap();

    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查服务器".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "检查数据库".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service.respond(message("/todo delete 检查")).await.unwrap();
    let wait = service
        .respond(message("先等等"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(wait.contains("待办候选还在等待选择"));
}

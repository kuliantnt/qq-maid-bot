use super::*;

#[tokio::test]
async fn todo_root_aliases_list_pending_items() {
    let service = test_service();

    for command in ["/todo", "/待办", "/代办", "/任务"] {
        let response = service.respond(message(command)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_list"));
        let text = response.text.unwrap();
        assert!(text.contains("暂无未完成待办"));
        assert!(!text.starts_with("null"));
    }
}

#[tokio::test]
async fn todo_daily_reminder_command_updates_private_preference() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "private:u1");

    let enabled = service
        .respond(private_todo_message("/todo daily on"))
        .await
        .unwrap();
    assert_eq!(enabled.command.as_deref(), Some("todo_daily_reminder"));
    assert!(
        enabled
            .text
            .as_deref()
            .unwrap()
            .contains("已开启 Todo 每日摘要")
    );
    assert!(service.task_store.daily_reminder_enabled(&owner).unwrap());

    let status = service
        .respond(private_todo_message("/todo daily status"))
        .await
        .unwrap();
    assert!(status.text.as_deref().unwrap().contains("当前为开启"));

    let disabled = service
        .respond(private_todo_message("/todo daily off"))
        .await
        .unwrap();
    assert!(
        disabled
            .text
            .as_deref()
            .unwrap()
            .contains("单条 Todo 提醒不受影响")
    );
    assert!(!service.task_store.daily_reminder_enabled(&owner).unwrap());
}

#[tokio::test]
async fn todo_daily_reminder_command_does_not_enable_group_push() {
    let service = test_service();

    let response = service
        .respond(message_in_scope("/todo daily on", "group:g1", "u1", "g1"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_daily_reminder"));
    assert!(response.text.as_deref().unwrap().contains("只支持私聊开启"));
}

#[tokio::test]
async fn group_todo_defaults_to_actor_personal_owner() {
    let service = test_service();
    let owner_a = TodoStore::owner(Some("u1"), "group:g1");
    let owner_b = TodoStore::owner(Some("u2"), "group:g1");
    service
        .task_store
        .create(&owner_a, draft("A 的个人待办"))
        .unwrap();
    service
        .task_store
        .create(&owner_b, draft("B 的个人待办"))
        .unwrap();

    let a_list = service.respond(message("/todo")).await.unwrap();
    let a_text = a_list.text.unwrap();
    assert!(a_text.contains("A 的个人待办"));
    assert!(!a_text.contains("B 的个人待办"));

    let b_list = service
        .respond(message_in_scope("/todo", "group:g1", "u2", "g1"))
        .await
        .unwrap();
    let b_text = b_list.text.unwrap();
    assert!(b_text.contains("B 的个人待办"));
    assert!(!b_text.contains("A 的个人待办"));
}

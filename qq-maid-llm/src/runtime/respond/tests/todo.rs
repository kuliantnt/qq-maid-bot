use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::support::*;
use crate::runtime::todo::{TodoItemDraft, TodoStatus, TodoStore, TodoTimePrecision};

#[tokio::test]
async fn todo_root_aliases_list_pending_items() {
    let service = test_service();

    for command in ["/todo", "/待办", "/任务"] {
        let response = service.respond(message(command)).await.unwrap();
        assert_eq!(response.command.as_deref(), Some("todo_list"));
        let text = response.text.unwrap();
        assert!(text.contains("当前没有未完成待办"));
        assert!(!text.starts_with("null"));
    }
}

#[tokio::test]
async fn todo_add_waits_for_confirmation_before_writing() {
    let service = test_service();

    let draft = service
        .respond(message("/todo add 无时间买牛奶"))
        .await
        .unwrap();
    assert!(draft.text.unwrap().contains("待确认新增待办"));

    let before_confirm = service.respond(message("/todo")).await.unwrap();
    assert!(before_confirm.text.unwrap().contains("还在等待确认"));
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    assert!(service.todo_store.list_pending(&owner).unwrap().is_empty());

    let confirmed = service.respond(message("确认")).await.unwrap();
    let confirmed_text = confirmed.text.unwrap();
    let confirmed_markdown = confirmed.markdown.unwrap();
    assert!(confirmed_text.contains("已新增待办：买牛奶"));
    assert!(!confirmed_text.contains("[1]"));
    assert!(!confirmed_markdown.contains("[1]"));

    let list = service.respond(message("/todo")).await.unwrap();
    let text = list.text.unwrap();
    let markdown = list.markdown.unwrap();
    assert!(text.contains("1. 买牛奶"));
    assert!(!text.contains("[1] 买牛奶"));
    assert!(!markdown.contains("[1]"));
    assert!(text.contains("时间：未指定"));
}

#[tokio::test]
async fn todo_list_markdown_and_text_hide_internal_ids_for_non_contiguous_items() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");

    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "第一项".to_owned(),
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
                title: "第二项".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.cancel(&owner, "2").unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "第三项".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();
    service.todo_store.cancel(&owner, "3").unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "第四项".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    let response = service.respond(message("/todo")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.contains("1. 第一项"));
    assert!(text.contains("2. 第四项"));
    assert!(!text.contains("[1]"));
    assert!(!text.contains("[4]"));
    assert!(markdown.contains("1. **第一项**"));
    assert!(markdown.contains("2. **第四项**"));
    assert!(!markdown.contains("[1]"));
    assert!(!markdown.contains("[4]"));
}

#[tokio::test]
async fn todo_add_parses_relative_absolute_and_inferred_dates() {
    let service = test_service();

    service
        .respond(message("/todo add 三天后检查日志"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    service
        .respond(message("/todo add 2026年6月15日提交报告"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    service
        .respond(message("/todo add 月底复盘"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    let text = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(text.contains("检查日志"));
    assert!(text.contains("提交报告"));
    assert!(text.contains("06-15（一）"));
    assert!(text.contains("复盘"));
    assert!(!text.contains("推测"));
}

#[tokio::test]
async fn todo_search_and_unknown_second_segment_do_not_call_llm() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(calls.clone()));
    service
        .respond(message("/todo add 检查服务器"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    service.respond(message("/todo add 查交通")).await.unwrap();
    service.respond(message("确认")).await.unwrap();

    calls.store(0, Ordering::SeqCst);
    let text = service
        .respond(message("/todo 服务器"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(text.contains("待办搜索结果"));
    assert!(text.contains("检查服务器"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let text = service
        .respond(message("/待办 查 交通"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(text.contains("查交通"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let text = service
        .respond(message("/任务 搜 查询"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(text.contains("没有找到匹配"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn todo_pending_cancel_does_not_write() {
    let service = test_service();

    service
        .respond(message("/todo add 无时间买牛奶"))
        .await
        .unwrap();
    let cancelled = service.respond(message("取消")).await.unwrap();
    assert!(cancelled.text.unwrap().contains("已取消"));

    let list = service.respond(message("/todo")).await.unwrap();
    assert!(list.text.unwrap().contains("当前没有未完成待办"));
}

#[tokio::test]
async fn todo_done_uses_list_number_without_confirmation_and_delete_still_confirms() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .respond(message("/todo add 检查服务器"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    service.respond(message("/todo")).await.unwrap();

    let edit = service
        .respond(message("/todo edit 1 改成明天检查服务"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(edit.contains("待确认修改待办"));
    let edited = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(edited.contains("已修改待办：检查服务"));
    assert!(!edited.contains("[1]"));

    service.respond(message("/todo")).await.unwrap();
    let done = service
        .respond(message("/todo done 1"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(done, "已完成待办：\n第 1 条：检查服务");
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
    assert_eq!(
        service
            .todo_store
            .list_all(&owner)
            .unwrap()
            .iter()
            .find(|item| item.id == "1")
            .unwrap()
            .status,
        TodoStatus::Completed
    );
    let list = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("当前没有未完成待办"));

    let keyword_done = service
        .respond(message("/todo done 检查服务"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(
        keyword_done,
        "编号只能使用正整数，并用空格、逗号或中文逗号分隔。"
    );

    service
        .respond(message("/todo add 检查数据库"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    let delete = service
        .respond(message("/todo delete 数据库"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(delete.contains("确认删除这条待办"));
    assert!(!delete.contains('['));
    service.respond(message("确认")).await.unwrap();
    let list = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("当前没有未完成待办"));
}

#[tokio::test]
async fn todo_pending_edit_revision_updates_draft_before_confirmation() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "示例决算和合同".to_owned(),
                detail: Some("和客户当场对接".to_owned()),
                raw_text: None,
                due_date: Some("2026-06-11".to_owned()),
                due_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "示例材料需要重新做".to_owned(),
                detail: Some("旧详情".to_owned()),
                raw_text: None,
                due_date: Some("2026-06-30".to_owned()),
                due_at: None,
                time_precision: TodoTimePrecision::Inferred,
            },
        )
        .unwrap();

    let _first_confirm = service.respond(message("/todo")).await.unwrap();
    let first_confirm = service
        .respond(message("/todo edit 2 改成月底前需要和负责人理一下"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(first_confirm.contains("待确认修改待办"));
    assert!(first_confirm.contains("需要和负责人理一下"));

    let revised_confirm = service
        .respond(message("然后示例材料详情这么理解，先做一份给负责人看看"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(revised_confirm.contains("待确认修改待办"));
    assert!(revised_confirm.contains("先做一份示例材料给负责人看看，再根据反馈调整"));

    let confirmed = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(confirmed.contains("已修改待办：示例材料需要重新做"));

    let list = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("详情：先做一份示例材料给负责人看看，再根据反馈调整"));
    assert!(!list.contains("详情：需要和负责人理一下"));
}

#[tokio::test]
async fn todo_pending_edit_revision_cancel_keeps_database_record() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "示例材料需要重新做".to_owned(),
                detail: Some("旧详情".to_owned()),
                raw_text: None,
                due_date: Some("2026-06-30".to_owned()),
                due_at: None,
                time_precision: TodoTimePrecision::Inferred,
            },
        )
        .unwrap();

    service.respond(message("/todo")).await.unwrap();
    service
        .respond(message("/todo edit 1 改成月底前需要和负责人理一下"))
        .await
        .unwrap();
    service
        .respond(message("然后示例材料详情这么理解，先做一份给负责人看看"))
        .await
        .unwrap();
    let cancelled = service
        .respond(message("取消"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(cancelled, "已取消，不修改待办。");

    let item = service
        .todo_store
        .list_pending(&owner)
        .unwrap()
        .into_iter()
        .find(|item| item.id == "1")
        .unwrap();
    assert_eq!(item.title, "示例材料需要重新做");
    assert_eq!(item.detail.as_deref(), Some("旧详情"));
    assert_eq!(item.due_date.as_deref(), Some("2026-06-30"));
}

#[tokio::test]
async fn todo_pending_edit_merges_time_patch_before_confirmation() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "占位".to_owned(),
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
                title: "示例材料需要重新做".to_owned(),
                detail: None,
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    let _first = service.respond(message("/todo")).await.unwrap();
    let first = service
        .respond(message("/todo edit 2 修改为 示例系统维保 - 2026 做完了"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(first.contains("待确认修改待办"));
    assert!(first.contains("示例系统维保 - 2026"));

    let revised = service
        .respond(message("时间需要改成这个月底之前完成"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(revised.contains("待确认修改待办"));
    assert!(revised.contains("示例系统维保 - 2026"));
    assert!(revised.contains("06-30（二）"));

    let confirmed = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(confirmed.contains("已修改待办：示例系统维保 - 2026"));
    assert!(confirmed.contains("时间：06-30（二）"));

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
}

#[tokio::test]
async fn todo_pending_edit_merges_title_detail_and_consumes_confirm_phrase() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "占位".to_owned(),
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
                title: "示例材料需要重新做".to_owned(),
                detail: Some("旧详情".to_owned()),
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service.respond(message("/todo")).await.unwrap();
    service
        .respond(message("/todo edit 2 修改为 示例系统维保 - 2026 做完了"))
        .await
        .unwrap();
    let retitled = service
        .respond(message(
            "理解错了，实际上标题还是示例项目审查，内容是之前的标题",
        ))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(retitled.contains("示例项目审查"));
    assert!(retitled.contains("之前的标题"));

    let detailed = service
        .respond(message(
            "内容改成 示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成",
        ))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(detailed.contains("示例项目审查"));
    assert!(detailed.contains("示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成"));
    assert!(detailed.contains("06-30（二）"));

    let confirmed = service
        .respond(message("可以，就这个了"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(confirmed.contains("已修改待办：示例项目审查"));
    assert!(confirmed.contains("时间：06-30（二）"));
    assert!(confirmed.contains("详情：示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成"));

    let repeated = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(!repeated.contains("已修改待办"));

    let list = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("1. 示例项目审查"));
    assert!(list.contains("详情：示例系统维保 - 2026；已经完成；其他内容都在这个月底前完成"));
}

#[tokio::test]
async fn todo_pending_add_revision_updates_draft_before_confirmation() {
    let service = test_service();

    let draft = service
        .respond(message("/todo add 检查服务器"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(draft.contains("待确认新增待办"));
    assert!(draft.contains("检查服务器"));

    let revised = service
        .respond(message("改成明天检查服务"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(revised.contains("待确认新增待办"));
    assert!(revised.contains("检查服务"));

    let revised = service
        .respond(message(
            "标题改成准备材料，详情补充先发负责人，时间这个月底前",
        ))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(revised.contains("待确认新增待办"));
    assert!(revised.contains("准备材料"));
    assert!(revised.contains("先发负责人"));
    assert!(revised.contains("06-30（二）"));

    let confirmed = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(confirmed.contains("已新增待办：准备材料"));

    let list = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("详情：先发负责人"));
    assert!(list.contains("06-30（二）"));
}

#[tokio::test]
async fn todo_pending_add_revision_failure_keeps_current_draft() {
    let service = test_service();

    service
        .respond(message("/todo add 检查服务器"))
        .await
        .unwrap();
    service.respond(message("改成明天检查服务")).await.unwrap();
    let before_failure = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap()
        .pending_operation
        .unwrap();

    let failed = service
        .respond(message("invalid-json"))
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
    assert_eq!(session.pending_operation, Some(before_failure));
}

#[tokio::test]
async fn todo_pending_edit_revision_failure_keeps_original_pending() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "示例材料需要重新做".to_owned(),
                detail: Some("旧详情".to_owned()),
                raw_text: None,
                due_date: None,
                due_at: None,
                time_precision: TodoTimePrecision::None,
            },
        )
        .unwrap();

    service.respond(message("/todo")).await.unwrap();
    service
        .respond(message("/todo edit 1 月底前需要和负责人理一下"))
        .await
        .unwrap();
    let before_failure = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap()
        .pending_operation
        .unwrap();

    let failed = service
        .respond(message("invalid-json"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(
        failed,
        "这次没整理成功，当前草稿已保留。可以换个说法，或回复“确认 / 取消”。"
    );
    assert!(!failed.contains("回复："));

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert_eq!(session.pending_operation, Some(before_failure));

    let blocked = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(blocked.contains("还在等待确认"));
}

#[tokio::test]
async fn todo_all_lists_completed_items_with_chinese_alias() {
    let service = test_service();
    service
        .respond(message("/todo add 检查服务器"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    service.respond(message("/todo")).await.unwrap();
    assert_eq!(
        service
            .respond(message("/todo done 1"))
            .await
            .unwrap()
            .text
            .as_deref(),
        Some("已完成待办：\n第 1 条：检查服务器")
    );
    service
        .respond(message("/todo add 检查数据库"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    service
        .respond(message("/todo delete 数据库"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    let pending = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(pending.contains("当前没有未完成待办"));

    let all = service.respond(message("/todo all")).await.unwrap();
    assert_eq!(all.command.as_deref(), Some("todo_all"));
    let text = all.text.unwrap();
    assert!(text.contains("全部待办"));
    assert!(text.contains("检查服务器"));
    assert!(text.contains("已完成"));
    assert!(text.contains("完成时间："));
    assert!(!text.contains("+08:00"));
    assert!(text.contains("检查数据库"));
    assert!(text.contains("已取消"));

    let alias = service.respond(message("/todo 全部")).await.unwrap();
    assert_eq!(alias.command.as_deref(), Some("todo_all"));
    assert!(alias.text.unwrap().contains("已完成"));
}

#[tokio::test]
async fn todo_root_only_lists_pending_and_search_stays_pending_only() {
    let service = test_service();
    service
        .respond(message("/todo add 检查服务器"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    service
        .respond(message("/todo add 检查数据库"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    service.respond(message("/todo")).await.unwrap();
    assert_eq!(
        service
            .respond(message("/todo done 1"))
            .await
            .unwrap()
            .text
            .as_deref(),
        Some("已完成待办：\n第 1 条：检查服务器")
    );
    service
        .respond(message("/todo delete 数据库"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    let pending = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(pending, "当前没有未完成待办。");

    let search_alias = service
        .respond(message("/todo 服务器"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(search_alias, "没有找到匹配的未完成待办。");

    let search_explicit = service
        .respond(message("/todo search 服务器"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(search_explicit, "没有找到匹配的未完成待办。");
}

#[tokio::test]
async fn todo_done_without_argument_lists_completed_items_desc() {
    let (service, _base) = test_service_with_base();
    seed_completed_time_todos(&service.todo_store);

    let response = service.respond(message("/todo done")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_done"));
    let text = response.text.unwrap();
    assert!(text.starts_with("已完成待办："));
    assert!(text.contains("1. 今天完成"));
    assert!(text.contains("2. 昨天完成"));
    assert!(text.contains("3. 前天完成"));
    assert!(text.contains("4. 没有完成时间"));
    assert!(text.contains("完成时间：未知"));
    assert!(!text.contains("已取消完成"));
}

#[tokio::test]
async fn todo_delete_reuses_pending_list_index() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "月底处理".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-30".to_owned()),
                due_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "今天处理".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-12".to_owned()),
                due_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();

    let list = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("1. 今天处理"));
    assert!(list.contains("2. 月底处理"));

    let confirm = service
        .respond(message("/todo delete 1"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(confirm.contains("确认删除这条待办"));
    assert!(confirm.contains("今天处理"));
    assert!(!confirm.contains("[2]"));
    service.respond(message("确认")).await.unwrap();

    let all = service.todo_store.list_all(&owner).unwrap();
    let first_visible = all.iter().find(|item| item.id == "2").unwrap();
    assert_eq!(
        first_visible.status,
        crate::runtime::todo::TodoStatus::Cancelled
    );
    let second_visible = all.iter().find(|item| item.id == "1").unwrap();
    assert_eq!(
        second_visible.status,
        crate::runtime::todo::TodoStatus::Pending
    );
}

#[tokio::test]
async fn todo_delete_reuses_completed_list_index() {
    let (service, _base) = test_service_with_base();
    let seeded = seed_completed_time_todos(&service.todo_store);

    let list = service
        .respond(message("/todo done"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("1. 今天完成"));
    assert!(list.contains("2. 昨天完成"));

    let confirm = service
        .respond(message("/todo delete 2"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(confirm.contains("确认删除这 1 条已完成待办？来源：已完成列表第 2 条"));
    assert!(confirm.contains("昨天完成"));
    assert!(!confirm.contains("[2]"));
    service.respond(message("确认")).await.unwrap();

    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let all = service.todo_store.list_all(&owner).unwrap();
    let deleted = all
        .iter()
        .find(|item| item.id == seeded.yesterday_id)
        .unwrap();
    assert_eq!(deleted.status, crate::runtime::todo::TodoStatus::Cancelled);
    let kept = all.iter().find(|item| item.id == seeded.old_id).unwrap();
    assert_eq!(kept.status, crate::runtime::todo::TodoStatus::Completed);
}

#[tokio::test]
async fn todo_done_without_argument_returns_empty_hint() {
    let service = test_service();

    let response = service.respond(message("/todo done")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_done"));
    assert_eq!(response.text.as_deref(), Some("当前没有已完成待办。"));
}

#[tokio::test]
async fn todo_done_and_undo_use_list_snapshots_and_return_titles() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "月底处理".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-30".to_owned()),
                due_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "今天处理".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-12".to_owned()),
                due_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();
    service
        .todo_store
        .create(
            &owner,
            TodoItemDraft {
                title: "明天处理".to_owned(),
                detail: None,
                raw_text: None,
                due_date: Some("2026-06-13".to_owned()),
                due_at: None,
                time_precision: TodoTimePrecision::Date,
            },
        )
        .unwrap();

    let list = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("1. 今天处理"));
    assert!(list.contains("2. 明天处理"));
    assert!(list.contains("3. 月底处理"));

    let done = service
        .respond(message("/todo done 1, 3，1 9"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(
        done,
        "已完成待办：\n第 1 条：今天处理\n第 3 条：月底处理\n未找到匹配的未完成待办：9"
    );
    let all = service.todo_store.list_all(&owner).unwrap();
    assert_eq!(
        all.iter().find(|item| item.id == "2").unwrap().status,
        TodoStatus::Completed
    );
    assert_eq!(
        all.iter().find(|item| item.id == "1").unwrap().status,
        TodoStatus::Completed
    );
    assert_eq!(
        all.iter().find(|item| item.id == "3").unwrap().status,
        TodoStatus::Pending
    );

    let reused = service
        .respond(message("/todo done 2"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(reused, "请先发送 /todo 查看未完成待办。");

    let completed = service.respond(message("/todo undo")).await.unwrap();
    assert_eq!(completed.command.as_deref(), Some("todo_undo"));
    let completed_text = completed.text.unwrap();
    assert!(completed_text.contains("1. 今天处理"));
    assert!(completed_text.contains("2. 月底处理"));

    let undo = service
        .respond(message("/todo undo 1，2, 2 8"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(
        undo,
        "已恢复待办：\n第 1 条：今天处理\n第 2 条：月底处理\n未找到匹配的已完成待办：8"
    );
    let all = service.todo_store.list_all(&owner).unwrap();
    let month_end = all.iter().find(|item| item.id == "1").unwrap();
    assert_eq!(month_end.status, TodoStatus::Pending);
    assert!(month_end.completed_at.is_none());
    let today = all.iter().find(|item| item.id == "2").unwrap();
    assert_eq!(today.status, TodoStatus::Pending);
    assert!(today.completed_at.is_none());
}

#[tokio::test]
async fn todo_done_and_undo_require_matching_list_snapshot() {
    let service = test_service();
    let owner = TodoStore::owner(Some("u1"), "group:g1");
    service
        .respond(message("/todo add 检查服务器"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    let no_pending_snapshot = service
        .respond(message("/todo done 1"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(no_pending_snapshot, "请先发送 /todo 查看未完成待办。");
    assert_eq!(
        service.todo_store.list_all(&owner).unwrap()[0].status,
        TodoStatus::Pending
    );

    service.respond(message("/todo done")).await.unwrap();
    let completed_snapshot_is_not_pending = service
        .respond(message("/todo done 1"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(
        completed_snapshot_is_not_pending,
        "请先发送 /todo 查看未完成待办。"
    );

    service.respond(message("/todo")).await.unwrap();
    service.respond(message("/todo done 1")).await.unwrap();
    let no_completed_snapshot = service
        .respond(message("/todo undo 1"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(
        no_completed_snapshot,
        "请先发送 /todo done 查看已完成待办。"
    );
}

#[tokio::test]
async fn todo_all_lists_all_statuses_by_created_at_desc() {
    let (service, _base) = test_service_with_base();
    seed_completed_time_todos(&service.todo_store);

    let response = service.respond(message("/todo all")).await.unwrap();
    assert_eq!(response.command.as_deref(), Some("todo_all"));
    let text = response.text.unwrap();
    assert!(text.contains("1. 未完成旧截止"));
    assert!(text.contains("2. 已取消完成"));
    assert!(text.contains("3. 没有完成时间"));
    assert!(text.contains("4. 今天完成"));
    assert!(text.contains("5. 昨天完成"));
    assert!(text.contains("6. 前天完成"));
    assert!(text.contains("已取消"));
}

#[tokio::test]
async fn todo_completed_time_query_reuses_context_for_bulk_delete() {
    let (service, _base) = test_service_with_base();
    let seeded = seed_completed_time_todos(&service.todo_store);

    let query = service
        .respond(message("/todo 昨天之前完成"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(query.contains("已完成待办：昨天之前完成"));
    assert!(query.contains("1. 前天完成"));
    assert!(query.contains("完成时间："));
    assert!(!query.contains("+08:00"));
    assert!(!query.contains("昨天完成"));

    let confirm = service
        .respond(message("/todo 删除"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(confirm.contains("确认删除这 1 条已完成待办？来源：昨天之前完成"));
    assert!(confirm.contains("前天完成"));

    let deleted = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(deleted.contains("已删除 1 条已完成待办。来源：昨天之前完成"));

    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let visible = service.todo_store.list_all(&owner).unwrap();
    let deleted_item = visible
        .iter()
        .find(|item| item.id == seeded.old_id)
        .unwrap();
    assert_eq!(
        deleted_item.status,
        crate::runtime::todo::TodoStatus::Cancelled
    );
    let yesterday = visible
        .iter()
        .find(|item| item.id == seeded.yesterday_id)
        .unwrap();
    assert_eq!(
        yesterday.status,
        crate::runtime::todo::TodoStatus::Completed
    );

    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());
}

#[tokio::test]
async fn todo_delete_with_completed_time_query_directly_prepares_bulk_confirmation() {
    let (service, _base) = test_service_with_base();
    seed_completed_time_todos(&service.todo_store);

    let confirm = service
        .respond(message("/todo 删除 昨天以前完成"))
        .await
        .unwrap()
        .text
        .unwrap();

    assert!(confirm.contains("确认删除这 2 条已完成待办？来源：昨天以前完成"));
    assert!(confirm.contains("前天完成"));
    assert!(confirm.contains("昨天完成"));
}

#[tokio::test]
async fn todo_delete_done_prepares_all_completed_cleanup() {
    let (service, _base) = test_service_with_base();
    let seeded = seed_completed_time_todos(&service.todo_store);

    let confirm = service
        .respond(message("/todo delete done"))
        .await
        .unwrap()
        .text
        .unwrap();

    assert!(confirm.contains("确认删除这 4 条已完成待办？来源：全部已完成待办"));
    assert!(confirm.contains("今天完成"));
    assert!(!confirm.contains("已取消完成"));

    let deleted = service
        .respond(message("确认"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(deleted.contains("已删除 4 条已完成待办。来源：全部已完成待办"));

    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let all = service.todo_store.list_all(&owner).unwrap();
    assert_eq!(
        all.iter()
            .find(|item| item.id == seeded.old_id)
            .unwrap()
            .status,
        crate::runtime::todo::TodoStatus::Cancelled
    );
    assert_eq!(
        all.iter().find(|item| item.id == "6").unwrap().status,
        crate::runtime::todo::TodoStatus::Pending
    );
}

#[tokio::test]
async fn todo_completed_time_query_no_result_and_direct_delete_no_deletable_items() {
    let (service, _base) = test_service_with_base();
    seed_completed_time_todos(&service.todo_store);

    let query = service
        .respond(message("/todo search 1900-01-01之前完成"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(query, "没有找到符合完成时间条件的已完成待办。");

    let delete = service
        .respond(message("/todo 删除 1900-01-01之前完成"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(delete, "没有可删除的已完成待办。");
}

#[tokio::test]
async fn todo_normal_delete_keyword_is_unchanged() {
    let service = test_service();
    service
        .respond(message("/todo add 检查数据库"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    let delete = service
        .respond(message("/todo 删除 数据库"))
        .await
        .unwrap()
        .text
        .unwrap();

    assert!(delete.contains("确认删除这条待办"));
    assert!(delete.contains("检查数据库"));
    assert!(!delete.contains("[1]"));
}

#[tokio::test]
async fn todo_non_completed_query_clears_last_completed_query() {
    let (service, _base) = test_service_with_base();
    seed_completed_time_todos(&service.todo_store);

    service
        .respond(message("/todo 昨天之前完成"))
        .await
        .unwrap();
    service
        .respond(message("/todo search 不存在"))
        .await
        .unwrap();
    let delete = service
        .respond(message("/todo 删除"))
        .await
        .unwrap()
        .text
        .unwrap();

    assert_eq!(
        delete,
        "用法：/todo delete 列表序号或关键词；清理已完成任务用 /todo delete done"
    );
}

#[tokio::test]
async fn todo_expired_last_completed_query_is_not_reused() {
    let (service, _base) = test_service_with_base();
    seed_completed_time_todos(&service.todo_store);

    service
        .respond(message("/todo 昨天之前完成"))
        .await
        .unwrap();
    let mut session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    session.last_todo_query.as_mut().unwrap().created_at = "2000-01-01T00:00:00+08:00".to_owned();
    service.session_store.save(&mut session).unwrap();

    let delete = service
        .respond(message("/todo 删除"))
        .await
        .unwrap()
        .text
        .unwrap();

    assert_eq!(
        delete,
        "用法：/todo delete 列表序号或关键词；清理已完成任务用 /todo delete done"
    );
}

#[tokio::test]
async fn todo_bulk_delete_cancel_clears_pending_operation() {
    let (service, _base) = test_service_with_base();
    let seeded = seed_completed_time_todos(&service.todo_store);

    service
        .respond(message("/todo 删除 昨天之前完成"))
        .await
        .unwrap();
    let cancelled = service
        .respond(message("取消"))
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

    let owner = TodoStore::owner(Some("u1"), "group:g1");
    let all = service.todo_store.list_all(&owner).unwrap();
    let old = all.iter().find(|item| item.id == seeded.old_id).unwrap();
    assert_eq!(old.status, crate::runtime::todo::TodoStatus::Completed);
}

#[tokio::test]
async fn todo_done_keyword_no_longer_uses_candidate_or_confirmation() {
    let service = test_service();
    service
        .respond(message("/todo add 检查服务器"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();
    service
        .respond(message("/todo add 检查数据库"))
        .await
        .unwrap();
    service.respond(message("确认")).await.unwrap();

    let response = service
        .respond(message("/todo done 检查"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert_eq!(
        response,
        "编号只能使用正整数，并用空格、逗号或中文逗号分隔。"
    );
    let session = service
        .session_store
        .get_or_create_active(&test_meta())
        .unwrap();
    assert!(session.pending_operation.is_none());

    let list = service
        .respond(message("/todo"))
        .await
        .unwrap()
        .text
        .unwrap();
    assert!(list.contains("1. 检查服务器"));
    assert!(list.contains("2. 检查数据库"));
}

#[tokio::test]
async fn todo_sentence_after_root_is_search_not_add() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = test_service_with_provider(MockProvider::with_counter(calls.clone()));

    let response = service
        .respond(message("/待办 三天后检查日志"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_search"));
    assert!(response.text.unwrap().contains("没有找到匹配"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn todo_root_with_bracket_id_and_body_is_plain_search_now() {
    let service = test_service();

    let response = service
        .respond(message("/todo [2] 示例项目审查"))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("todo_search"));
    let text = response.text.unwrap();
    assert_eq!(text, "没有找到匹配的未完成待办。");
}

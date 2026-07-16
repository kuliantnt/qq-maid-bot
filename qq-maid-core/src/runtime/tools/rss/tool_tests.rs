use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use qq_maid_common::time_context::now_iso_cn;

use crate::{
    runtime::tools::rss::{RssFeedItem, RssFetchConfig, RssSubscription, RssTarget, RssTargetType},
    storage::{APP_MIGRATIONS, database::SqliteDatabase},
};

use super::*;

fn test_context() -> ToolContext {
    ToolContext {
        task_id: "msg-1".to_owned(),
        actor: ExecutionActorContext {
            user_id: Some("u1".to_owned()),
            group_member_role: None,
        },
        conversation: ExecutionConversationContext {
            platform: "test".to_owned(),
            account_id: None,
            kind: ConversationKind::Private,
            target_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            interaction_scope_id: "private:u1".to_owned(),
        },
        tool_call_id: Some("call-1".to_owned()),
        execution_deadline: None,
    }
}

fn test_store() -> RssStore {
    RssStore::new(SqliteDatabase::open_temp("rss-tool-tests", APP_MIGRATIONS).unwrap())
}

fn test_fetcher() -> RssFetcher {
    RssFetcher::new(RssFetchConfig {
        timeout_seconds: 5,
        max_body_bytes: 1024 * 1024,
        user_agent: "rss-tool-test".to_owned(),
        allow_private_networks: true,
    })
    .unwrap()
}

fn manage_arguments(operation: &str) -> Value {
    match operation {
        "add" => json!({
            "operation": "add",
            "feeds": [{"url": "https://example.test/feed.xml", "title": null}],
            "targets": null,
            "raw_text": null
        }),
        "delete" => json!({
            "operation": "delete",
            "feeds": null,
            "targets": ["1"],
            "raw_text": null
        }),
        _ => unreachable!(),
    }
}

fn spawn_feed_server(title: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel><title>{title}</title><item><title>Item</title><link>https://example.test/item</link><guid>{title}</guid></item></channel></rss>"#
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
    });
    format!("http://{addr}/feed.xml")
}

fn feed_item(key: &str, title: &str) -> RssFeedItem {
    RssFeedItem {
        item_key: key.to_owned(),
        revision_hash: format!("rev:{key}"),
        title: title.to_owned(),
        link: Some(format!("https://example.test/{key}")),
        published_at: Some("2026-06-18T00:00:00+00:00".to_owned()),
        updated_at: Some("2026-06-18T01:00:00+00:00".to_owned()),
        summary: Some("Codex 发布摘要".to_owned()),
        source_order: 0,
    }
}

#[tokio::test]
async fn rss_tool_reads_recent_items_from_current_scope() {
    let store = test_store();
    let target = RssTarget {
        target_type: RssTargetType::Private,
        target_id: "u1".to_owned(),
        scope_key: "private:u1".to_owned(),
    };
    let sub = store
        .create_subscription(
            &target,
            "https://example.test/codex.xml",
            "Codex 发布",
            &[],
            50,
        )
        .unwrap();
    store
        .enqueue_items(&sub.id, &[feed_item("codex-1", "Codex v1")], 50)
        .unwrap();
    let tool = RssRecentItemsTool::new(store);

    let output = tool
        .execute(test_context(), json!({"query": "codex", "limit": 1}))
        .await
        .unwrap();

    assert_eq!(output.value["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        output.value["items"][0]["subscription"]["title"],
        "Codex 发布"
    );
    assert_eq!(output.value["items"][0]["item"]["title"], "Codex v1");
    assert_eq!(output.value["query"], "codex");
}

#[tokio::test]
async fn rss_tool_rejects_invalid_limit() {
    let tool = RssRecentItemsTool::new(test_store());

    let err = tool
        .execute(test_context(), json!({"query": null, "limit": 0}))
        .await
        .unwrap_err();

    assert_eq!(err.code, "bad_tool_arguments");
}

#[tokio::test]
async fn rss_tool_returns_empty_list_when_no_match() {
    let store = test_store();
    let target = RssTarget {
        target_type: RssTargetType::Private,
        target_id: "u1".to_owned(),
        scope_key: "private:u1".to_owned(),
    };
    let sub = store
        .create_subscription(
            &target,
            "https://example.test/feed.xml",
            "普通订阅",
            &[RssFeedItem {
                item_key: "baseline".to_owned(),
                revision_hash: "rev:baseline".to_owned(),
                title: "普通标题".to_owned(),
                link: None,
                published_at: None,
                updated_at: None,
                summary: None,
                source_order: 0,
            }],
            50,
        )
        .unwrap();
    store.mark_item_pushed(&sub.id, "baseline").unwrap();
    let tool = RssRecentItemsTool::new(store);

    let output = tool
        .execute(test_context(), json!({"query": "codex", "limit": null}))
        .await
        .unwrap();

    assert!(output.value["items"].as_array().unwrap().is_empty());
    assert!(
        output.value["scope_id"]
            .as_str()
            .unwrap()
            .starts_with("private:")
    );
    assert!(!now_iso_cn().is_empty());
}

#[tokio::test]
async fn rss_manage_tool_adds_numbered_raw_text_in_current_scope() {
    let store = test_store();
    let first = spawn_feed_server("Feed One");
    let second = spawn_feed_server("Feed Two");
    let tool = RssManageSubscriptionsTool::new(store.clone(), test_fetcher(), 500, 50);

    let output = tool
        .execute(
            test_context(),
            json!({
                "operation": "add",
                "feeds": null,
                "targets": null,
                "raw_text": format!("1. Release notes\n{first}\n2. Recent Commits\n{second}")
            }),
        )
        .await
        .unwrap();

    assert_eq!(output.value["ok"], true);
    assert_eq!(output.value["created"].as_array().unwrap().len(), 2);
    let subscriptions = store.list_by_scope("private:u1").unwrap();
    assert_eq!(subscriptions.len(), 2);
    assert!(
        subscriptions
            .iter()
            .any(|subscription| subscription.title == "Release notes")
    );
    assert!(
        subscriptions
            .iter()
            .any(|subscription| subscription.title == "Recent Commits")
    );
}

#[tokio::test]
async fn rss_manage_tool_rejects_too_long_raw_text_url() {
    let tool = RssManageSubscriptionsTool::new(test_store(), test_fetcher(), 500, 50);
    let url = format!(
        "https://example.test/{}",
        "a".repeat(RSS_TOOL_URL_MAX_CHARS)
    );

    let err = tool
        .execute(
            test_context(),
            json!({
                "operation": "add",
                "feeds": null,
                "targets": null,
                "raw_text": url
            }),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "bad_tool_arguments");
}

#[tokio::test]
async fn rss_manage_tool_rejects_disallowed_contexts_for_add_and_delete() {
    let tool = RssManageSubscriptionsTool::new(test_store(), test_fetcher(), 500, 50);
    let mut group_member = test_context();
    group_member.conversation.kind = ConversationKind::Group;
    group_member.conversation.target_id = Some("g1".to_owned());
    group_member.actor.group_member_role = Some("member".to_owned());
    let mut group_without_role = group_member.clone();
    group_without_role.actor.group_member_role = None;

    for context in [
        {
            let mut context = test_context();
            context.conversation.kind = ConversationKind::Channel;
            context
        },
        {
            let mut context = test_context();
            context.conversation.kind = ConversationKind::Unknown;
            context
        },
        group_member,
        group_without_role,
    ] {
        for operation in ["add", "delete"] {
            let output = tool
                .execute(context.clone(), manage_arguments(operation))
                .await
                .unwrap();
            assert_eq!(output.value["ok"], false);
            assert_eq!(output.value["error"]["code"], "permission_denied");
        }
    }
}

#[tokio::test]
async fn rss_manage_tool_allows_group_admin_to_add_and_delete() {
    let store = test_store();
    let tool = RssManageSubscriptionsTool::new(store.clone(), test_fetcher(), 500, 50);
    let mut context = test_context();
    context.conversation.kind = ConversationKind::Group;
    context.conversation.target_id = Some("g1".to_owned());
    context.conversation.scope_id = "opaque-group-scope".to_owned();
    context.actor.group_member_role = Some("admin".to_owned());
    let url = spawn_feed_server("Admin Feed");

    let added = tool
        .execute(
            context.clone(),
            json!({
                "operation": "add",
                "feeds": [{"url": url, "title": null}],
                "targets": null,
                "raw_text": null
            }),
        )
        .await
        .unwrap();
    assert_eq!(added.value["ok"], true);
    assert_eq!(store.list_by_scope("opaque-group-scope").unwrap().len(), 1);

    let deleted = tool
        .execute(context, manage_arguments("delete"))
        .await
        .unwrap();
    assert_eq!(deleted.value["ok"], true);
    assert!(
        store
            .list_by_scope("opaque-group-scope")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn rss_target_requires_authoritative_group_id_without_parsing_scope() {
    let mut context = test_context();
    context.conversation.kind = ConversationKind::Group;
    context.conversation.target_id = Some("  ".to_owned());
    context.conversation.scope_id = "group:must-not-be-parsed".to_owned();
    context.actor.user_id = Some("must-not-be-used".to_owned());

    let err = target_from_context(&context).unwrap_err();

    assert_eq!(err.code, "missing_conversation_target");
}

#[test]
fn rss_target_preserves_private_and_service_account_behavior() {
    for kind in [ConversationKind::Private, ConversationKind::ServiceAccount] {
        let mut context = test_context();
        context.conversation.kind = kind;
        context.conversation.target_id = None;
        context.actor.user_id = Some("actor-1".to_owned());
        context.conversation.scope_id = "opaque-private-scope".to_owned();

        let target = target_from_context(&context).unwrap();

        assert_eq!(target.target_type, RssTargetType::Private);
        assert_eq!(target.target_id, "actor-1");
        assert_eq!(target.scope_key, "opaque-private-scope");
    }
}

#[test]
fn rss_manage_compact_output_stays_under_default_tool_limit_for_full_batch() {
    let long_title = "很长的订阅标题".repeat(20);
    let long_url = format!("https://example.test/{}", "a".repeat(470));
    let long_error = "解析失败：返回内容不是 RSS 或 Atom 文档。".repeat(20);
    let now = now_iso_cn();
    let created = (0..RSS_TOOL_MAX_BATCH_ITEMS)
        .map(|index| {
            let subscription = RssSubscription {
                id: format!("00000000-0000-0000-0000-{index:012}"),
                target_type: RssTargetType::Private,
                target_id: "u1".to_owned(),
                scope_key: "private:u1".to_owned(),
                url: long_url.clone(),
                title: long_title.clone(),
                enabled: true,
                created_at: now.clone(),
                last_checked_at: None,
                last_success_at: None,
                last_error: None,
                consecutive_failures: 0,
                initialized: true,
            };
            let mut details_truncated = false;
            compact_manage_subscription_json(&subscription, Some(1), &mut details_truncated)
        })
        .collect::<Vec<_>>();
    let mut details_truncated = false;
    let failed = (0..RSS_TOOL_MAX_BATCH_ITEMS)
        .map(|_| compact_manage_failure_json(&long_url, &long_error, &mut details_truncated))
        .collect::<Vec<_>>();
    let outputs = [
        json!({
            "ok": true,
            "operation": "add",
            "scope_id": "private:u1",
            "created": created,
            "failed": [],
            "details_truncated": true,
            "message": format_manage_message("add", RSS_TOOL_MAX_BATCH_ITEMS, 0),
        }),
        json!({
            "ok": false,
            "operation": "add",
            "scope_id": "private:u1",
            "created": [],
            "failed": failed,
            "details_truncated": details_truncated,
            "message": format_manage_message("add", 0, RSS_TOOL_MAX_BATCH_ITEMS),
        }),
    ];

    for output in outputs {
        let serialized = serde_json::to_string(&output).unwrap();
        assert!(
            serialized.chars().count() <= qq_maid_llm::tool::DEFAULT_TOOL_OUTPUT_MAX_CHARS,
            "RSS 管理输出不应触发通用 Tool 截断，实际 {} 字符",
            serialized.chars().count()
        );
        assert_eq!(output["details_truncated"], true);
        assert_eq!(output["operation"], "add");
        assert!(output.get("ok").and_then(Value::as_bool).is_some());
    }
}

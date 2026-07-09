use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use super::{super::RespondRequest, support::*};
use crate::runtime::rss::{RssFeedItem, RssTarget, RssTargetType};

const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0">
  <channel>
    <title>Fixture Feed</title>
    <item>
      <title>Existing Item</title>
      <link>https://example.test/existing</link>
      <guid>existing-guid</guid>
      <description><![CDATA[<p>Existing <b>summary</b></p>]]></description>
    </item>
  </channel>
</rss>"#;

fn spawn_feed_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
    });
    format!("http://{addr}/feed.xml")
}

fn private_message(text: &str, user_id: &str) -> RespondRequest {
    RespondRequest {
        content: text.to_owned(),
        scope_key: format!("private:{user_id}"),
        user_id: Some(user_id.to_owned()),
        group_id: None,
        platform: "qq_official".to_owned(),
        event_type: "FakeEvent".to_owned(),
        ..RespondRequest::default()
    }
}

fn rss_item(key: &str, title: &str, published_at: &str) -> RssFeedItem {
    RssFeedItem {
        item_key: key.to_owned(),
        revision_hash: format!("rev:{key}"),
        title: title.to_owned(),
        link: Some(format!("https://example.test/{key}")),
        published_at: Some(published_at.to_owned()),
        updated_at: None,
        summary: Some(format!("{title} 摘要")),
        source_order: 0,
    }
}

fn group_member_message(text: &str, role: Option<&str>) -> RespondRequest {
    let mut req = message(text);
    req.user_id = Some("u2".to_owned());
    req.group_member_role = role.map(str::to_owned);
    req
}

#[tokio::test]
async fn rss_add_records_baseline_without_pending_push() {
    let (service, _) = test_service_with_base();
    let url = spawn_feed_server(FEED);

    let response = service
        .respond(message(&format!("/rss add {url} 自定义订阅")))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("rss_add"));
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();
    assert!(!text.starts_with("null"));
    assert!(!text.contains("null已"));
    assert!(text.contains("已添加 RSS 订阅"));
    assert!(text.contains("不会推送历史文章"));
    assert!(markdown.contains("地址："));
    let subscriptions = service.rss_store.list_by_scope("group:g1").unwrap();
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].title, "自定义订阅");
    assert!(
        service
            .rss_store
            .pending_items(&subscriptions[0].id, 10, 3)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn group_rss_management_requires_owner_or_admin() {
    let (service, _) = test_service_with_base();

    let denied_add = service
        .respond(group_member_message(
            "/rss add http://127.0.0.1:9/feed.xml 普通成员订阅",
            Some("member"),
        ))
        .await
        .unwrap();
    assert_eq!(denied_add.command.as_deref(), Some("group_admin_required"));
    assert!(denied_add.text.unwrap().contains("群主或管理员"));

    let url = spawn_feed_server(FEED);
    service
        .respond(message(&format!("/rss add {url} 群订阅")))
        .await
        .unwrap();
    let denied_delete = service
        .respond(group_member_message("/rss delete 1", Some("member")))
        .await
        .unwrap();
    assert_eq!(
        denied_delete.command.as_deref(),
        Some("group_admin_required")
    );
    assert_eq!(
        service.rss_store.list_by_scope("group:g1").unwrap().len(),
        1
    );

    let list = service
        .respond(group_member_message("/rss", Some("member")))
        .await
        .unwrap();
    assert!(list.text.unwrap().contains("群订阅"));
}

#[tokio::test]
async fn rss_list_and_delete_use_current_scope_only() {
    let (service, _) = test_service_with_base();
    let group_url = spawn_feed_server(FEED);
    service
        .respond(message(&format!("/rss add {group_url} 群订阅")))
        .await
        .unwrap();

    let private_url = spawn_feed_server(FEED);
    service
        .respond(private_message(
            &format!("/rss add {private_url} 私聊订阅"),
            "u2",
        ))
        .await
        .unwrap();

    let group_list = service.respond(message("/rss")).await.unwrap();
    assert!(group_list.text.as_deref().unwrap().contains("群订阅"));
    assert!(
        group_list
            .markdown
            .as_deref()
            .unwrap()
            .contains("1. 群订阅")
    );
    let private_list = service
        .respond(private_message("/订阅", "u2"))
        .await
        .unwrap();
    assert!(private_list.text.as_deref().unwrap().contains("私聊订阅"));
    assert!(
        private_list
            .markdown
            .as_deref()
            .unwrap()
            .contains("1. 私聊订阅")
    );

    let deleted = service.respond(message("/rss delete 1")).await.unwrap();
    assert_eq!(deleted.command.as_deref(), Some("rss_delete"));
    let delete_text = deleted.text.unwrap();
    assert!(!delete_text.starts_with("null"));
    assert!(!delete_text.contains("null已"));
    assert!(delete_text.contains("已删除 RSS 订阅：群订阅"));
    assert!(
        service
            .rss_store
            .list_by_scope("group:g1")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        service.rss_store.list_by_scope("private:u2").unwrap().len(),
        1
    );
}

#[tokio::test]
async fn rss_recent_returns_items_instead_of_subscription_list() {
    let (service, _) = test_service_with_base();
    let target = RssTarget {
        target_type: RssTargetType::Group,
        target_id: "g1".to_owned(),
        scope_key: "group:g1".to_owned(),
    };
    let sub = service
        .rss_store
        .create_subscription(
            &target,
            "https://example.test/feed.xml",
            "Recent Commits",
            &[],
            50,
        )
        .unwrap();
    service
        .rss_store
        .enqueue_items(
            &sub.id,
            &[rss_item(
                "commit-1",
                "修复 RSS recent",
                "2026-07-08T05:00:00+00:00",
            )],
            50,
        )
        .unwrap();

    let response = service.respond(message("/rss recent")).await.unwrap();
    let text = response.text.unwrap();

    assert_eq!(response.command.as_deref(), Some("rss_recent"));
    assert!(text.contains("最近 RSS 更新"));
    assert!(text.contains("[Recent Commits] 修复 RSS recent"));
    assert!(text.contains("https://example.test/commit-1"));
    assert!(text.contains("发布时间：2026-07-08"));
    assert!(!text.contains("RSS 订阅："));
}

#[tokio::test]
async fn rss_recent_sanitizes_markdown_link_title() {
    let (service, _) = test_service_with_base();
    let target = RssTarget {
        target_type: RssTargetType::Group,
        target_id: "g1".to_owned(),
        scope_key: "group:g1".to_owned(),
    };
    let sub = service
        .rss_store
        .create_subscription(
            &target,
            "https://example.test/feed.xml",
            "Release notes",
            &[],
            50,
        )
        .unwrap();
    service
        .rss_store
        .enqueue_items(
            &sub.id,
            &[RssFeedItem {
                item_key: "release-1".to_owned(),
                revision_hash: "rev:release-1".to_owned(),
                title: "v0.14.2\n[cpa_final_answer](x)".to_owned(),
                link: Some(
                    "https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2".to_owned(),
                ),
                published_at: Some("2026-07-08T05:00:00+00:00".to_owned()),
                updated_at: None,
                summary: Some("What's Changed\ncpa_final_answer 只作为正文".to_owned()),
                source_order: 0,
            }],
            50,
        )
        .unwrap();

    let response = service.respond(message("/rss recent")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.contains("[Release notes] v0.14.2 [cpa_final_answer](x)"));
    assert!(!text.contains("v0.14.2\n[cpa_final_answer](x)"));
    assert!(markdown.contains(
        r"[v0.14.2 \[cpa\_final\_answer\]\(x\)](<https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2>)"
    ));
    assert!(!markdown.contains("[v0.14.2\n"));
}

#[tokio::test]
async fn rss_recent_release_regression_keeps_title_isolated_from_protocol_like_summary() {
    let (service, _) = test_service_with_base();
    let target = RssTarget {
        target_type: RssTargetType::Group,
        target_id: "g1".to_owned(),
        scope_key: "group:g1".to_owned(),
    };
    let sub = service
        .rss_store
        .create_subscription(
            &target,
            "https://example.test/releases.xml",
            "Release notes from qq-maid-bot",
            &[],
            50,
        )
        .unwrap();
    service
        .rss_store
        .enqueue_items(
            &sub.id,
            &[RssFeedItem {
                item_key: "release-v0.14.2".to_owned(),
                revision_hash: "rev:release-v0.14.2".to_owned(),
                title: "v0.14.2".to_owned(),
                link: Some(
                    "https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2"
                        .to_owned(),
                ),
                published_at: Some("2026-07-08T05:00:00+00:00".to_owned()),
                updated_at: None,
                summary: Some(
                    "What's Changed\n\ncpa_final_answer\ntool_call\nCPA final answer\n最终回答要求\n如果正确的下一步输出是普通的助手文本最终回答".to_owned(),
                ),
                source_order: 0,
            }],
            50,
        )
        .unwrap();

    let response = service.respond(message("/rss recent")).await.unwrap();
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();

    assert!(text.contains("[Release notes from qq-maid-bot] v0.14.2"));
    assert!(!text.contains("[Release notes from qq-maid-bot] cpa_final_answer"));
    assert!(!text.contains("[Release notes from qq-maid-bot] tool_call"));
    assert!(
        markdown
            .contains("[v0.14.2](<https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2>)")
    );
    assert!(!markdown.contains(
        "[cpa\\_final\\_answer](<https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2>)"
    ));
    assert!(!markdown.contains(
        "[tool\\_call](<https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2>)"
    ));
    assert!(!markdown.contains(
        "[最终回答要求](<https://github.com/kuliantnt/qq-maid-bot/releases/tag/v0.14.2>)"
    ));
}

#[tokio::test]
async fn rss_recent_chinese_alias_and_scope_are_isolated() {
    let (service, _) = test_service_with_base();
    let group_sub = service
        .rss_store
        .create_subscription(
            &RssTarget {
                target_type: RssTargetType::Group,
                target_id: "g1".to_owned(),
                scope_key: "group:g1".to_owned(),
            },
            "https://example.test/group.xml",
            "群订阅",
            &[],
            50,
        )
        .unwrap();
    let other_sub = service
        .rss_store
        .create_subscription(
            &RssTarget {
                target_type: RssTargetType::Group,
                target_id: "g2".to_owned(),
                scope_key: "group:g2".to_owned(),
            },
            "https://example.test/other.xml",
            "其他群订阅",
            &[],
            50,
        )
        .unwrap();
    service
        .rss_store
        .enqueue_items(
            &group_sub.id,
            &[rss_item("group", "当前群更新", "2026-07-08T05:00:00+00:00")],
            50,
        )
        .unwrap();
    service
        .rss_store
        .enqueue_items(
            &other_sub.id,
            &[rss_item("other", "其他群更新", "2026-07-08T06:00:00+00:00")],
            50,
        )
        .unwrap();

    let response = service.respond(message("/rss 最近 10")).await.unwrap();
    let text = response.text.unwrap();

    assert_eq!(response.command.as_deref(), Some("rss_recent"));
    assert!(text.contains("当前群更新"));
    assert!(!text.contains("其他群更新"));
}

#[tokio::test]
async fn rss_recent_distinguishes_no_subscription_and_no_items() {
    let (service, _) = test_service_with_base();

    let empty = service.respond(message("/rss recent")).await.unwrap();
    assert!(
        empty
            .text
            .as_deref()
            .unwrap()
            .contains("当前会话还没有 RSS 订阅")
    );

    service
        .rss_store
        .create_subscription(
            &RssTarget {
                target_type: RssTargetType::Group,
                target_id: "g1".to_owned(),
                scope_key: "group:g1".to_owned(),
            },
            "https://example.test/feed.xml",
            "空订阅",
            &[],
            50,
        )
        .unwrap();
    let no_items = service.respond(message("/rss recent")).await.unwrap();
    assert!(
        no_items
            .text
            .as_deref()
            .unwrap()
            .contains("已有 RSS 订阅，但还没有抓到更新")
    );
}

#[tokio::test]
async fn rss_recent_mentions_failed_feeds_without_blocking_items() {
    let (service, _) = test_service_with_base();
    let target = RssTarget {
        target_type: RssTargetType::Group,
        target_id: "g1".to_owned(),
        scope_key: "group:g1".to_owned(),
    };
    let ok_sub = service
        .rss_store
        .create_subscription(&target, "https://example.test/ok.xml", "正常订阅", &[], 50)
        .unwrap();
    let failed_sub = service
        .rss_store
        .create_subscription(
            &target,
            "https://example.test/timeout.xml",
            "失败订阅",
            &[],
            50,
        )
        .unwrap();
    service
        .rss_store
        .record_check_failure(&failed_sub.id, "timeout")
        .unwrap();
    service
        .rss_store
        .enqueue_items(
            &ok_sub.id,
            &[rss_item(
                "ok",
                "仍然展示的更新",
                "2026-07-08T05:00:00+00:00",
            )],
            50,
        )
        .unwrap();

    let response = service.respond(message("/rss recent")).await.unwrap();
    let text = response.text.unwrap();

    assert!(text.contains("仍然展示的更新"));
    assert!(text.contains("1 个订阅源最近检查失败"));
    assert!(!text.contains("失败订阅 [启用]"));
}

#[tokio::test]
async fn rss_add_ignores_placeholder_null_custom_name() {
    let (service, _) = test_service_with_base();
    let url = spawn_feed_server(FEED);

    let response = service
        .respond(message(&format!("/rss add {url} null")))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("rss_add"));
    let text = response.text.unwrap();
    let markdown = response.markdown.unwrap();
    assert!(!text.starts_with("null"));
    assert!(!text.contains("null已"));
    assert!(text.contains("已添加 RSS 订阅：Fixture Feed"));
    assert!(markdown.contains("已添加 RSS 订阅：Fixture Feed"));
    let subscriptions = service.rss_store.list_by_scope("group:g1").unwrap();
    assert_eq!(subscriptions[0].title, "Fixture Feed");
}

#[tokio::test]
async fn rss_add_accepts_numbered_multiline_title_url_pairs() {
    let (service, _) = test_service_with_base();
    let releases_url = spawn_feed_server(FEED);
    let commits_url = spawn_feed_server(FEED);

    let response = service
        .respond(message(&format!(
            "/RSS add\n1. Release notes from qq-maid-bot\n{releases_url}\n2. Recent Commits to qq-maid-bot:master\n{commits_url}"
        )))
        .await
        .unwrap();

    assert_eq!(response.command.as_deref(), Some("rss_add"));
    let text = response.text.unwrap();
    assert!(text.contains("RSS 批量添加结果"));
    assert!(text.contains("Release notes from qq-maid-bot"));
    assert!(text.contains("Recent Commits to qq-maid-bot:master"));
    let subscriptions = service.rss_store.list_by_scope("group:g1").unwrap();
    assert_eq!(subscriptions.len(), 2);
    assert!(
        subscriptions
            .iter()
            .any(|subscription| subscription.title == "Release notes from qq-maid-bot")
    );
    assert!(
        subscriptions
            .iter()
            .any(|subscription| subscription.title == "Recent Commits to qq-maid-bot:master")
    );
}

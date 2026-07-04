use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use super::{super::RespondRequest, support::*};

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

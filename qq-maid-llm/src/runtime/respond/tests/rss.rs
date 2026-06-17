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
    assert!(text.contains("已添加 RSS 订阅"));
    assert!(text.contains("不会推送历史文章"));
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
    assert!(group_list.text.unwrap().contains("群订阅"));
    let private_list = service
        .respond(private_message("/订阅", "u2"))
        .await
        .unwrap();
    assert!(private_list.text.unwrap().contains("私聊订阅"));

    let deleted = service.respond(message("/rss delete 1")).await.unwrap();
    assert_eq!(deleted.command.as_deref(), Some("rss_delete"));
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

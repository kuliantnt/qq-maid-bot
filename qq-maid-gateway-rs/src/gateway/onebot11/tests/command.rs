use super::*;

#[tokio::test]
async fn ping_uses_onebot_reply_chain_and_bypasses_core_for_private_and_group() {
    let core = Arc::new(RecordingCore::new("普通 Core 回复"));
    let (handle, _runtime, shutdown) = spawn_server_with_core(test_config(), core.clone()).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();

    send_text_event(&mut client, private_message_event("ping-private", "/ping")).await;
    let private_reply = next_action_request(&mut client).await;
    assert_eq!(private_reply.action, "send_private_msg");
    assert!(private_reply.params.to_string().contains("核心链路"));
    assert!(!private_reply.params.to_string().contains(TOKEN));
    complete_action(&mut client, &private_reply, "ping-private-reply").await;
    assert!(core.requests().is_empty());

    send_text_event(
        &mut client,
        group_message_event(
            "ping-group",
            json!([{"type": "text", "data": {"text": "/ping all"}}]),
        ),
    )
    .await;
    let group_reply = next_action_request(&mut client).await;
    assert_eq!(group_reply.action, "send_group_msg");
    let group_body = group_reply.params.to_string();
    assert!(group_body.contains("核心链路"));
    assert!(!group_body.contains("### 配置"));
    assert!(!group_body.contains(TOKEN));
    complete_action(&mut client, &group_reply, "ping-group-reply").await;
    assert!(core.requests().is_empty());

    send_text_event(
        &mut client,
        private_message_event("similar-command", "/pingxxx"),
    )
    .await;
    let normal_reply = next_action_request(&mut client).await;
    assert!(normal_reply.params.to_string().contains("普通 Core 回复"));
    complete_action(&mut client, &normal_reply, "normal-reply").await;
    wait_until(|| core.requests().len() == 1).await;
    assert_eq!(core.requests()[0].text, "/pingxxx");

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

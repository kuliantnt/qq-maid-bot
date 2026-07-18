use super::*;
#[tokio::test]
async fn concurrent_text_sends_use_unique_echo_and_complete_out_of_order() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());
    let private_sender = sender.clone();
    let private = tokio::spawn(async move {
        private_sender
            .send_private_text("20002", "private body")
            .await
    });
    let group = tokio::spawn(async move { sender.send_group_text("30003", "group body").await });

    let first = next_action_request(&mut client).await;
    let second = next_action_request(&mut client).await;
    assert_ne!(first.echo, second.echo);
    for request in [&first, &second] {
        let (target_key, target_id, body) = match request.action.as_str() {
            "send_private_msg" => ("user_id", 20002_u64, "private body"),
            "send_group_msg" => ("group_id", 30003_u64, "group body"),
            action => panic!("unexpected action {action}"),
        };
        assert_eq!(request.params[target_key], json!(target_id));
        assert!(request.params[target_key].is_u64());
        assert_eq!(
            request.params["message"],
            json!([{"type": "text", "data": {"text": body}}])
        );
    }

    for request in [&second, &first] {
        let message_id = if request.action == "send_private_msg" {
            json!(90001)
        } else {
            json!("90002")
        };
        client
            .send(Message::Text(
                serde_json::to_string(&action_response(request, json!({"message_id": message_id})))
                    .unwrap()
                    .into(),
            ))
            .await
            .unwrap();
    }

    assert_eq!(private.await.unwrap().unwrap().message_id, "90001");
    assert_eq!(group.await.unwrap().unwrap().message_id, "90002");

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn large_target_id_is_sent_as_exact_json_number() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());
    let send = tokio::spawn(async move {
        sender
            .send_private_text("18446744073709551615", "large target")
            .await
    });

    let request = next_action_request(&mut client).await;
    assert_eq!(request.params["user_id"], json!(u64::MAX));
    assert_eq!(request.params["user_id"].as_u64(), Some(u64::MAX));
    client
        .send(Message::Text(
            serde_json::to_string(&action_response(&request, json!({"message_id": "90003"})))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(send.await.unwrap().unwrap().message_id, "90003");

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn invalid_target_id_returns_error_without_writing_action() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());

    assert!(matches!(
        sender.send_group_text("not-a-number", "invalid").await,
        Err(OneBotSendError::InvalidTargetId)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), client.next())
            .await
            .is_err(),
        "无效目标 ID 不应向 WebSocket 写入 action"
    );

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn unknown_echo_does_not_complete_pending_send() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());
    let mut send = tokio::spawn(async move { sender.send_private_text("20002", "hello").await });
    let request = next_action_request(&mut client).await;
    let mut unknown = action_response(&request, json!({"message_id": 1}));
    unknown.echo = Some(crate::gateway::onebot11::protocol::Echo(json!("unknown")));
    client
        .send(Message::Text(
            serde_json::to_string(&unknown).unwrap().into(),
        ))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut send)
            .await
            .is_err()
    );

    client
        .send(Message::Text(
            serde_json::to_string(&action_response(&request, json!({"message_id": "real-id"})))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    assert_eq!(send.await.unwrap().unwrap().message_id, "real-id");

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn sender_propagates_retcode_failure_without_forging_success() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());
    let send = tokio::spawn(async move { sender.send_group_text("30003", "hello").await });
    let request = next_action_request(&mut client).await;
    client
        .send(Message::Text(
            json!({
                "status": "failed",
                "retcode": 1404,
                "data": null,
                "message": "failed remotely",
                "wording": "target unavailable",
                "echo": request.echo
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();

    assert!(matches!(
        send.await.unwrap(),
        Err(OneBotSendError::Rejected {
            retcode: 1404,
            remote_message_present: true,
            ..
        })
    ));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn sender_returns_explicit_offline_and_disconnect_errors() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let sender = OneBotSender::new(handle.connection.clone());
    assert!(matches!(
        sender.send_private_text("20002", "offline").await,
        Err(OneBotSendError::Transport(OneBotCallError::NotConnected))
    ));

    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let pending_sender = sender.clone();
    let pending = tokio::spawn(async move {
        pending_sender
            .send_private_text("20002", "disconnect")
            .await
    });
    let _request = next_action_request(&mut client).await;
    client.close(None).await.unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(OneBotSendError::Transport(
            OneBotCallError::ConnectionClosed
        ))
    ));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn sender_propagates_action_timeout() {
    let mut config = test_config();
    config.onebot11.request_timeout = Duration::from_millis(50);
    let (handle, _runtime, shutdown) = spawn_server_with_config(config).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let sender = OneBotSender::new(handle.connection.clone());
    let send = tokio::spawn(async move { sender.send_private_text("20002", "timeout").await });
    let _request = next_action_request(&mut client).await;

    assert!(matches!(
        send.await.unwrap(),
        Err(OneBotSendError::Transport(OneBotCallError::Timeout))
    ));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn unregistered_connection_cannot_complete_active_request_with_forged_echo() {
    let (handle, _runtime, shutdown) = spawn_server().await;
    let mut active = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    let mut unregistered = connect(handle.local_addr, PATH, TOKEN, None).await.unwrap();

    let connection = handle.connection.clone();
    let mut call = tokio::spawn(async move { connection.call("get_status", json!({})).await });
    let request = next_action_request(&mut active).await;
    unregistered
        .send(Message::Text(
            serde_json::to_string(&action_response(&request, json!({"forged": true})))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();

    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut call)
            .await
            .is_err(),
        "未注册连接伪造的 echo 不应完成活动请求"
    );
    active
        .send(Message::Text(
            serde_json::to_string(&action_response(&request, json!({"forged": false})))
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    let response = call.await.unwrap().unwrap();
    assert_eq!(response.data, json!({"forged": false}));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn stale_generation_response_cannot_complete_current_request() {
    let context = OneBotConnectionContext::new(Duration::from_millis(500));
    let self_id = OneBotId::new("10001").unwrap();
    let (old_outbound, mut old_requests) = mpsc::channel(1);
    let old_registration = context
        .register(self_id.clone(), old_outbound, CancellationToken::new())
        .unwrap();
    let old_context = context.clone();
    let old_call = tokio::spawn(async move { old_context.call("get_status", json!({})).await });
    let _old_request: ActionRequest =
        serde_json::from_str(&old_requests.recv().await.unwrap()).unwrap();

    let (current_outbound, mut current_requests) = mpsc::channel(1);
    let current_registration = context
        .register(self_id, current_outbound, CancellationToken::new())
        .unwrap();
    assert!(matches!(
        old_call.await.unwrap(),
        Err(OneBotCallError::ConnectionClosed)
    ));

    let current_context = context.clone();
    let mut current_call =
        tokio::spawn(async move { current_context.call("get_status", json!({})).await });
    let current_request: ActionRequest =
        serde_json::from_str(&current_requests.recv().await.unwrap()).unwrap();
    context.dispatch_response(
        old_registration.generation,
        action_response(&current_request, json!({"generation": "old"})),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut current_call)
            .await
            .is_err(),
        "旧 generation 的 response 不应完成当前请求"
    );

    context.dispatch_response(
        current_registration.generation,
        action_response(&current_request, json!({"generation": "current"})),
    );
    let response = current_call.await.unwrap().unwrap();
    assert_eq!(response.data, json!({"generation": "current"}));
}

#[tokio::test]
async fn outbound_queue_wait_is_included_in_request_timeout() {
    let request_timeout = Duration::from_millis(50);
    let context = OneBotConnectionContext::new(request_timeout);
    let (outbound, _requests) = mpsc::channel(1);
    outbound.send("occupied".to_owned()).await.unwrap();
    context
        .register(
            OneBotId::new("10001").unwrap(),
            outbound,
            CancellationToken::new(),
        )
        .unwrap();

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        context.call("get_status", json!({})),
    )
    .await
    .expect("call 应由 request_timeout 自行结束，而不是持续阻塞在 outbound 队列");

    assert!(matches!(result, Err(OneBotCallError::Timeout)));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test]
async fn heartbeat_is_optional_until_client_sends_first_heartbeat() {
    let mut config = test_config();
    config.onebot11.request_timeout = Duration::from_millis(50);
    let (handle, runtime, shutdown) = spawn_server_with_config(config).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(runtime.snapshot().onebot_connected);
    let connection = handle.connection.clone();
    let call = tokio::spawn(async move { connection.call("get_status", json!({})).await });
    assert_eq!(next_action_request(&mut client).await.action, "get_status");
    assert!(matches!(call.await.unwrap(), Err(OneBotCallError::Timeout)));

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn same_account_replaces_old_connection_and_different_account_is_rejected() {
    let (handle, runtime, shutdown) = spawn_server().await;
    let mut first = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    wait_until(|| runtime.snapshot().onebot_connected).await;

    let connection = handle.connection.clone();
    let pending_call = tokio::spawn(async move { connection.call("get_status", json!({})).await });
    assert!(matches!(
        first.next().await.unwrap().unwrap(),
        Message::Text(_)
    ));
    let _second = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    assert!(matches!(
        pending_call.await.unwrap(),
        Err(OneBotCallError::ConnectionClosed)
    ));
    assert_eq!(
        next_close(&mut first).await.as_deref(),
        Some("replaced by newer connection")
    );
    wait_until(|| runtime.snapshot().last_onebot_replaced_at.is_some()).await;
    assert!(runtime.snapshot().onebot_connected);

    let mut mismatch = connect(handle.local_addr, PATH, TOKEN, Some("20002"))
        .await
        .unwrap();
    assert_eq!(
        next_close(&mut mismatch).await.as_deref(),
        Some("different self_id is not allowed")
    );
    assert!(runtime.snapshot().onebot_connected);
    assert_eq!(
        handle.connection.connected_self_id().unwrap().as_str(),
        "10001"
    );

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn invalid_json_is_isolated_and_client_can_reconnect() {
    let (handle, runtime, shutdown) = spawn_server().await;
    let mut invalid = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    invalid
        .send(Message::Text("{not-json".into()))
        .await
        .unwrap();
    assert_eq!(
        next_close(&mut invalid).await.as_deref(),
        Some("invalid JSON")
    );
    wait_until(|| !runtime.snapshot().onebot_connected).await;
    assert_eq!(
        runtime.snapshot().last_onebot_disconnect_summary.as_deref(),
        Some("invalid JSON")
    );
    assert!(runtime.snapshot().onebot_listening);

    let _reconnected = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    wait_until(|| runtime.snapshot().onebot_connected).await;

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn heartbeat_timeout_closes_only_the_client() {
    let mut config = test_config();
    config.onebot11.request_timeout = Duration::from_millis(100);
    let (handle, runtime, shutdown) = spawn_server_with_config(config).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    client
        .send(Message::Text(
            json!({
                "self_id": "10001",
                "post_type": "meta_event",
                "meta_event_type": "heartbeat",
                "interval": 10,
                "status": {"online": true}
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    assert_eq!(
        next_close(&mut client).await.as_deref(),
        Some("heartbeat timed out")
    );
    assert!(runtime.snapshot().onebot_listening);

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn api_request_timeout_does_not_drop_healthy_connection() {
    let mut config = test_config();
    config.onebot11.request_timeout = Duration::from_millis(100);
    let (handle, runtime, shutdown) = spawn_server_with_config(config).await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    wait_until(|| runtime.snapshot().onebot_connected).await;

    let connection = handle.connection.clone();
    let call = tokio::spawn(async move { connection.call("get_status", json!({})).await });
    assert!(matches!(
        client.next().await.unwrap().unwrap(),
        Message::Text(_)
    ));
    assert!(matches!(call.await.unwrap(), Err(OneBotCallError::Timeout)));
    assert!(runtime.snapshot().onebot_connected);

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn oversized_message_is_rejected_without_stopping_listener() {
    let (handle, runtime, shutdown) = spawn_server().await;
    let mut client = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    client
        .send(Message::Text("x".repeat(2048).into()))
        .await
        .unwrap();
    let _ = next_close(&mut client).await;
    wait_until(|| !runtime.snapshot().onebot_connected).await;
    assert_eq!(
        runtime.snapshot().last_onebot_disconnect_summary.as_deref(),
        Some("message too large")
    );
    assert!(runtime.snapshot().onebot_listening);

    let _reconnected = connect(handle.local_addr, PATH, TOKEN, Some("10001"))
        .await
        .unwrap();
    wait_until(|| runtime.snapshot().onebot_connected).await;

    shutdown.cancel();
    handle.task.await.unwrap().unwrap();
}

#[test]
fn test_payload_helpers_do_not_require_float_id_round_trip() {
    let value: Value = serde_json::from_str("123456789012345678").unwrap();
    assert!(value.is_u64());
}

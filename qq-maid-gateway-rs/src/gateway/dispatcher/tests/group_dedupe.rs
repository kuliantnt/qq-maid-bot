use super::*;

fn test_group_ingress_with_dedupe(
    config: AppConfig,
    ref_index: SharedRefIndex,
    dedupe: Arc<MessageDedupe>,
) -> Arc<GroupIngressPreprocessor> {
    Arc::new(GroupIngressPreprocessor::new(
        config,
        test_respond_client().with_qq_official_account_id("appid"),
        dedupe,
        Arc::new(Mutex::new(BotOutboundCache::default())),
        Arc::new(BotIdentity::new("appid", &[])),
        ref_index,
    ))
}

#[tokio::test]
async fn closed_dispatcher_rolls_back_group_reservation_for_same_event_retry() {
    let mut config = test_config();
    config.group_message_mode = crate::config::GroupMessageMode::Active;
    let dedupe = Arc::new(MessageDedupe::new(Duration::from_secs(60)));
    let (command_tx, command_rx) = mpsc::channel(1);
    let (reject_tx, _reject_rx) = mpsc::channel(1);
    let handle = MessageDispatcherHandle {
        command_tx,
        reject_tx,
        respond: test_respond_client(),
        group_ingress: test_group_ingress_with_dedupe(
            config,
            crate::gateway::ref_index::ref_index(),
            dedupe,
        ),
    };
    drop(command_rx);

    for _ in 0..2 {
        let mut message = group("retry-closed", "group-a");
        message.current_msg_idx = Some("1".to_owned());
        let error = handle.enqueue_group(message).await.unwrap_err();
        assert!(matches!(
            error,
            DispatcherEnqueueError::Unavailable {
                reason: "dispatcher_closed"
            }
        ));
    }
}

#[tokio::test]
async fn successful_group_enqueue_commits_reservation_before_duplicate_retry() {
    let mut config = test_config();
    config.group_message_mode = crate::config::GroupMessageMode::Active;
    let dedupe = Arc::new(MessageDedupe::new(Duration::from_secs(60)));
    let (command_tx, mut command_rx) = mpsc::channel(2);
    let (reject_tx, _reject_rx) = mpsc::channel(1);
    let handle = MessageDispatcherHandle {
        command_tx,
        reject_tx,
        respond: test_respond_client(),
        group_ingress: test_group_ingress_with_dedupe(
            config,
            crate::gateway::ref_index::ref_index(),
            dedupe,
        ),
    };
    let mut message = group("accepted-once", "group-a");
    message.current_msg_idx = Some("7".to_owned());

    let first = tokio::spawn({
        let handle = handle.clone();
        let message = message.clone();
        async move { handle.enqueue_group(message).await }
    });
    let command = command_rx.recv().await.expect("first event should enqueue");
    let DispatcherCommand::Enqueue { ack, .. } = command else {
        panic!("expected enqueue command");
    };
    ack.send(Ok(())).unwrap();
    first.await.unwrap().unwrap();

    handle.enqueue_group(message).await.unwrap();
    assert!(command_rx.try_recv().is_err());
}

#[tokio::test]
async fn full_group_queue_rolls_back_reservation_and_accepts_same_event_after_recovery() {
    let mut config = test_config();
    config.group_message_mode = crate::config::GroupMessageMode::Active;
    config.conversation_queue_capacity = 1;
    config.max_active_conversation_workers = 1;
    let ref_index = crate::gateway::ref_index::ref_index();
    let dedupe = Arc::new(MessageDedupe::new(Duration::from_secs(60)));
    let handler = Arc::new(RecordingHandler {
        block: true,
        ..RecordingHandler::default()
    });
    let (command_tx, command_rx) = mpsc::channel(16);
    let (reject_tx, reject_rx) = mpsc::channel(16);
    let auth = AccessTokenManager::new(
        qq_maid_common::http_client::client(),
        config.app_id.clone().unwrap(),
        config.app_secret.clone().unwrap(),
        config.token_refresh_margin,
    );
    let api = QqApiClient::new(
        qq_maid_common::http_client::client(),
        config.api_base.clone(),
        auth,
    );
    let shutdown = CancellationToken::new();
    let actor = DispatcherActor::new(
        config.clone(),
        api,
        GatewayRuntimeStatus::new(),
        command_rx,
        command_tx.clone(),
        reject_tx.clone(),
        reject_rx,
        Arc::new(RejectMetrics::default()),
        handler.clone(),
        shutdown.clone(),
    );
    let handle = MessageDispatcherHandle {
        command_tx,
        reject_tx,
        respond: test_respond_client(),
        group_ingress: test_group_ingress_with_dedupe(config, ref_index, dedupe),
    };
    let actor_task = tokio::spawn(actor.run());

    handle
        .enqueue_group(group("blocking", "group-a"))
        .await
        .unwrap();
    wait_for_events(&handler, 1).await;
    handle
        .enqueue_group(group("queued", "group-a"))
        .await
        .unwrap();

    let mut retried = group("retry-after-full", "group-a");
    retried.current_msg_idx = Some("9".to_owned());
    let error = handle.enqueue_group(retried.clone()).await.unwrap_err();
    assert!(matches!(
        error,
        DispatcherEnqueueError::RejectedAndHandled {
            reason: "conversation_queue_full"
        }
    ));

    handler.release_all();
    wait_for_events(&handler, 4).await;
    handle.enqueue_group(retried).await.unwrap();
    wait_for_events(&handler, 6).await;
    assert_eq!(
        handler
            .events()
            .iter()
            .filter(|event| event.contains("retry-after-full"))
            .count(),
        2
    );

    shutdown.cancel();
    timeout(Duration::from_secs(2), actor_task)
        .await
        .expect("dispatcher actor should stop")
        .unwrap();
}

#[tokio::test]
async fn passive_group_duplicate_does_not_refresh_ref_index_observation() {
    let mut config = test_config();
    config.group_message_mode = crate::config::GroupMessageMode::Off;
    let ref_index = crate::gateway::ref_index::ref_index();
    let (command_tx, mut command_rx) = mpsc::channel(1);
    let (reject_tx, _reject_rx) = mpsc::channel(1);
    let respond = test_respond_client().with_qq_official_account_id("appid");
    let handle = MessageDispatcherHandle {
        command_tx,
        reject_tx,
        respond: respond.clone(),
        group_ingress: test_group_ingress_with(config, ref_index.clone()),
    };
    let mut first = group("passive-once", "group-a");
    first.event_type = crate::event::GroupEventType::GroupMessage;
    first.current_msg_idx = Some("REFIDX_passive_once".to_owned());
    first.content = "第一次被动观察".to_owned();
    first.input_parts = vec![qq_maid_common::input_part::MessageInputPart::text(
        first.content.clone(),
    )];
    let mut duplicate = first.clone();
    duplicate.content = "重复事件不应刷新".to_owned();
    duplicate.input_parts = vec![qq_maid_common::input_part::MessageInputPart::text(
        duplicate.content.clone(),
    )];

    handle.enqueue_group(first).await.unwrap();
    handle.enqueue_group(duplicate).await.unwrap();
    assert!(command_rx.try_recv().is_err());

    let mut quoted = group("quote-passive-once", "group-a");
    quoted.reply = Some(crate::event::MessageReply {
        message_id: "quoted-passive-once".to_owned(),
        ref_msg_idx: Some("REFIDX_passive_once".to_owned()),
        content: None,
        input_parts: Vec::new(),
        media_summaries: Vec::new(),
    });
    let mut inbound = respond.prepare_inbound(
        crate::gateway::platform::qq_official::inbound_from_group(&quoted),
    );
    ref_index.lock().unwrap().enrich_inbound(&mut inbound);
    assert_eq!(
        inbound.quoted.unwrap().text_summary.as_deref(),
        Some("第一次被动观察")
    );
}

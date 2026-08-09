use super::*;

#[tokio::test]
async fn old_batch_retry_does_not_drop_new_message() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "A")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 1).await;

    enqueue(&handle, c2c("2", "u1", "C")).await;
    enqueue(&handle, c2c("1", "u1", "A retry")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 2).await;

    let contents = h
        .dispatcher
        .messages()
        .into_iter()
        .map(|message| message.content)
        .collect::<Vec<_>>();
    assert_eq!(contents, vec!["A", "C"]);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn old_batch_retry_with_same_event_id_and_new_message_id_does_not_drop_new_message() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    let first = c2c("1", "u1", "A");
    let mut retry = c2c("3", "u1", "A retry");
    retry.event_id = first.event_id.clone();
    retry.source_event_ids = first.source_event_ids.clone();

    enqueue(&handle, first).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 1).await;
    enqueue(&handle, c2c("2", "u1", "C")).await;
    enqueue(&handle, retry).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 2).await;

    let contents = h
        .dispatcher
        .messages()
        .into_iter()
        .map(|message| message.content)
        .collect::<Vec<_>>();
    assert_eq!(contents, vec!["A", "C"]);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn duplicate_physical_message_does_not_poison_batch_with_new_message() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "A")).await;
    enqueue(&handle, c2c("1", "u1", "A retry")).await;
    enqueue(&handle, c2c("2", "u1", "C")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 1).await;

    assert_eq!(h.dispatcher.messages()[0].content, "A\nC");
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn same_content_with_different_ids_is_retained() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "same")).await;
    enqueue(&handle, c2c("2", "u1", "same")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 1).await;
    assert_eq!(h.dispatcher.messages()[0].content, "same\nsame");
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn timer_and_new_message_race_submits_once() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 1).await;
    enqueue(&handle, c2c("2", "u1", "b")).await;
    assert_eq!(h.dispatcher.messages().len(), 1);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn active_key_limit_degrades_without_loss() {
    pause();
    let mut config = test_config();
    config.message_aggregation.max_active_keys = 1;
    let h = harness_with_config(config);
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    enqueue(&handle, c2c("2", "u2", "b")).await;
    assert_eq!(h.dispatcher.messages()[0].content, "b");
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 2).await;
    assert_eq!(h.dispatcher.messages().len(), 2);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn barrier_state_is_cleaned_after_processing() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "/todo add 无时间买牛奶")).await;
    wait_for_barrier_state(&handle, 1, 1).await;
    h.dispatcher.process_next();
    wait_for_barrier_state(&handle, 0, 0).await;
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn closed_processed_ack_releases_barrier() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "/todo add 无时间买牛奶")).await;
    wait_for_barrier_state(&handle, 1, 1).await;
    h.dispatcher.close_next_ack();
    wait_for_barrier_state(&handle, 0, 0).await;
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn closed_barrier_allows_next_plain_message_to_aggregate() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "/todo add 无时间买牛奶")).await;
    h.dispatcher.close_next_ack();
    wait_for_barrier_state(&handle, 0, 0).await;

    enqueue(&handle, c2c("2", "u1", "普通聊天")).await;
    assert_eq!(h.dispatcher.messages().len(), 1);
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 2).await;
    assert_eq!(h.dispatcher.messages()[1].content, "普通聊天");
    assert_eq!(h.dispatcher.pending_barriers(), 0);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn consecutive_barriers_complete_out_of_order_without_removing_newer_barrier() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "/todo add 一")).await;
    enqueue(&handle, c2c("2", "u1", "/resume")).await;
    enqueue(&handle, c2c("3", "u1", "/memory 需要记住的事")).await;
    wait_for_barrier_state(&handle, 3, 3).await;

    h.dispatcher.process_by_message_id("2");
    wait_for_barrier_state(&handle, 3, 2).await;
    h.dispatcher.process_by_message_id("1");
    wait_for_barrier_state(&handle, 1, 1).await;
    h.dispatcher.process_by_message_id("3");
    wait_for_barrier_state(&handle, 0, 0).await;
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn many_scope_barriers_do_not_grow_after_processing() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    for index in 0..20 {
        enqueue(
            &handle,
            c2c(
                &format!("{}", index + 1),
                &format!("u{}", index + 1),
                "/todo add 无时间任务",
            ),
        )
        .await;
    }
    wait_for_barrier_state(&handle, 20, 20).await;
    h.dispatcher.process_all();
    wait_for_barrier_state(&handle, 0, 0).await;
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn shutdown_exits_pending_barrier_tasks() {
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "/todo add 无时间买牛奶")).await;
    wait_for_barrier_state(&handle, 1, 1).await;
    timeout(Duration::from_secs(1), h.aggregator.shutdown())
        .await
        .expect("aggregator shutdown should not wait forever for processed ack");
}

#[tokio::test]
async fn shutdown_flushes_and_actor_exits() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    h.aggregator.shutdown().await;
    assert_eq!(h.dispatcher.messages()[0].content, "a");
}

#[tokio::test]
async fn dispatcher_is_not_closed_before_aggregator_flush() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    h.aggregator.shutdown().await;
    h.dispatcher.closed.store(true, Ordering::Relaxed);
    assert_eq!(h.dispatcher.messages().len(), 1);
}

#[tokio::test]
async fn classification_failure_dispatches_immediately() {
    let h = harness();
    h.core.fail_classify.store(true, Ordering::Relaxed);
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "hello")).await;
    assert_eq!(h.dispatcher.messages()[0].content, "hello");
    assert_eq!(h.dispatcher.pending_barriers(), 1);
    h.aggregator.shutdown().await;
}

#[test]
fn request_scope_key_matches_private_message() {
    let request = CoreRequest {
        message_id: Some("aggregator-test-message".to_owned()),
        text: "hello".to_owned(),
        input_parts: Vec::new(),
        quoted: None,
        mentions: Vec::new(),
        visible_entity_snapshot: None,
        platform: Platform::QqOfficial,
        account_id: None,
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            union_id: None,
            display_name: None,
            group_member_role: None,
            is_bot: false,
            identity_source: IdentitySource::Event,
        },
        addressed_to_bot: false,
        conversation: CoreConversation::Private {
            peer_id: "u1".to_owned(),
        },
    };
    assert_eq!(
        request.scope_key(),
        "platform:qq_official:account:-:private:u1"
    );
}

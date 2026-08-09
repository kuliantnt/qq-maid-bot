use super::*;

#[tokio::test]
async fn character_truncation_continues_below_threshold_and_clears_state() {
    let user = "character-continuation";
    let (database, store, sessions) = test_stores_with_database();
    character_truncation_backlog(&sessions, user);
    let provider =
        MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY"), Ok("NO_REPLY")]);
    let observable = provider.clone();
    let mut config = production_dream_config();
    config.max_input_chars = 800;
    let worker = production_worker(&store, provider, config);

    let first = worker
        .run_once(private_context(user))
        .await
        .unwrap()
        .unwrap();
    assert!(first.truncated);
    assert_eq!(first.input_sessions, 1);
    assert_eq!(observable.requests().len(), 1, "单次调度只处理一个批次");
    let first_state = dream_state(&database, user).unwrap();
    assert!(first_state.2, "字符截断后应保存待续批状态");

    let second = worker
        .run_once(private_context(user))
        .await
        .unwrap()
        .unwrap();
    assert!(!second.truncated);
    assert_eq!(second.input_sessions, 5);
    assert_eq!(observable.requests().len(), 2);
    let second_state = dream_state(&database, user).unwrap();
    assert!(second_state.0 > first_state.0);
    assert!(!second_state.2, "最后一批成功后应清除待续批状态");

    add_private_session(&sessions, user, "续批完成后的少量新消息");
    assert!(
        worker
            .run_once(private_context(user))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(observable.requests().len(), 2);
}

#[tokio::test]
async fn session_limit_truncation_continues_below_threshold() {
    let user = "session-limit-continuation";
    let (database, store, sessions) = test_stores_with_database();
    add_private_messages_with_timestamps(
        &sessions,
        user,
        5,
        &repeated_timestamps(30, &["2026-07-20T10:00:00+08:00"]),
    );
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY")]);
    let observable = provider.clone();
    let mut config = production_dream_config();
    config.max_sessions = 4;
    let worker = production_worker(&store, provider, config);

    let first = worker
        .run_once(private_context(user))
        .await
        .unwrap()
        .unwrap();
    assert!(first.truncated);
    assert_eq!(first.input_sessions, 4);
    assert!(dream_state(&database, user).unwrap().2);
    assert_eq!(observable.requests().len(), 1);

    let second = worker
        .run_once(private_context(user))
        .await
        .unwrap()
        .unwrap();
    assert!(!second.truncated);
    assert_eq!(second.input_sessions, 1);
    assert!(!dream_state(&database, user).unwrap().2);
    assert_eq!(observable.requests().len(), 2);
}

#[tokio::test]
async fn continuation_model_failure_keeps_checkpoint_and_retries() {
    let user = "continuation-model-failure";
    let (database, store, sessions) = test_stores_with_database();
    character_truncation_backlog(&sessions, user);
    let provider = MockProvider::with_dream_replies(vec![
        Ok("NO_REPLY"),
        Err(LlmError::provider("dream unavailable", "test")),
        Ok("NO_REPLY"),
    ]);
    let observable = provider.clone();
    let mut config = production_dream_config();
    config.max_input_chars = 800;
    let worker = production_worker(&store, provider, config);

    assert!(
        worker
            .run_once(private_context(user))
            .await
            .unwrap()
            .unwrap()
            .truncated
    );
    let pending_state = dream_state(&database, user).unwrap();
    assert!(pending_state.2);

    assert_eq!(
        worker.run_once(private_context(user)).await.unwrap_err(),
        "dream_model_failed"
    );
    assert_eq!(dream_state(&database, user), Some(pending_state));

    let retry = worker
        .run_once(private_context(user))
        .await
        .unwrap()
        .unwrap();
    assert!(!retry.truncated);
    assert!(!dream_state(&database, user).unwrap().2);
    assert_eq!(observable.requests().len(), 3);
}

#[tokio::test]
async fn continuation_database_failure_keeps_checkpoint_and_retries() {
    let user = "continuation-database-failure";
    let (database, store, sessions) = test_stores_with_database();
    character_truncation_backlog(&sessions, user);
    let provider =
        MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY"), Ok("NO_REPLY")]);
    let mut config = production_dream_config();
    config.max_input_chars = 800;
    let worker = production_worker(&store, provider, config);

    assert!(
        worker
            .run_once(private_context(user))
            .await
            .unwrap()
            .unwrap()
            .truncated
    );
    let pending_state = dream_state(&database, user).unwrap();
    assert!(pending_state.2);
    database
        .connection()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER abort_dream_completion_for_test
             BEFORE UPDATE OF last_processed_message_id ON memory_dream_state
             WHEN NEW.last_status = 'success'
             BEGIN SELECT RAISE(ABORT, 'Dream completion aborted for test'); END;",
        )
        .unwrap();

    assert_eq!(
        worker.run_once(private_context(user)).await.unwrap_err(),
        "dream_commit_failed"
    );
    assert_eq!(dream_state(&database, user), Some(pending_state));

    database
        .connection()
        .unwrap()
        .execute("DROP TRIGGER abort_dream_completion_for_test", [])
        .unwrap();
    let retry = worker
        .run_once(private_context(user))
        .await
        .unwrap()
        .unwrap();
    assert!(!retry.truncated);
    assert!(!dream_state(&database, user).unwrap().2);
}

#[tokio::test]
async fn concurrent_continuation_claims_execute_model_only_once() {
    let user = "concurrent-continuation";
    let (store, sessions) = test_stores();
    character_truncation_backlog(&sessions, user);
    let provider = MockProvider::with_dream_replies(vec![Ok("NO_REPLY"), Ok("NO_REPLY")])
        .with_dream_delay(Duration::from_millis(100));
    let observable = provider.clone();
    let mut config = production_dream_config();
    config.max_input_chars = 800;
    let worker = production_worker(&store, provider, config);
    assert!(
        worker
            .run_once(private_context(user))
            .await
            .unwrap()
            .unwrap()
            .truncated
    );

    let first = tokio::spawn({
        let worker = worker.clone();
        async move { worker.run_once(private_context(user)).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second = worker.run_once(private_context(user)).await.unwrap();
    let first = first.await.unwrap().unwrap();

    assert!(first.is_some());
    assert!(second.is_none());
    assert_eq!(observable.requests().len(), 2);
}

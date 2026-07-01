use super::*;
use crate::config::GroupMessageMode;
use std::sync::{Mutex, atomic::AtomicBool};
use tokio::sync::{Barrier, Notify};

#[derive(Default)]
struct RecordingHandler {
    events: Mutex<Vec<String>>,
    active_by_scope: Mutex<HashMap<String, usize>>,
    max_active_by_scope: Mutex<HashMap<String, usize>>,
    active_total: AtomicU64,
    max_active_total: AtomicU64,
    released: AtomicBool,
    release: Notify,
    block: bool,
    start_barrier: Option<Arc<Barrier>>,
}

impl RecordingHandler {
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    fn max_for_scope(&self, scope: &str) -> usize {
        self.max_active_by_scope
            .lock()
            .unwrap()
            .get(scope)
            .copied()
            .unwrap_or_default()
    }

    fn max_total(&self) -> u64 {
        self.max_active_total.load(Ordering::Relaxed)
    }

    fn release_all(&self) {
        self.released.store(true, Ordering::Relaxed);
        self.release.notify_waiters();
    }
}

impl MessageHandler for RecordingHandler {
    fn handle<'a>(&'a self, message: InboundEnvelope) -> HandlerFuture<'a> {
        Box::pin(async move {
            let (scope, id) = match message {
                InboundEnvelope::C2c(message) => {
                    (scope_key_from_c2c_message(&message), message.message_id)
                }
                InboundEnvelope::Group(message) => {
                    (scope_key_from_group_message(&message), message.message_id)
                }
            };
            self.events
                .lock()
                .unwrap()
                .push(format!("start:{scope}:{id}"));
            {
                let mut active = self.active_by_scope.lock().unwrap();
                let current = active.entry(scope.clone()).or_default();
                *current += 1;
                let mut max_active = self.max_active_by_scope.lock().unwrap();
                let max = max_active.entry(scope.clone()).or_default();
                *max = (*max).max(*current);
            }
            let total = self.active_total.fetch_add(1, Ordering::Relaxed) + 1;
            self.max_active_total.fetch_max(total, Ordering::Relaxed);
            if let Some(barrier) = &self.start_barrier {
                barrier.wait().await;
            }
            if self.block {
                while !self.released.load(Ordering::Relaxed) {
                    self.release.notified().await;
                }
            }
            self.active_total.fetch_sub(1, Ordering::Relaxed);
            {
                let mut active = self.active_by_scope.lock().unwrap();
                *active.get_mut(&scope).unwrap() -= 1;
            }
            self.events
                .lock()
                .unwrap()
                .push(format!("end:{scope}:{id}"));
            Ok(())
        })
    }
}

fn test_config() -> AppConfig {
    AppConfig {
        app_id: "appid".to_owned(),
        app_secret: "secret".to_owned(),
        sandbox: false,
        api_base: "https://example.test".to_owned(),
        token_refresh_margin: Duration::from_secs(60),
        enable_markdown: false,
        enable_image: false,
        enable_group_messages: true,
        verbose_log: false,
        group_message_mode: GroupMessageMode::Mention,
        group_active_keywords: vec!["小女仆".to_owned()],
        conversation_queue_capacity: 8,
        max_active_conversation_workers: 4,
        conversation_worker_idle_timeout: Duration::from_secs(60),
        message_aggregation: crate::config::MessageAggregationConfig {
            private_enabled: true,
            group_enabled: false,
            quiet: Duration::from_millis(1200),
            max_wait: Duration::from_millis(3000),
            max_messages: 10,
            max_chars: 12000,
            max_active_keys: 1024,
        },
        markdown_chunk_soft_limit: 1800,
        text_chunk_soft_limit: 1800,
    }
}

fn c2c(id: &str, user: &str) -> C2cMessage {
    C2cMessage {
        message_id: id.to_owned(),
        event_id: Some(format!("event-{id}")),
        source_message_ids: vec![id.to_owned()],
        source_event_ids: vec![format!("event-{id}")],
        user_openid: user.to_owned(),
        content: "hello".to_owned(),
        reply: None,
        timestamp: None,
        first_message_timestamp: None,
        last_message_timestamp: None,
        attachments: Vec::new(),
    }
}

fn group(id: &str, group_openid: &str) -> GroupMessage {
    GroupMessage {
        message_id: id.to_owned(),
        group_openid: group_openid.to_owned(),
        member_openid: Some("member".to_owned()),
        content: "hello".to_owned(),
        reply: None,
        timestamp: None,
        attachments: Vec::new(),
        event_type: super::super::event::GroupEventType::GroupAtMessage,
        author_is_bot: false,
        author_is_self: false,
    }
}

fn queued_c2c(id: &str, user: &str) -> QueuedMessage {
    let message = c2c(id, user);
    QueuedMessage {
        reject_target: RejectTarget::C2c {
            user_openid: message.user_openid.clone(),
            message_id: message.message_id.clone(),
        },
        envelope: InboundEnvelope::C2c(message),
        processed_ack: None,
        notify_on_reject: true,
    }
}

fn queued_group(id: &str, group_openid: &str) -> QueuedMessage {
    let message = group(id, group_openid);
    QueuedMessage {
        reject_target: RejectTarget::Group {
            group_openid: message.group_openid.clone(),
            message_id: message.message_id.clone(),
        },
        envelope: InboundEnvelope::Group(message),
        processed_ack: None,
        notify_on_reject: true,
    }
}

fn test_actor_with_handler(
    config: AppConfig,
    handler: Arc<dyn MessageHandler>,
) -> (
    DispatcherActor,
    mpsc::Receiver<DispatcherCommand>,
    mpsc::Receiver<RejectNotification>,
) {
    let (command_tx, command_rx) = mpsc::channel(32);
    let (unused_command_tx, actor_command_rx) = mpsc::channel(32);
    drop(unused_command_tx);
    let (reject_tx, reject_rx) = mpsc::channel(32);
    let auth = AccessTokenManager::new(
        reqwest::Client::new(),
        config.app_id.clone(),
        config.app_secret.clone(),
        config.token_refresh_margin,
    );
    let api = QqApiClient::new(reqwest::Client::new(), config.api_base.clone(), auth);
    let actor = DispatcherActor::new(
        config,
        api,
        GatewayRuntimeStatus::new(),
        actor_command_rx,
        command_tx,
        reject_tx,
        mpsc::channel(1).1,
        Arc::new(RejectMetrics::default()),
        handler,
        CancellationToken::new(),
    );
    (actor, command_rx, reject_rx)
}

async fn wait_for_events(handler: &RecordingHandler, count: usize) {
    timeout(Duration::from_secs(2), async {
        loop {
            if handler.events.lock().unwrap().len() >= count {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn drain_worker_commands(
    actor: &mut DispatcherActor,
    command_rx: &mut mpsc::Receiver<DispatcherCommand>,
    count: usize,
) {
    for _ in 0..count {
        let command = timeout(Duration::from_secs(2), command_rx.recv())
            .await
            .unwrap()
            .unwrap();
        actor.handle_command(command).await;
    }
}

#[tokio::test]
async fn same_scope_messages_keep_fifo_order() {
    let handler = Arc::new(RecordingHandler::default());
    let (mut actor, mut command_rx, _) = test_actor_with_handler(test_config(), handler.clone());
    let scope = "private:user-a".to_owned();

    actor
        .enqueue(scope.clone(), queued_c2c("m1", "user-a"))
        .await
        .unwrap();
    actor
        .enqueue(scope.clone(), queued_c2c("m2", "user-a"))
        .await
        .unwrap();
    drain_worker_commands(&mut actor, &mut command_rx, 2).await;
    wait_for_events(&handler, 4).await;

    assert_eq!(
        handler.events(),
        vec![
            "start:private:user-a:m1",
            "end:private:user-a:m1",
            "start:private:user-a:m2",
            "end:private:user-a:m2",
        ]
    );
    assert_eq!(handler.max_for_scope("private:user-a"), 1);
}

#[tokio::test]
async fn different_scopes_can_overlap() {
    let barrier = Arc::new(Barrier::new(2));
    let handler = Arc::new(RecordingHandler {
        block: true,
        start_barrier: Some(barrier),
        ..RecordingHandler::default()
    });
    let (mut actor, mut command_rx, _) = test_actor_with_handler(test_config(), handler.clone());

    actor
        .enqueue("private:user-a".to_owned(), queued_c2c("m1", "user-a"))
        .await
        .unwrap();
    actor
        .enqueue("private:user-b".to_owned(), queued_c2c("m2", "user-b"))
        .await
        .unwrap();
    drain_worker_commands(&mut actor, &mut command_rx, 2).await;
    wait_for_events(&handler, 2).await;

    assert_eq!(handler.max_total(), 2);
    handler.release_all();
    wait_for_events(&handler, 4).await;
}

#[tokio::test]
async fn idle_expiry_race_does_not_create_parallel_same_scope_workers() {
    async fn handle_idle_expired(
        actor: &mut DispatcherActor,
        scope: &str,
        generation: u64,
    ) -> IdleDecision {
        let (reply_tx, reply_rx) = oneshot::channel();
        actor
            .handle_command(DispatcherCommand::WorkerIdleExpired {
                scope_key: scope.to_owned(),
                generation,
                reply: reply_tx,
            })
            .await;
        reply_rx.await.unwrap()
    }

    async fn handle_enqueue(actor: &mut DispatcherActor, scope: &str, message: QueuedMessage) {
        let (ack_tx, ack_rx) = oneshot::channel();
        actor
            .handle_command(DispatcherCommand::Enqueue {
                scope_key: scope.to_owned(),
                message: Box::new(message),
                ack: ack_tx,
            })
            .await;
        ack_rx.await.unwrap().unwrap();
    }

    {
        let handler = Arc::new(RecordingHandler::default());
        let (mut actor, mut command_rx, _) =
            test_actor_with_handler(test_config(), handler.clone());
        let scope = "private:user-a".to_owned();

        actor
            .enqueue(scope.clone(), queued_c2c("m1", "user-a"))
            .await
            .unwrap();
        drain_worker_commands(&mut actor, &mut command_rx, 1).await;
        wait_for_events(&handler, 2).await;
        let generation = actor.scopes.get(&scope).unwrap().generation;

        // 顺序一：空闲到期先被 actor 处理，新消息随后进入 Retiring backlog。
        assert_eq!(
            handle_idle_expired(&mut actor, &scope, generation).await,
            IdleDecision::RetireNow
        );
        handle_enqueue(&mut actor, &scope, queued_c2c("m2", "user-a")).await;
        let entry = actor.scopes.get(&scope).unwrap();
        assert_eq!(entry.generation, generation);
        assert_eq!(entry.state, ScopeState::Retiring);
        assert!(entry.sender.is_none());
        assert_eq!(entry.backlog.len(), 1);

        actor
            .worker_exited(scope.clone(), generation, WorkerExitReason::Completed)
            .await;
        drain_worker_commands(&mut actor, &mut command_rx, 1).await;
        wait_for_events(&handler, 4).await;
        assert_eq!(handler.max_for_scope(&scope), 1);
    }

    {
        let handler = Arc::new(RecordingHandler {
            block: true,
            ..RecordingHandler::default()
        });
        let (mut actor, mut command_rx, _) =
            test_actor_with_handler(test_config(), handler.clone());
        let scope = "private:user-b".to_owned();

        actor
            .enqueue(scope.clone(), queued_c2c("m1", "user-b"))
            .await
            .unwrap();
        drain_worker_commands(&mut actor, &mut command_rx, 1).await;
        let generation = actor.scopes.get(&scope).unwrap().generation;

        // 顺序二：新消息先进入 Active worker queue，随后到达的空闲到期命令必须留在 Active。
        handle_enqueue(&mut actor, &scope, queued_c2c("m2", "user-b")).await;
        assert_eq!(
            handle_idle_expired(&mut actor, &scope, generation).await,
            IdleDecision::StayActive
        );
        let entry = actor.scopes.get(&scope).unwrap();
        assert_eq!(entry.generation, generation);
        assert_eq!(entry.state, ScopeState::Active);
        assert!(entry.sender.is_some());
        assert_eq!(entry.queue_len, 1);

        assert_eq!(handler.max_for_scope(&scope), 1);
        handler.release_all();
        wait_for_events(&handler, 4).await;
        drain_worker_commands(&mut actor, &mut command_rx, 1).await;
        assert_eq!(handler.max_for_scope(&scope), 1);
    }
}

#[tokio::test]
async fn retiring_backlog_replays_in_original_order() {
    let handler = Arc::new(RecordingHandler::default());
    let (mut actor, mut command_rx, _) = test_actor_with_handler(test_config(), handler.clone());
    let scope = "private:user-a".to_owned();

    actor
        .enqueue(scope.clone(), queued_c2c("m1", "user-a"))
        .await
        .unwrap();
    drain_worker_commands(&mut actor, &mut command_rx, 1).await;
    wait_for_events(&handler, 2).await;
    let current_generation = actor.scopes.get(&scope).unwrap().generation;
    assert_eq!(
        actor.worker_idle_expired(&scope, current_generation),
        IdleDecision::RetireNow
    );
    actor
        .enqueue(scope.clone(), queued_c2c("m2", "user-a"))
        .await
        .unwrap();
    actor
        .enqueue(scope.clone(), queued_c2c("m3", "user-a"))
        .await
        .unwrap();

    let generation = actor.scopes.get(&scope).unwrap().generation;
    actor
        .worker_exited(scope.clone(), generation, WorkerExitReason::Completed)
        .await;
    drain_worker_commands(&mut actor, &mut command_rx, 2).await;
    wait_for_events(&handler, 6).await;

    let starts = handler
        .events()
        .into_iter()
        .filter(|event| event.starts_with("start:"))
        .collect::<Vec<_>>();
    assert_eq!(
        starts,
        vec![
            "start:private:user-a:m1",
            "start:private:user-a:m2",
            "start:private:user-a:m3",
        ]
    );
}

#[tokio::test]
async fn successor_slot_exhaustion_rejects_each_backlog_target() {
    let handler = Arc::new(RecordingHandler::default());
    let (mut actor, _command_rx, mut reject_rx) = test_actor_with_handler(test_config(), handler);
    let scope = "private:user-a".to_owned();
    let generation = actor.next_generation();
    actor.scopes.insert(
        scope.clone(),
        ScopeEntry {
            state: ScopeState::Retiring,
            generation,
            sender: None,
            queue_len: 0,
            backlog: VecDeque::from([
                queued_c2c("m1", "user-a"),
                queued_c2c("m2", "user-a"),
                queued_c2c("m3", "user-a"),
            ]),
            worker_cancel: actor.shutdown_token.child_token(),
        },
    );
    let _held_permits = (0..actor.config.max_active_conversation_workers)
        .map(|_| actor.worker_slots.clone().try_acquire_owned().unwrap())
        .collect::<Vec<_>>();

    actor
        .worker_exited(scope.clone(), generation, WorkerExitReason::Completed)
        .await;

    let mut rejected = Vec::new();
    for _ in 0..3 {
        let notification = reject_rx.recv().await.unwrap();
        match notification.target {
            RejectTarget::C2c { message_id, .. } => rejected.push(message_id),
            RejectTarget::Group { .. } => panic!("expected c2c reject target"),
        }
    }
    assert_eq!(rejected, vec!["m1", "m2", "m3"]);
}

#[tokio::test]
async fn command_queue_full_applies_backpressure_until_capacity_frees() {
    let (command_tx, mut command_rx) = mpsc::channel(1);
    let (reject_tx, mut reject_rx) = mpsc::channel(1);
    let metrics = Arc::new(RejectMetrics::default());
    let handle = MessageDispatcherHandle {
        command_tx: command_tx.clone(),
        reject_tx,
    };
    command_tx
        .try_send(DispatcherCommand::WorkerDequeued {
            scope_key: "occupied".to_owned(),
            generation: 1,
        })
        .unwrap();

    let enqueue_task = tokio::spawn({
        let handle = handle.clone();
        async move {
            handle
                .enqueue(
                    InboundEnvelope::C2c(c2c("m1", "user-a")),
                    "private:user-a".to_owned(),
                    RejectTarget::C2c {
                        user_openid: "user-a".to_owned(),
                        message_id: "m1".to_owned(),
                    },
                    None,
                    true,
                )
                .await
        }
    });

    timeout(Duration::from_millis(50), async { reject_rx.recv().await })
        .await
        .expect_err("backpressure should not trigger immediate reject notification");
    command_rx
        .recv()
        .await
        .expect("occupied command should be released");
    let command = command_rx
        .recv()
        .await
        .expect("enqueue should succeed after capacity frees");
    let DispatcherCommand::Enqueue { ack, .. } = command else {
        panic!("expected enqueue command");
    };
    let _ = ack.send(Ok(()));
    enqueue_task.await.unwrap().unwrap();
    assert_eq!(metrics.total.load(Ordering::Relaxed), 0);
    assert_eq!(metrics.dropped.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn closed_command_channel_returns_error_without_busy_reject() {
    let (command_tx, command_rx) = mpsc::channel(1);
    let (reject_tx, mut reject_rx) = mpsc::channel(1);
    let metrics = Arc::new(RejectMetrics::default());
    let handle = MessageDispatcherHandle {
        command_tx,
        reject_tx,
    };
    drop(command_rx);

    let err = handle
        .enqueue(
            InboundEnvelope::C2c(c2c("m1", "user-a")),
            "private:user-a".to_owned(),
            RejectTarget::C2c {
                user_openid: "user-a".to_owned(),
                message_id: "m1".to_owned(),
            },
            None,
            true,
        )
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        DispatcherEnqueueError::Unavailable {
            reason: "dispatcher_closed"
        }
    ));
    assert!(reject_rx.try_recv().is_err());
    assert_eq!(metrics.total.load(Ordering::Relaxed), 0);
    assert_eq!(metrics.dropped.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn shutdown_rejects_new_messages() {
    let handler = Arc::new(RecordingHandler::default());
    let (mut actor, _command_rx, _) = test_actor_with_handler(test_config(), handler);
    actor.shutdown_token.cancel();

    let err = actor
        .enqueue("private:user-a".to_owned(), queued_c2c("m1", "user-a"))
        .await
        .unwrap_err();

    assert!(matches!(
        err,
        DispatcherEnqueueError::Unavailable {
            reason: "dispatcher_shutdown"
        }
    ));
    assert!(actor.scopes.is_empty());
}

#[tokio::test]
async fn group_reject_target_keeps_own_message_id() {
    let handler = Arc::new(RecordingHandler::default());
    let (mut actor, _command_rx, mut reject_rx) = test_actor_with_handler(test_config(), handler);
    let scope = "group:group-a".to_owned();
    let generation = actor.next_generation();
    actor.scopes.insert(
        scope.clone(),
        ScopeEntry {
            state: ScopeState::Retiring,
            generation,
            sender: None,
            queue_len: 0,
            backlog: VecDeque::from([queued_group("g1", "group-a"), queued_group("g2", "group-a")]),
            worker_cancel: actor.shutdown_token.child_token(),
        },
    );
    let _held_permits = (0..actor.config.max_active_conversation_workers)
        .map(|_| actor.worker_slots.clone().try_acquire_owned().unwrap())
        .collect::<Vec<_>>();

    actor
        .worker_exited(scope, generation, WorkerExitReason::Completed)
        .await;

    let first = reject_rx.recv().await.unwrap();
    let second = reject_rx.recv().await.unwrap();
    match (first.target, second.target) {
        (RejectTarget::Group { message_id: a, .. }, RejectTarget::Group { message_id: b, .. }) => {
            assert_eq!((a, b), ("g1".to_owned(), "g2".to_owned()))
        }
        _ => panic!("expected group reject targets"),
    }
}

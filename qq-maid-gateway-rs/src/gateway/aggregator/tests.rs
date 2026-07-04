use super::{AggregationDispatcher, MessageAggregator, MessageAggregatorHandle};
use crate::{
    config::{AgentTypingConfig, AppConfig, GroupMessageMode, MessageAggregationConfig},
    gateway::{
        dedupe::MessageDedupe,
        dispatcher::DispatcherEnqueueError,
        event::{C2cMessage, GroupMessage},
    },
    respond::{RespondClient, scope_key_from_c2c_message},
};
use async_trait::async_trait;
use qq_maid_core::service::{
    CoreActor, CoreConversation, CoreError, CoreHealthSnapshot, CoreInboundClassification,
    CoreInboundKind, CoreRequest, CoreRespondOutput, CoreResponse, CoreService, Platform,
    UpstreamStatusSnapshot,
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::oneshot,
    time::{advance, pause, timeout},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct MockCore {
    pending: Mutex<HashSet<String>>,
    fail_classify: AtomicBool,
}

#[async_trait]
impl CoreService for MockCore {
    async fn respond(&self, _request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
        Ok(CoreRespondOutput::Complete(CoreResponse {
            text: Some("ok".to_owned()),
            markdown: None,
            handled: Some(true),
            session_id: None,
            command: None,
            diagnostics: None,
        }))
    }

    async fn classify_inbound(
        &self,
        request: CoreRequest,
    ) -> Result<CoreInboundClassification, CoreError> {
        if self.fail_classify.load(Ordering::Relaxed) {
            return Err(CoreError::new("internal", "classify", "failed"));
        }
        let scope = request.scope_key();
        if self.pending.lock().unwrap().contains(&scope) {
            return Ok(CoreInboundClassification {
                kind: CoreInboundKind::Immediate,
            });
        }
        let text = request.text.trim();
        let is_command = text.starts_with('/') || text.starts_with('／');
        Ok(CoreInboundClassification {
            kind: if is_command {
                CoreInboundKind::Immediate
            } else {
                CoreInboundKind::NormalChat
            },
        })
    }

    async fn upstream_check(&self) -> Result<(), CoreError> {
        Ok(())
    }

    fn health_snapshot(&self) -> CoreHealthSnapshot {
        CoreHealthSnapshot {
            ok: true,
            provider: "mock".to_owned(),
            model: "mock".to_owned(),
            stream: false,
            upstream: UpstreamStatusSnapshot::default(),
        }
    }
}

#[derive(Default)]
struct MockDispatcher {
    core: Arc<MockCore>,
    c2c: Mutex<Vec<C2cMessage>>,
    failure_notifications: Mutex<Vec<(String, String)>>,
    attempt_counts: Mutex<HashMap<String, usize>>,
    pending_acks: Mutex<VecDeque<(C2cMessage, oneshot::Sender<()>)>>,
    closed: AtomicBool,
    fail_next_enqueues: AtomicUsize,
    fail_next_contents: Mutex<VecDeque<String>>,
    notified_failures: Mutex<VecDeque<&'static str>>,
}

#[async_trait]
impl AggregationDispatcher for MockDispatcher {
    async fn enqueue_c2c(&self, message: C2cMessage) -> Result<(), DispatcherEnqueueError> {
        self.record_attempt(&message);
        if let Some(error) = self.next_enqueue_error(&message, true) {
            return Err(error);
        }
        if self.closed.load(Ordering::Relaxed) {
            return Err(DispatcherEnqueueError::Unavailable {
                reason: "dispatcher_closed",
            });
        }
        self.c2c.lock().unwrap().push(message);
        Ok(())
    }

    async fn enqueue_c2c_silent(&self, message: C2cMessage) -> Result<(), DispatcherEnqueueError> {
        self.record_attempt(&message);
        if let Some(error) = self.next_enqueue_error(&message, false) {
            return Err(error);
        }
        if self.closed.load(Ordering::Relaxed) {
            return Err(DispatcherEnqueueError::Unavailable {
                reason: "dispatcher_closed",
            });
        }
        self.c2c.lock().unwrap().push(message);
        Ok(())
    }

    async fn enqueue_c2c_with_processed_ack(
        &self,
        message: C2cMessage,
        processed_ack: oneshot::Sender<()>,
    ) -> Result<(), DispatcherEnqueueError> {
        self.record_attempt(&message);
        if let Some(error) = self.next_enqueue_error(&message, true) {
            return Err(error);
        }
        if self.closed.load(Ordering::Relaxed) {
            return Err(DispatcherEnqueueError::Unavailable {
                reason: "dispatcher_closed",
            });
        }
        self.c2c.lock().unwrap().push(message.clone());
        self.pending_acks
            .lock()
            .unwrap()
            .push_back((message, processed_ack));
        Ok(())
    }

    async fn enqueue_group(&self, _message: GroupMessage) -> Result<(), DispatcherEnqueueError> {
        Ok(())
    }

    async fn notify_c2c_failure(&self, message: &C2cMessage, text: &str) -> anyhow::Result<()> {
        self.failure_notifications
            .lock()
            .unwrap()
            .push((message.message_id.clone(), text.to_owned()));
        Ok(())
    }
}

impl MockDispatcher {
    fn record_attempt(&self, message: &C2cMessage) {
        let mut counts = self.attempt_counts.lock().unwrap();
        *counts.entry(message.content.clone()).or_default() += 1;
    }

    fn next_enqueue_error(
        &self,
        message: &C2cMessage,
        notify_on_reject: bool,
    ) -> Option<DispatcherEnqueueError> {
        if let Some(reason) = self.notified_failures.lock().unwrap().pop_front() {
            if notify_on_reject {
                self.failure_notifications.lock().unwrap().push((
                    message.message_id.clone(),
                    "当前消息较多，请稍后再试。".to_owned(),
                ));
                return Some(DispatcherEnqueueError::RejectedAndNotified { reason });
            }
            return Some(DispatcherEnqueueError::Unavailable {
                reason: "dispatcher_reject_suppressed",
            });
        }
        if self
            .fail_next_enqueues
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_sub(1)
            })
            .is_ok()
        {
            return Some(DispatcherEnqueueError::Unavailable {
                reason: "dispatcher_injected_failure",
            });
        }
        let mut contents = self.fail_next_contents.lock().unwrap();
        if let Some(index) = contents
            .iter()
            .position(|content| content == message.content.as_str())
        {
            contents.remove(index);
            return Some(DispatcherEnqueueError::Unavailable {
                reason: "dispatcher_injected_failure",
            });
        }
        None
    }

    fn fail_next(&self, count: usize) {
        self.fail_next_enqueues.store(count, Ordering::Relaxed);
    }

    fn fail_next_after_notifying(&self, reason: &'static str) {
        self.notified_failures.lock().unwrap().push_back(reason);
    }

    fn messages(&self) -> Vec<C2cMessage> {
        self.c2c.lock().unwrap().clone()
    }

    fn process_next(&self) {
        let Some((message, ack)) = self.pending_acks.lock().unwrap().pop_front() else {
            return;
        };
        self.apply_processed_side_effect(&message);
        let _ = ack.send(());
    }

    fn process_by_message_id(&self, message_id: &str) {
        let mut pending = self.pending_acks.lock().unwrap();
        let index = pending
            .iter()
            .position(|(message, _)| message.message_id == message_id)
            .expect("pending ack should exist");
        let (message, ack) = pending.remove(index).unwrap();
        drop(pending);
        self.apply_processed_side_effect(&message);
        let _ = ack.send(());
    }

    fn close_next_ack(&self) {
        let _ = self.pending_acks.lock().unwrap().pop_front();
    }

    fn process_all(&self) {
        while !self.pending_acks.lock().unwrap().is_empty() {
            self.process_next();
        }
    }

    fn apply_processed_side_effect(&self, message: &C2cMessage) {
        let scope = scope_key_from_c2c_message(message);
        let text = message.content.trim();
        if text.starts_with("/todo add") || text.starts_with("/memory") {
            self.core.pending.lock().unwrap().insert(scope);
        } else if matches!(text, "确认" | "取消") {
            self.core.pending.lock().unwrap().remove(&scope);
        }
    }

    fn pending_barriers(&self) -> usize {
        self.pending_acks.lock().unwrap().len()
    }

    fn failure_notifications(&self) -> Vec<(String, String)> {
        self.failure_notifications.lock().unwrap().clone()
    }
}

struct Harness {
    aggregator: MessageAggregator,
    dispatcher: Arc<MockDispatcher>,
    core: Arc<MockCore>,
    dedupe: Arc<MessageDedupe>,
}

fn test_config() -> AppConfig {
    AppConfig {
        app_id: "appid".to_owned(),
        app_secret: "secret".to_owned(),
        bot_mention_ids: Vec::new(),
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
        message_aggregation: MessageAggregationConfig {
            private_enabled: true,
            group_enabled: false,
            quiet: Duration::from_millis(100),
            max_wait: Duration::from_millis(300),
            max_messages: 3,
            max_chars: 12,
            max_active_keys: 4,
        },
        c2c_final_reply_stream_enabled: false,
        agent_typing: AgentTypingConfig {
            enabled: false,
            delay: Duration::from_secs(1),
        },
        markdown_chunk_soft_limit: 1800,
        text_chunk_soft_limit: 1800,
    }
}

fn harness_with_config(config: AppConfig) -> Harness {
    harness_with_config_and_dedupe_ttl(config, Duration::from_secs(60))
}

fn harness_with_config_and_dedupe_ttl(config: AppConfig, dedupe_ttl: Duration) -> Harness {
    let core = Arc::new(MockCore::default());
    let dispatcher = Arc::new(MockDispatcher {
        core: core.clone(),
        ..MockDispatcher::default()
    });
    let dedupe = Arc::new(MessageDedupe::new(dedupe_ttl));
    let aggregator = MessageAggregator::new_with_dispatcher(
        config,
        RespondClient::new(core.clone()),
        dispatcher.clone(),
        dedupe.clone(),
        Arc::new(Mutex::new(HashMap::new())),
        CancellationToken::new(),
    );
    Harness {
        aggregator,
        dispatcher,
        core,
        dedupe,
    }
}

fn harness() -> Harness {
    harness_with_config(test_config())
}

fn c2c(id: &str, user: &str, content: &str) -> C2cMessage {
    C2cMessage {
        message_id: id.to_owned(),
        event_id: Some(format!("event-{id}")),
        source_message_ids: vec![id.to_owned()],
        source_event_ids: vec![format!("event-{id}")],
        user_openid: user.to_owned(),
        content: content.to_owned(),
        reply: None,
        timestamp: Some(format!("2026-06-10T12:00:0{id}+08:00")),
        first_message_timestamp: Some(format!("2026-06-10T12:00:0{id}+08:00")),
        last_message_timestamp: Some(format!("2026-06-10T12:00:0{id}+08:00")),
        attachments: Vec::new(),
    }
}

async fn yield_actor() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

async fn wait_for_messages(dispatcher: &MockDispatcher, count: usize) {
    for _ in 0..50 {
        if dispatcher.messages().len() >= count {
            return;
        }
        advance(Duration::ZERO).await;
        tokio::task::yield_now().await;
    }
}

async fn wait_for_barrier_state(
    handle: &MessageAggregatorHandle,
    barrier_count: usize,
    task_count: usize,
) {
    for _ in 0..50 {
        let state = handle.debug_barrier_state().await;
        if state.barrier_count == barrier_count && state.task_count == task_count {
            return;
        }
        advance(Duration::ZERO).await;
        tokio::task::yield_now().await;
    }
    let state = handle.debug_barrier_state().await;
    assert_eq!(state.barrier_count, barrier_count);
    assert_eq!(state.task_count, task_count);
}

async fn enqueue(handle: &MessageAggregatorHandle, message: C2cMessage) {
    handle.enqueue_c2c(message).await.unwrap();
    yield_actor().await;
}

#[tokio::test]
async fn immediate_enqueue_failure_rolls_back_message_id_for_retry() {
    let h = harness();
    let handle = h.aggregator.handle();
    h.dispatcher.fail_next(1);
    assert!(handle.enqueue_c2c(c2c("1", "u1", "/todo")).await.is_err());
    enqueue(&handle, c2c("1", "u1", "/todo retry")).await;

    assert_eq!(h.dispatcher.messages().len(), 1);
    assert_eq!(h.dispatcher.messages()[0].message_id, "1");
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn immediate_enqueue_failure_rolls_back_event_id_for_retry() {
    let h = harness();
    let handle = h.aggregator.handle();
    let first = c2c("1", "u1", "/todo");
    let mut retry = c2c("2", "u1", "/todo retry");
    retry.event_id = first.event_id.clone();
    retry.source_event_ids = first.source_event_ids.clone();

    h.dispatcher.fail_next(1);
    assert!(handle.enqueue_c2c(first).await.is_err());
    enqueue(&handle, retry).await;

    assert_eq!(h.dispatcher.messages().len(), 1);
    assert_eq!(h.dispatcher.messages()[0].message_id, "2");
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn successful_immediate_dispatch_commits_message_and_event_ids() {
    let h = harness();
    let handle = h.aggregator.handle();
    let first = c2c("1", "u1", "/todo");
    let mut retry = c2c("2", "u1", "/todo retry");
    retry.event_id = first.event_id.clone();
    retry.source_event_ids = first.source_event_ids.clone();

    enqueue(&handle, first).await;
    enqueue(&handle, retry).await;

    assert_eq!(h.dispatcher.messages().len(), 1);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn quiet_timeout_flush_failure_rolls_back_and_notifies_user() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "hello")).await;
    h.dispatcher.fail_next(1);
    advance(Duration::from_millis(101)).await;
    yield_actor().await;

    assert!(h.dispatcher.messages().is_empty());
    assert!(!h.dedupe.contains_recent("1"));
    assert_eq!(
        h.dispatcher.failure_notifications(),
        vec![(
            "1".to_owned(),
            "当前服务暂时不可用，请稍后再试。".to_owned()
        )]
    );
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn max_messages_flush_failure_rolls_back_and_notifies_once() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    enqueue(&handle, c2c("2", "u1", "b")).await;
    h.dispatcher.fail_next(1);

    assert!(handle.enqueue_c2c(c2c("3", "u1", "c")).await.is_err());
    yield_actor().await;

    assert!(h.dispatcher.messages().is_empty());
    assert!(!h.dedupe.contains_recent("1"));
    assert!(!h.dedupe.contains_recent("2"));
    assert!(!h.dedupe.contains_recent("3"));
    assert_eq!(
        h.dispatcher.failure_notifications(),
        vec![(
            "3".to_owned(),
            "当前服务暂时不可用，请稍后再试。".to_owned()
        )]
    );
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn max_chars_flush_failure_rolls_back_and_notifies_once() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "123456")).await;
    h.dispatcher.fail_next(1);

    assert!(handle.enqueue_c2c(c2c("2", "u1", "123456")).await.is_err());
    yield_actor().await;

    assert!(h.dispatcher.messages().is_empty());
    assert!(!h.dedupe.contains_recent("1"));
    assert!(!h.dedupe.contains_recent("2"));
    assert_eq!(
        h.dispatcher.failure_notifications(),
        vec![(
            "2".to_owned(),
            "当前服务暂时不可用，请稍后再试。".to_owned()
        )]
    );
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn failed_old_batch_flush_rejects_current_message_and_notifies_once_per_message() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "1234567")).await;
    h.dispatcher.fail_next(1);
    assert!(handle.enqueue_c2c(c2c("2", "u1", "/todo")).await.is_err());

    assert!(h.dispatcher.messages().is_empty());
    assert!(!h.dedupe.contains_recent("1"));
    assert!(!h.dedupe.contains_recent("2"));
    assert_eq!(
        h.dispatcher.failure_notifications(),
        vec![(
            "2".to_owned(),
            "当前服务暂时不可用，请稍后再试。".to_owned()
        )]
    );
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn dispatcher_failure_for_immediate_message_rolls_back_and_notifies_user() {
    let h = harness();
    let handle = h.aggregator.handle();
    h.dispatcher.fail_next(1);
    assert!(handle.enqueue_c2c(c2c("1", "u1", "/todo")).await.is_err());

    assert!(!h.dedupe.contains_recent("1"));
    assert_eq!(
        h.dispatcher.failure_notifications(),
        vec![(
            "1".to_owned(),
            "当前服务暂时不可用，请稍后再试。".to_owned()
        )]
    );
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn immediate_dispatch_unavailable_notifies_service_unavailable_once() {
    let h = harness();
    let handle = h.aggregator.handle();
    h.dispatcher.fail_next(1);

    assert!(
        handle
            .enqueue_c2c(c2c("1", "u1", "/memory 记住这个"))
            .await
            .is_err()
    );

    assert!(!h.dedupe.contains_recent("1"));
    assert_eq!(
        h.dispatcher.failure_notifications(),
        vec![(
            "1".to_owned(),
            "当前服务暂时不可用，请稍后再试。".to_owned()
        )]
    );
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn conversation_queue_full_does_not_double_notify() {
    let h = harness();
    let handle = h.aggregator.handle();
    h.dispatcher
        .fail_next_after_notifying("conversation_queue_full");

    assert!(handle.enqueue_c2c(c2c("1", "u1", "/todo")).await.is_err());

    assert!(!h.dedupe.contains_recent("1"));
    assert_eq!(
        h.dispatcher.failure_notifications(),
        vec![("1".to_owned(), "当前消息较多，请稍后再试。".to_owned())]
    );
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn worker_slot_exhausted_does_not_double_notify() {
    let h = harness();
    let handle = h.aggregator.handle();
    h.dispatcher
        .fail_next_after_notifying("worker_slot_exhausted");

    assert!(handle.enqueue_c2c(c2c("1", "u1", "/todo")).await.is_err());

    assert!(!h.dedupe.contains_recent("1"));
    assert_eq!(
        h.dispatcher.failure_notifications(),
        vec![("1".to_owned(), "当前消息较多，请稍后再试。".to_owned())]
    );
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn shutdown_flush_failure_rolls_back_reservations_without_user_notification() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "hello")).await;
    h.dispatcher
        .fail_next_after_notifying("conversation_queue_full");
    h.aggregator.shutdown().await;

    assert!(h.dispatcher.messages().is_empty());
    assert!(!h.dedupe.contains_recent("1"));
    assert!(h.dispatcher.failure_notifications().is_empty());
}

#[tokio::test]
async fn single_message_quiet_timeout_flushes() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "hello")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 1).await;

    assert_eq!(h.dispatcher.messages()[0].content, "hello");
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn multiple_messages_merge_in_order() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    enqueue(&handle, c2c("2", "u1", "b")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 1).await;

    let messages = h.dispatcher.messages();
    assert_eq!(messages[0].content, "a\nb");
    assert_eq!(messages[0].message_id, "2");
    assert_eq!(messages[0].source_message_ids, vec!["1", "2"]);
    assert_eq!(messages[0].source_event_ids, vec!["event-1", "event-2"]);
    assert_eq!(
        messages[0].first_message_timestamp.as_deref(),
        Some("2026-06-10T12:00:01+08:00")
    );
    assert_eq!(
        messages[0].last_message_timestamp.as_deref(),
        Some("2026-06-10T12:00:02+08:00")
    );
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn quiet_deadline_resets() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    advance(Duration::from_millis(80)).await;
    enqueue(&handle, c2c("2", "u1", "b")).await;
    advance(Duration::from_millis(90)).await;
    yield_actor().await;
    assert!(h.dispatcher.messages().is_empty());
    advance(Duration::from_millis(11)).await;
    wait_for_messages(&h.dispatcher, 1).await;
    assert_eq!(h.dispatcher.messages().len(), 1);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn hard_deadline_does_not_reset() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    advance(Duration::from_millis(90)).await;
    enqueue(&handle, c2c("2", "u1", "b")).await;
    advance(Duration::from_millis(90)).await;
    enqueue(&handle, c2c("3", "u1", "c")).await;
    advance(Duration::from_millis(120)).await;
    wait_for_messages(&h.dispatcher, 1).await;
    assert_eq!(h.dispatcher.messages().len(), 1);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn max_wait_forces_flush() {
    pause();
    let mut config = test_config();
    config.message_aggregation.quiet = Duration::from_secs(60);
    config.message_aggregation.max_wait = Duration::from_millis(300);
    let h = harness_with_config(config);
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    advance(Duration::from_millis(301)).await;
    wait_for_messages(&h.dispatcher, 1).await;
    assert_eq!(h.dispatcher.messages().len(), 1);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn max_messages_equal_and_exceeded_flushes() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    enqueue(&handle, c2c("2", "u1", "b")).await;
    enqueue(&handle, c2c("3", "u1", "c")).await;
    yield_actor().await;
    assert_eq!(h.dispatcher.messages()[0].content, "a\nb\nc");
    enqueue(&handle, c2c("4", "u1", "d")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 2).await;
    assert_eq!(h.dispatcher.messages().len(), 2);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn max_chars_equal_and_exceeded_flushes() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "123456")).await;
    enqueue(&handle, c2c("2", "u1", "123456")).await;
    yield_actor().await;
    assert_eq!(h.dispatcher.messages()[0].content, "123456\n123456");
    enqueue(&handle, c2c("3", "u1", "x")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 2).await;
    assert_eq!(h.dispatcher.messages().len(), 2);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn oversized_single_message_dispatches_immediately() {
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "1234567890123")).await;
    assert_eq!(h.dispatcher.messages()[0].content, "1234567890123");
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn two_users_aggregate_independently() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    enqueue(&handle, c2c("2", "u2", "b")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 2).await;
    assert_eq!(h.dispatcher.messages().len(), 2);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn command_flushes_batch_and_preserves_order() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    enqueue(&handle, c2c("2", "u1", "/todo")).await;
    let messages = h.dispatcher.messages();
    assert_eq!(messages[0].content, "a");
    assert_eq!(messages[1].content, "/todo");
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn consecutive_barriers_keep_pending_input_immediate() {
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "/todo add 无时间买牛奶")).await;
    enqueue(&handle, c2c("2", "u1", "/resume")).await;
    enqueue(&handle, c2c("3", "u1", "取消")).await;
    let messages = h.dispatcher.messages();
    assert_eq!(
        messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["/todo add 无时间买牛奶", "/resume", "取消"]
    );
    assert_eq!(h.dispatcher.pending_barriers(), 3);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn plain_cancel_without_pending_can_aggregate() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "取消")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 1).await;
    assert_eq!(h.dispatcher.messages()[0].content, "取消");
    assert_eq!(h.dispatcher.pending_barriers(), 0);
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn message_id_retry_is_deduped() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    enqueue(&handle, c2c("1", "u1", "a")).await;
    enqueue(&handle, c2c("1", "u1", "a retry")).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 1).await;
    assert_eq!(h.dispatcher.messages()[0].content, "a");
    h.aggregator.shutdown().await;
}

#[tokio::test]
async fn event_id_retry_is_deduped_even_with_different_message_id() {
    pause();
    let h = harness();
    let handle = h.aggregator.handle();
    let first = c2c("1", "u1", "a");
    let mut retry = c2c("2", "u1", "a retry");
    retry.event_id = first.event_id.clone();
    retry.source_event_ids = first.source_event_ids.clone();
    enqueue(&handle, first).await;
    enqueue(&handle, retry).await;
    advance(Duration::from_millis(101)).await;
    wait_for_messages(&h.dispatcher, 1).await;
    assert_eq!(h.dispatcher.messages()[0].content, "a");
    h.aggregator.shutdown().await;
}

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
        text: "hello".to_owned(),
        platform: Platform::QqOfficial,
        actor: CoreActor {
            user_id: Some("u1".to_owned()),
            group_member_role: None,
        },
        conversation: CoreConversation::Private {
            peer_id: "u1".to_owned(),
        },
    };
    assert_eq!(request.scope_key(), "private:u1");
}

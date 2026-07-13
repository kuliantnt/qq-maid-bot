//! OneBot 11 入站的有界会话级调度边界。
//!
//! WebSocket 读循环只调用 [`OneBotScopeDispatcher::enqueue`] 做同步、快速入队；
//! Core 与 sender 都在受追踪的 scope worker 中执行。同一 Core scope 只有一个 FIFO
//! worker，不同 scope 可并发，worker 空闲后会回收。

use std::{
    collections::HashMap,
    panic::AssertUnwindSafe,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures_util::FutureExt;
use thiserror::Error;
use tokio::{sync::mpsc, task::AbortHandle, time::timeout};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::{debug, info, warn};

use crate::{
    config::AppConfig,
    gateway::{
        dedupe::MessageDedupe,
        logging::mask_scope_key,
        platform::{self, InboundMessage},
    },
};

use super::dispatch::OneBotInboundDispatcher;

const SHUTDOWN_CANCEL_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OneBotEnqueueOutcome {
    Accepted,
    Duplicate,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(super) enum OneBotEnqueueError {
    #[error("OneBot inbound dispatcher is shutting down")]
    Shutdown,
    #[error("OneBot inbound scope queue is full")]
    ScopeQueueFull,
    #[error("OneBot inbound active scope limit is reached")]
    ActiveScopeLimit,
    #[error("OneBot inbound message does not have a supported Core scope")]
    InvalidScope,
    #[error("OneBot inbound scope worker is unavailable")]
    WorkerUnavailable,
}

#[async_trait]
trait OneBotInboundHandler: Send + Sync {
    async fn handle(&self, inbound: InboundMessage);
}

#[async_trait]
impl OneBotInboundHandler for OneBotInboundDispatcher {
    async fn handle(&self, inbound: InboundMessage) {
        OneBotInboundDispatcher::log_result(self.dispatch(inbound).await);
    }
}

struct ScopeEntry {
    generation: u64,
    sender: mpsc::Sender<InboundMessage>,
    abort_handle: AbortHandle,
}

struct DispatcherState {
    accepting: bool,
    next_generation: u64,
    scopes: HashMap<String, ScopeEntry>,
}

struct DispatcherInner {
    state: Mutex<DispatcherState>,
    handler: Arc<dyn OneBotInboundHandler>,
    dedupe: Arc<MessageDedupe>,
    queue_capacity: usize,
    max_active_scopes: usize,
    idle_timeout: Duration,
    shutdown: CancellationToken,
    tasks: TaskTracker,
}

#[derive(Clone)]
pub(super) struct OneBotScopeDispatcher {
    inner: Arc<DispatcherInner>,
}

impl OneBotScopeDispatcher {
    pub(super) fn new(
        config: &AppConfig,
        handler: OneBotInboundDispatcher,
        dedupe: Arc<MessageDedupe>,
        parent_shutdown: &CancellationToken,
    ) -> Self {
        Self::with_handler(
            config.conversation_queue_capacity,
            config.max_active_conversation_workers,
            config.conversation_worker_idle_timeout,
            Arc::new(handler),
            dedupe,
            parent_shutdown.child_token(),
        )
    }

    fn with_handler(
        queue_capacity: usize,
        max_active_scopes: usize,
        idle_timeout: Duration,
        handler: Arc<dyn OneBotInboundHandler>,
        dedupe: Arc<MessageDedupe>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            inner: Arc::new(DispatcherInner {
                state: Mutex::new(DispatcherState {
                    accepting: true,
                    next_generation: 1,
                    scopes: HashMap::new(),
                }),
                handler,
                dedupe,
                queue_capacity,
                max_active_scopes,
                idle_timeout,
                shutdown,
                tasks: TaskTracker::new(),
            }),
        }
    }

    /// 只做 scope 计算、去重 reservation 和 `try_send`，不会等待 Core、sender 或 echo。
    pub(super) fn enqueue(
        &self,
        inbound: InboundMessage,
    ) -> Result<OneBotEnqueueOutcome, OneBotEnqueueError> {
        if self.inner.shutdown.is_cancelled() {
            return Err(OneBotEnqueueError::Shutdown);
        }
        let scope_key =
            platform::core_scope_key(&inbound).map_err(|_| OneBotEnqueueError::InvalidScope)?;
        let reservation = match inbound.dedupe_message_key() {
            Some(key) => match self.inner.dedupe.reserve_many([key], Instant::now()) {
                Ok(reservation) => Some(reservation),
                Err(_) => {
                    debug!(
                        scope_key = %mask_scope_key(&scope_key),
                        "ignored duplicate OneBot 11 message before Core dispatch"
                    );
                    return Ok(OneBotEnqueueOutcome::Duplicate);
                }
            },
            None => None,
        };

        let mut state = self
            .inner
            .state
            .lock()
            .expect("OneBot scope dispatcher lock should not be poisoned");
        if !state.accepting || self.inner.shutdown.is_cancelled() {
            return Err(OneBotEnqueueError::Shutdown);
        }

        if let Some(entry) = state.scopes.get(&scope_key) {
            match entry.sender.try_send(inbound) {
                Ok(()) => {
                    if let Some(reservation) = reservation {
                        reservation.commit();
                    }
                    return Ok(OneBotEnqueueOutcome::Accepted);
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!(
                        scope_key = %mask_scope_key(&scope_key),
                        queue_capacity = self.inner.queue_capacity,
                        "rejected OneBot 11 inbound message because scope queue is full"
                    );
                    return Err(OneBotEnqueueError::ScopeQueueFull);
                }
                Err(mpsc::error::TrySendError::Closed(inbound)) => {
                    // worker 正在退出但尚未清理 map 时，由下方在同一把锁内移除旧 generation
                    // 并尝试创建继任 worker；消息仍只会入队一次。
                    state.scopes.remove(&scope_key);
                    return self.enqueue_new_scope_locked(
                        &mut state,
                        scope_key,
                        inbound,
                        reservation,
                    );
                }
            }
        }

        self.enqueue_new_scope_locked(&mut state, scope_key, inbound, reservation)
    }

    fn enqueue_new_scope_locked(
        &self,
        state: &mut DispatcherState,
        scope_key: String,
        inbound: InboundMessage,
        reservation: Option<crate::gateway::dedupe::MessageReservation>,
    ) -> Result<OneBotEnqueueOutcome, OneBotEnqueueError> {
        if state.scopes.len() >= self.inner.max_active_scopes {
            warn!(
                scope_key = %mask_scope_key(&scope_key),
                active_scopes = state.scopes.len(),
                max_active_scopes = self.inner.max_active_scopes,
                "rejected OneBot 11 inbound message because active scope limit is reached"
            );
            return Err(OneBotEnqueueError::ActiveScopeLimit);
        }

        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1).max(1);
        let (sender, receiver) = mpsc::channel(self.inner.queue_capacity);
        sender
            .try_send(inbound)
            .map_err(|_| OneBotEnqueueError::WorkerUnavailable)?;
        let inner = Arc::clone(&self.inner);
        let worker_scope = scope_key.clone();
        let task = self.inner.tasks.spawn(async move {
            run_scope_worker(inner, worker_scope, generation, receiver).await;
        });
        let abort_handle = task.abort_handle();
        drop(task);
        state.scopes.insert(
            scope_key.clone(),
            ScopeEntry {
                generation,
                sender,
                abort_handle,
            },
        );
        if let Some(reservation) = reservation {
            reservation.commit();
        }
        info!(
            scope_key = %mask_scope_key(&scope_key),
            generation,
            active_scopes = state.scopes.len(),
            max_active_scopes = self.inner.max_active_scopes,
            "OneBot 11 scope dispatcher created worker"
        );
        Ok(OneBotEnqueueOutcome::Accepted)
    }

    /// shutdown 采用 cancel 策略：停止新入站，取消当前 dispatch，丢弃排队消息，并等待
    /// TaskTracker 确认全部 scope worker 退出；超时后显式 abort 再收口。
    pub(super) async fn shutdown(&self) {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("OneBot scope dispatcher lock should not be poisoned");
            state.accepting = false;
        }
        self.inner.shutdown.cancel();
        self.inner.tasks.close();

        if timeout(SHUTDOWN_CANCEL_TIMEOUT, self.inner.tasks.wait())
            .await
            .is_err()
        {
            let abort_handles = self
                .inner
                .state
                .lock()
                .expect("OneBot scope dispatcher lock should not be poisoned")
                .scopes
                .values()
                .map(|entry| entry.abort_handle.clone())
                .collect::<Vec<_>>();
            warn!(
                remaining_workers = abort_handles.len(),
                "OneBot 11 scope dispatcher cancellation timed out; aborting workers"
            );
            for abort_handle in abort_handles {
                abort_handle.abort();
            }
            self.inner.tasks.wait().await;
        }

        let remaining_scopes = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("OneBot scope dispatcher lock should not be poisoned");
            let remaining_scopes = state.scopes.len();
            // abort 不会继续执行 worker 尾部的 generation 清理；TaskTracker 已确认任务
            // 全部结束后可安全清空残留 sender/AbortHandle，不保留永久 scope 映射。
            state.scopes.clear();
            remaining_scopes
        };
        if remaining_scopes == 0 {
            info!("OneBot 11 scope dispatcher shutdown completed");
        } else {
            warn!(
                remaining_scopes,
                "OneBot 11 scope dispatcher stopped with stale scope entries"
            );
        }
    }
}

async fn run_scope_worker(
    inner: Arc<DispatcherInner>,
    scope_key: String,
    generation: u64,
    mut receiver: mpsc::Receiver<InboundMessage>,
) {
    loop {
        let next = tokio::select! {
            biased;
            _ = inner.shutdown.cancelled() => break,
            result = timeout(inner.idle_timeout, receiver.recv()) => result,
        };
        let inbound = match next {
            Ok(Some(inbound)) => inbound,
            Ok(None) => break,
            Err(_) => {
                // idle timeout 与新入站可能同时发生。必须在 state 锁内复查 receiver：
                // enqueue 若先拿锁，消息已可见；worker 若先拿锁，则先移除旧 scope，
                // 后续入站会创建继任 worker，不会把已接收消息遗落在关闭队列中。
                let mut state = inner
                    .state
                    .lock()
                    .expect("OneBot scope dispatcher lock should not be poisoned");
                match receiver.try_recv() {
                    Ok(inbound) => inbound,
                    Err(mpsc::error::TryRecvError::Empty) => {
                        remove_scope_generation(&mut state, &scope_key, generation);
                        debug!(
                            scope_key = %mask_scope_key(&scope_key),
                            generation,
                            "OneBot 11 scope dispatcher reclaimed idle worker"
                        );
                        return;
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                }
            }
        };

        let result = tokio::select! {
            biased;
            _ = inner.shutdown.cancelled() => break,
            result = AssertUnwindSafe(inner.handler.handle(inbound)).catch_unwind() => result,
        };
        if result.is_err() {
            warn!(
                scope_key = %mask_scope_key(&scope_key),
                generation,
                "OneBot 11 scope worker panicked"
            );
            break;
        }
    }

    let mut state = inner
        .state
        .lock()
        .expect("OneBot scope dispatcher lock should not be poisoned");
    remove_scope_generation(&mut state, &scope_key, generation);
}

fn remove_scope_generation(state: &mut DispatcherState, scope_key: &str, generation: u64) {
    if state
        .scopes
        .get(scope_key)
        .is_some_and(|entry| entry.generation == generation)
    {
        state.scopes.remove(scope_key);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use qq_maid_common::{identity_context::IdentitySource, input_part::MessageInputPart};
    use tokio::sync::{Notify, mpsc};

    use super::*;
    use crate::gateway::platform::{Actor, ConversationTarget, Platform};

    struct BlockingHandler {
        started: mpsc::UnboundedSender<String>,
        release: Arc<Notify>,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    #[async_trait]
    impl OneBotInboundHandler for BlockingHandler {
        async fn handle(&self, inbound: InboundMessage) {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.started.send(inbound.message_id).unwrap();
            self.release.notified().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn inbound(message_id: &str, target_id: &str, group: bool) -> InboundMessage {
        InboundMessage {
            platform: Platform::OneBot11,
            account_id: Some("10001".to_owned()),
            conversation: if group {
                ConversationTarget::Group {
                    target_id: target_id.to_owned(),
                }
            } else {
                ConversationTarget::Private {
                    target_id: target_id.to_owned(),
                }
            },
            actor: Actor {
                sender_id: Some(target_id.to_owned()),
                union_id: None,
                display_name: None,
                group_member_role: None,
                is_bot: false,
                source: IdentitySource::Event,
            },
            message_id: message_id.to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: message_id.to_owned(),
            input_parts: vec![MessageInputPart::text(message_id)],
            attachments: Vec::new(),
            quoted: None,
            visible_entity_snapshot: None,
            mentions: Vec::new(),
            mentioned_bot: group,
        }
    }

    fn dispatcher(
        queue_capacity: usize,
        max_active_scopes: usize,
    ) -> (
        OneBotScopeDispatcher,
        mpsc::UnboundedReceiver<String>,
        Arc<Notify>,
        Arc<BlockingHandler>,
        Arc<MessageDedupe>,
    ) {
        let (started_tx, started_rx) = mpsc::unbounded_channel();
        let release = Arc::new(Notify::new());
        let handler = Arc::new(BlockingHandler {
            started: started_tx,
            release: release.clone(),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        });
        let dedupe = Arc::new(MessageDedupe::new(Duration::from_secs(60)));
        let dispatcher = OneBotScopeDispatcher::with_handler(
            queue_capacity,
            max_active_scopes,
            Duration::from_secs(60),
            handler.clone(),
            dedupe.clone(),
            CancellationToken::new(),
        );
        (dispatcher, started_rx, release, handler, dedupe)
    }

    #[tokio::test]
    async fn same_private_and_group_scope_execute_in_fifo_order() {
        for group in [false, true] {
            let (dispatcher, mut started, release, _, _) = dispatcher(2, 2);
            dispatcher.enqueue(inbound("m1", "scope-a", group)).unwrap();
            dispatcher.enqueue(inbound("m2", "scope-a", group)).unwrap();

            assert_eq!(started.recv().await.as_deref(), Some("m1"));
            assert!(
                timeout(Duration::from_millis(30), started.recv())
                    .await
                    .is_err()
            );
            release.notify_one();
            assert_eq!(started.recv().await.as_deref(), Some("m2"));
            release.notify_one();
            dispatcher.shutdown().await;
        }
    }

    #[tokio::test]
    async fn different_scopes_execute_concurrently() {
        let (dispatcher, mut started, release, handler, _) = dispatcher(1, 2);
        dispatcher.enqueue(inbound("m1", "user-a", false)).unwrap();
        dispatcher.enqueue(inbound("m2", "user-b", false)).unwrap();

        let mut messages = [started.recv().await.unwrap(), started.recv().await.unwrap()];
        messages.sort();
        assert_eq!(messages, ["m1", "m2"]);
        assert_eq!(handler.max_active.load(Ordering::SeqCst), 2);
        release.notify_waiters();
        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn queue_and_active_scope_limits_reject_without_committing_dedupe() {
        let (dispatcher, mut started, release, _, dedupe) = dispatcher(1, 1);
        dispatcher.enqueue(inbound("m1", "user-a", false)).unwrap();
        assert_eq!(started.recv().await.as_deref(), Some("m1"));
        dispatcher.enqueue(inbound("m2", "user-a", false)).unwrap();

        assert_eq!(
            dispatcher.enqueue(inbound("m3", "user-a", false)),
            Err(OneBotEnqueueError::ScopeQueueFull)
        );
        assert!(!dedupe.contains_recent_message("m3", Instant::now()));
        assert_eq!(
            dispatcher.enqueue(inbound("m4", "user-b", false)),
            Err(OneBotEnqueueError::ActiveScopeLimit)
        );
        assert!(!dedupe.contains_recent_message("m4", Instant::now()));

        release.notify_waiters();
        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn duplicate_is_accepted_only_once() {
        let (dispatcher, mut started, release, _, _) = dispatcher(1, 1);
        let message = inbound("same", "user-a", false);
        assert_eq!(
            dispatcher.enqueue(message.clone()),
            Ok(OneBotEnqueueOutcome::Accepted)
        );
        assert_eq!(
            dispatcher.enqueue(message),
            Ok(OneBotEnqueueOutcome::Duplicate)
        );
        assert_eq!(started.recv().await.as_deref(), Some("same"));
        assert!(
            timeout(Duration::from_millis(30), started.recv())
                .await
                .is_err()
        );
        release.notify_one();
        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_cancels_active_and_queued_messages_and_rejects_new_input() {
        let (dispatcher, mut started, _release, _, _) = dispatcher(2, 1);
        dispatcher.enqueue(inbound("m1", "user-a", false)).unwrap();
        dispatcher.enqueue(inbound("m2", "user-a", false)).unwrap();
        assert_eq!(started.recv().await.as_deref(), Some("m1"));

        timeout(Duration::from_secs(1), dispatcher.shutdown())
            .await
            .expect("shutdown should cancel tracked workers promptly");
        assert!(started.try_recv().is_err());
        assert_eq!(
            dispatcher.enqueue(inbound("m3", "user-a", false)),
            Err(OneBotEnqueueError::Shutdown)
        );
    }

    #[tokio::test]
    async fn idle_worker_is_reclaimed_and_releases_active_scope_slot() {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        struct ImmediateHandler(mpsc::UnboundedSender<String>);
        #[async_trait]
        impl OneBotInboundHandler for ImmediateHandler {
            async fn handle(&self, inbound: InboundMessage) {
                self.0.send(inbound.message_id).unwrap();
            }
        }
        let dispatcher = OneBotScopeDispatcher::with_handler(
            1,
            1,
            Duration::from_millis(20),
            Arc::new(ImmediateHandler(started_tx)),
            Arc::new(MessageDedupe::new(Duration::from_secs(60))),
            CancellationToken::new(),
        );

        dispatcher.enqueue(inbound("m1", "user-a", false)).unwrap();
        assert_eq!(started_rx.recv().await.as_deref(), Some("m1"));
        tokio::time::sleep(Duration::from_millis(40)).await;
        dispatcher.enqueue(inbound("m2", "user-b", false)).unwrap();
        assert_eq!(started_rx.recv().await.as_deref(), Some("m2"));
        dispatcher.shutdown().await;
    }
}

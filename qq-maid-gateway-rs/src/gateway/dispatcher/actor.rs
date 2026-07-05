//! Dispatcher actor：scope 状态机、入队分发与 worker 生命周期管理。
//!
//! 这里承载原来集中在 `dispatcher/mod.rs` 中的会话级状态机：
//! - 维护每个 scope 的 `ScopeEntry`（Active / Retiring、队列与 backlog）；
//! - 处理 `DispatcherCommand`，决定串行执行、backlog 缓存或容量拒绝；
//! - 在 idle 到期、worker 退出、shutdown flush 之间维护串行化和回收语义。
//!
//! 拆分约束：不改变 QQ 消息入口、去重、队列容量、活跃 worker 上限、idle timeout
//! 的配置语义，也不改 CoreService 调用路径或 `/ping` 本地诊断。`MessageDispatcher`
//! / `MessageDispatcherHandle` 入口仍由 `mod.rs` 暴露。

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::{
    sync::{Semaphore, mpsc},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::super::{
    BotOutboundCache, bot_identity::SharedBotIdentity, dedupe::MessageDedupe,
    group_filter::GroupCooldowns, handle_c2c_message, handle_group_message,
    logging::mask_scope_key, ping::GatewayRuntimeStatus, ref_index::SharedRefIndex,
};
use super::reject::run_reject_worker;
use super::types::{
    DispatcherCommand, DispatcherEnqueueError, DispatcherEnqueueResult, IdleDecision,
    InboundEnvelope, QueuedMessage, REJECT_QUEUE_TEXT, RejectMetrics, RejectNotification,
    RejectTarget, SHUTDOWN_DRAIN_TIMEOUT_SECS, ScopeEntry, ScopeState, WORKER_CANCEL_TIMEOUT_SECS,
    WorkerExitReason,
};
use super::worker::{WorkerContext, run_worker};
use crate::{
    api::QqApiClient, auth::AccessTokenManager, config::AppConfig, respond::RespondClient,
};

pub(super) type HandlerFuture<'a> = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;

/// 统一的入站消息处理抽象，便于在 actor 与 worker 之间解耦，也便于测试注入
/// `RecordingHandler`。真实实现是 `RealMessageHandler`，转发到 gateway 的
/// `handle_c2c_message` / `handle_group_message`。
pub(super) trait MessageHandler: Send + Sync {
    fn handle<'a>(&'a self, message: InboundEnvelope) -> HandlerFuture<'a>;
}

pub(super) struct RealMessageHandler {
    config: AppConfig,
    auth: AccessTokenManager,
    respond: RespondClient,
    api: QqApiClient,
    dedupe: Arc<MessageDedupe>,
    ref_index: SharedRefIndex,
    group_outbound_cache: Arc<std::sync::Mutex<BotOutboundCache>>,
    group_cooldowns: Arc<std::sync::Mutex<GroupCooldowns>>,
    bot_identity: SharedBotIdentity,
    runtime: GatewayRuntimeStatus,
}

impl RealMessageHandler {
    /// 由 `MessageDispatcher::new` 在装配阶段调用，字段保持私有，避免暴露内部结构。
    // 返回 `Arc<dyn MessageHandler>` 而非 `Self`，是为了让 mod.rs 装配时直接得到
    // trait object、避免跨模块再做一次 unsizing 转换。
    #[allow(clippy::new_ret_no_self, clippy::too_many_arguments)]
    pub(super) fn new(
        config: AppConfig,
        auth: AccessTokenManager,
        respond: RespondClient,
        api: QqApiClient,
        dedupe: Arc<MessageDedupe>,
        ref_index: SharedRefIndex,
        group_outbound_cache: Arc<std::sync::Mutex<BotOutboundCache>>,
        group_cooldowns: Arc<std::sync::Mutex<GroupCooldowns>>,
        bot_identity: SharedBotIdentity,
        runtime: GatewayRuntimeStatus,
    ) -> Arc<dyn MessageHandler> {
        Arc::new(Self {
            config,
            auth,
            respond,
            api,
            dedupe,
            ref_index,
            group_outbound_cache,
            group_cooldowns,
            bot_identity,
            runtime,
        })
    }
}

impl MessageHandler for RealMessageHandler {
    fn handle<'a>(&'a self, message: InboundEnvelope) -> HandlerFuture<'a> {
        Box::pin(async move {
            match message {
                InboundEnvelope::C2c(message) => {
                    handle_c2c_message(
                        message,
                        &self.config,
                        &self.auth,
                        &self.respond,
                        &self.api,
                        &self.dedupe,
                        &self.ref_index,
                        &self.runtime,
                    )
                    .await
                }
                InboundEnvelope::Group(message) => {
                    handle_group_message(
                        message,
                        &self.config,
                        &self.respond,
                        &self.api,
                        &self.dedupe,
                        &self.group_outbound_cache,
                        &self.group_cooldowns,
                        &self.bot_identity,
                        &self.runtime,
                        &self.ref_index,
                    )
                    .await
                }
            }
        })
    }
}

pub(super) struct DispatcherActor {
    // 下面四个字段被 dispatcher 内联测试直接窥探（`actor.scopes`、
    // `actor.shutdown_token`、`actor.config`、`actor.worker_slots`），因此标注
    // `pub(super)`，仅对 `gateway::dispatcher` 模块及其子模块可见；gateway 外部
    // 仍然看不到这些字段，对外公开 API 不变。
    pub(super) config: AppConfig,
    api: QqApiClient,
    runtime: GatewayRuntimeStatus,
    command_rx: mpsc::Receiver<DispatcherCommand>,
    command_tx: mpsc::Sender<DispatcherCommand>,
    reject_tx: mpsc::Sender<RejectNotification>,
    reject_rx: mpsc::Receiver<RejectNotification>,
    pub(super) worker_slots: Arc<Semaphore>,
    active_workers: Arc<AtomicU64>,
    reject_metrics: Arc<RejectMetrics>,
    handler: Arc<dyn MessageHandler>,
    pub(super) scopes: HashMap<String, ScopeEntry>,
    pub(super) shutdown_token: CancellationToken,
}

impl DispatcherActor {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        config: AppConfig,
        api: QqApiClient,
        runtime: GatewayRuntimeStatus,
        command_rx: mpsc::Receiver<DispatcherCommand>,
        command_tx: mpsc::Sender<DispatcherCommand>,
        reject_tx: mpsc::Sender<RejectNotification>,
        reject_rx: mpsc::Receiver<RejectNotification>,
        reject_metrics: Arc<RejectMetrics>,
        handler: Arc<dyn MessageHandler>,
        shutdown_token: CancellationToken,
    ) -> Self {
        Self {
            worker_slots: Arc::new(Semaphore::new(config.max_active_conversation_workers)),
            active_workers: Arc::new(AtomicU64::new(0)),
            config,
            api,
            runtime,
            command_rx,
            command_tx,
            reject_tx,
            reject_rx,
            reject_metrics,
            handler,
            scopes: HashMap::new(),
            shutdown_token,
        }
    }

    pub(super) async fn run(mut self) {
        // 启动拒绝通知 worker：把 actor 持有的 reject_rx 移交给 reject_worker，
        // actor 后续只通过 reject_tx 投递拒绝通知，不再亲自处理发送。
        let reject_worker = tokio::spawn(run_reject_worker(
            self.api.clone(),
            self.runtime.clone(),
            std::mem::replace(&mut self.reject_rx, mpsc::channel(1).1),
            self.shutdown_token.child_token(),
        ));

        loop {
            tokio::select! {
                _ = self.shutdown_token.cancelled() => {
                    break;
                }
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    self.handle_command(command).await;
                }
            }
        }

        self.drain_shutdown().await;
        self.shutdown_token.cancel();
        let _ = timeout(
            Duration::from_secs(WORKER_CANCEL_TIMEOUT_SECS + 1),
            reject_worker,
        )
        .await;
    }

    pub(super) async fn handle_command(&mut self, command: DispatcherCommand) {
        match command {
            DispatcherCommand::Enqueue {
                scope_key,
                message,
                ack,
            } => {
                let result = self.enqueue(scope_key, *message).await;
                let _ = ack.send(result);
            }
            DispatcherCommand::WorkerIdleExpired {
                scope_key,
                generation,
                reply,
            } => {
                let _ = reply.send(self.worker_idle_expired(&scope_key, generation));
            }
            DispatcherCommand::WorkerExited {
                scope_key,
                generation,
                reason,
            } => {
                self.worker_exited(scope_key, generation, reason).await;
            }
            DispatcherCommand::WorkerDequeued {
                scope_key,
                generation,
            } => {
                if let Some(entry) = self.scopes.get_mut(&scope_key)
                    && entry.generation == generation
                    && entry.queue_len > 0
                {
                    entry.queue_len -= 1;
                }
            }
        }
    }

    pub(super) async fn enqueue(
        &mut self,
        scope_key: String,
        message: QueuedMessage,
    ) -> DispatcherEnqueueResult {
        if self.shutdown_token.is_cancelled() {
            return Err(DispatcherEnqueueError::Unavailable {
                reason: "dispatcher_shutdown",
            });
        }
        if let Some(entry) = self.scopes.get_mut(&scope_key) {
            let total_len = entry.queue_len + entry.backlog.len();
            if total_len >= self.config.conversation_queue_capacity {
                if message.notify_on_reject
                    && self
                        .reject(scope_key, message.reject_target, "conversation_queue_full")
                        .await
                {
                    return Err(DispatcherEnqueueError::RejectedAndNotified {
                        reason: "conversation_queue_full",
                    });
                }
                return Err(DispatcherEnqueueError::Unavailable {
                    reason: "conversation_queue_full_reject_dropped",
                });
            }
            match entry.state {
                ScopeState::Active => {
                    if let Some(sender) = entry.sender.as_ref() {
                        sender.try_send(message).map_err(|_| {
                            DispatcherEnqueueError::Unavailable {
                                reason: "worker_queue_unavailable",
                            }
                        })?;
                        entry.queue_len += 1;
                        debug!(
                            scope_key = %mask_scope_key(&scope_key),
                            queue_len = entry.queue_len,
                            backlog_len = entry.backlog.len(),
                            "dispatcher enqueued message to active worker"
                        );
                        return Ok(());
                    }
                }
                ScopeState::Retiring => {
                    entry.backlog.push_back(message);
                    debug!(
                        scope_key = %mask_scope_key(&scope_key),
                        queue_len = entry.queue_len,
                        backlog_len = entry.backlog.len(),
                        "dispatcher buffered message in retiring backlog"
                    );
                    return Ok(());
                }
            }
        }

        let permit = match self.worker_slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                if message.notify_on_reject
                    && self
                        .reject(scope_key, message.reject_target, "worker_slot_exhausted")
                        .await
                {
                    return Err(DispatcherEnqueueError::RejectedAndNotified {
                        reason: "worker_slot_exhausted",
                    });
                }
                return Err(DispatcherEnqueueError::Unavailable {
                    reason: "worker_slot_exhausted_reject_dropped",
                });
            }
        };
        let generation = self.next_generation();
        let worker_cancel = self.shutdown_token.child_token();
        let sender =
            self.spawn_worker(scope_key.clone(), generation, worker_cancel.clone(), permit);
        sender
            .try_send(message)
            .map_err(|_| DispatcherEnqueueError::Unavailable {
                reason: "worker_queue_unavailable",
            })?;
        self.scopes.insert(
            scope_key.clone(),
            ScopeEntry {
                state: ScopeState::Active,
                generation,
                sender: Some(sender),
                queue_len: 1,
                backlog: VecDeque::new(),
                worker_cancel,
            },
        );
        info!(
            scope_key = %mask_scope_key(&scope_key),
            generation,
            active_workers = self.active_workers.load(Ordering::Relaxed),
            max_active_workers = self.config.max_active_conversation_workers,
            "dispatcher created worker"
        );
        Ok(())
    }

    pub(super) fn worker_idle_expired(&mut self, scope_key: &str, generation: u64) -> IdleDecision {
        let Some(entry) = self.scopes.get_mut(scope_key) else {
            return IdleDecision::RetireNow;
        };
        if entry.generation != generation
            || entry.state != ScopeState::Active
            || entry.queue_len > 0
        {
            return IdleDecision::StayActive;
        }
        entry.state = ScopeState::Retiring;
        entry.sender = None;
        info!(
            scope_key = %mask_scope_key(scope_key),
            generation,
            backlog_len = entry.backlog.len(),
            "dispatcher marked worker retiring"
        );
        IdleDecision::RetireNow
    }

    pub(super) async fn worker_exited(
        &mut self,
        scope_key: String,
        generation: u64,
        reason: WorkerExitReason,
    ) {
        self.active_workers.fetch_sub(1, Ordering::Relaxed);
        let Some(mut entry) = self.scopes.remove(&scope_key) else {
            return;
        };
        if entry.generation != generation {
            self.scopes.insert(scope_key, entry);
            return;
        }
        match reason {
            WorkerExitReason::Completed => info!(
                scope_key = %mask_scope_key(&scope_key),
                generation,
                "dispatcher observed worker exit"
            ),
            WorkerExitReason::Cancelled => warn!(
                scope_key = %mask_scope_key(&scope_key),
                generation,
                queued_messages = entry.queue_len,
                backlog_len = entry.backlog.len(),
                "dispatcher observed cancelled worker"
            ),
            WorkerExitReason::Panic => warn!(
                scope_key = %mask_scope_key(&scope_key),
                generation,
                queued_messages = entry.queue_len,
                backlog_len = entry.backlog.len(),
                "dispatcher observed panicked worker"
            ),
        }
        if entry.backlog.is_empty() || self.shutdown_token.is_cancelled() {
            return;
        }
        let permit = match self.worker_slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                while let Some(message) = entry.backlog.pop_front() {
                    self.reject(
                        scope_key.clone(),
                        message.reject_target,
                        "worker_slot_exhausted",
                    )
                    .await;
                }
                return;
            }
        };
        let next_generation = self.next_generation();
        let worker_cancel = self.shutdown_token.child_token();
        let sender = self.spawn_worker(
            scope_key.clone(),
            next_generation,
            worker_cancel.clone(),
            permit,
        );
        let mut queue_len = 0usize;
        while let Some(message) = entry.backlog.pop_front() {
            if sender.try_send(message).is_ok() {
                queue_len += 1;
            } else {
                warn!(
                    scope_key = %mask_scope_key(&scope_key),
                    generation = next_generation,
                    "dispatcher successor worker queue unavailable while replaying backlog"
                );
            }
        }
        self.scopes.insert(
            scope_key.clone(),
            ScopeEntry {
                state: ScopeState::Active,
                generation: next_generation,
                sender: Some(sender),
                queue_len,
                backlog: VecDeque::new(),
                worker_cancel,
            },
        );
        info!(
            scope_key = %mask_scope_key(&scope_key),
            generation = next_generation,
            queue_len,
            "dispatcher started successor worker"
        );
    }

    fn spawn_worker(
        &mut self,
        scope_key: String,
        generation: u64,
        worker_cancel: CancellationToken,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> mpsc::Sender<QueuedMessage> {
        let (tx, rx) = mpsc::channel(self.config.conversation_queue_capacity);
        let command_tx = self.command_tx.clone();
        let handler = self.handler.clone();
        let idle_timeout = self.config.conversation_worker_idle_timeout;
        self.active_workers.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            let worker = tokio::spawn(run_worker(WorkerContext {
                scope_key: scope_key.clone(),
                generation,
                handler,
                command_tx: command_tx.clone(),
                rx,
                idle_timeout,
                shutdown_token: worker_cancel.clone(),
            }));
            let reason = match worker.await {
                Ok(reason) => reason,
                Err(error) if error.is_panic() => WorkerExitReason::Panic,
                Err(_) => WorkerExitReason::Cancelled,
            };
            drop(permit);
            let _ = command_tx
                .send(DispatcherCommand::WorkerExited {
                    scope_key,
                    generation,
                    reason,
                })
                .await;
        });
        tx
    }

    async fn reject(
        &mut self,
        scope_key: String,
        target: RejectTarget,
        reason: &'static str,
    ) -> bool {
        self.reject_metrics.total.fetch_add(1, Ordering::Relaxed);
        let notification = RejectNotification {
            scope_key: scope_key.clone(),
            target,
            message: REJECT_QUEUE_TEXT.to_owned(),
        };
        if self.reject_tx.try_send(notification).is_err() {
            let reject_total = self.reject_metrics.total.load(Ordering::Relaxed);
            let reject_dropped = self.reject_metrics.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            warn!(
                scope_key = %mask_scope_key(&scope_key),
                reject_total,
                reject_dropped,
                reason,
                "dispatcher reject queue full"
            );
            return false;
        }
        true
    }

    async fn drain_shutdown(&mut self) {
        let start = Instant::now();
        for entry in self.scopes.values() {
            entry.worker_cancel.cancel();
        }
        while !self.scopes.is_empty()
            && start.elapsed() < Duration::from_secs(SHUTDOWN_DRAIN_TIMEOUT_SECS)
        {
            if let Ok(Some(command)) =
                timeout(Duration::from_millis(100), self.command_rx.recv()).await
            {
                self.handle_command(command).await;
            }
        }
        let remaining_scopes = self.scopes.len();
        if remaining_scopes > 0 {
            warn!(
                remaining_scopes,
                active_workers = self.active_workers.load(Ordering::Relaxed),
                reject_total = self.reject_metrics.total.load(Ordering::Relaxed),
                reject_dropped = self.reject_metrics.dropped.load(Ordering::Relaxed),
                "dispatcher shutdown drained with remaining work"
            );
        } else {
            info!(
                reject_total = self.reject_metrics.total.load(Ordering::Relaxed),
                reject_dropped = self.reject_metrics.dropped.load(Ordering::Relaxed),
                "dispatcher shutdown completed"
            );
        }
    }

    pub(super) fn next_generation(&self) -> u64 {
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
        NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
    }
}

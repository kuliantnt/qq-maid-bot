//! Gateway 会话级消息调度器。
//!
//! 该模块把 QQ 入站消息从 WebSocket 读循环中解耦出来：同一 scope 串行、不同 scope 并发，
//! 并通过有界 command channel / worker queue / reject channel 避免无界积压。

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

use anyhow::anyhow;
use tokio::{
    sync::{Semaphore, mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{
    BotOutboundCache, ReplyCache,
    dedupe::MessageDedupe,
    event::{C2cMessage, GroupMessage},
    group_filter::GroupCooldowns,
    handle_c2c_message, handle_group_message,
    logging::{mask_identifier, mask_scope_key},
    outbound::{send_c2c_text_with_status, send_group_text_with_status},
    ping::GatewayRuntimeStatus,
};
use crate::{
    api::QqApiClient,
    auth::AccessTokenManager,
    config::AppConfig,
    respond::{RespondClient, scope_key_from_c2c_message, scope_key_from_group_message},
};

const REJECT_QUEUE_TEXT: &str = "当前消息较多，请稍后再试。";
const SHUTDOWN_DRAIN_TIMEOUT_SECS: u64 = 10;
const WORKER_CANCEL_TIMEOUT_SECS: u64 = 1;
const COMMAND_CHANNEL_MULTIPLIER: usize = 4;

#[derive(Clone)]
pub(super) struct MessageDispatcherHandle {
    command_tx: mpsc::Sender<DispatcherCommand>,
    reject_tx: mpsc::Sender<RejectNotification>,
    reject_metrics: Arc<RejectMetrics>,
}

impl MessageDispatcherHandle {
    pub(super) async fn enqueue_c2c(&self, message: C2cMessage) -> anyhow::Result<()> {
        let scope_key = scope_key_from_c2c_message(&message);
        let target = RejectTarget::C2c {
            user_openid: message.user_openid.clone(),
            message_id: message.message_id.clone(),
        };
        self.enqueue(InboundEnvelope::C2c(message), scope_key, target)
            .await
    }

    pub(super) async fn enqueue_group(&self, message: GroupMessage) -> anyhow::Result<()> {
        let scope_key = scope_key_from_group_message(&message);
        let target = RejectTarget::Group {
            group_openid: message.group_openid.clone(),
            message_id: message.message_id.clone(),
        };
        self.enqueue(InboundEnvelope::Group(message), scope_key, target)
            .await
    }

    async fn enqueue(
        &self,
        envelope: InboundEnvelope,
        scope_key: String,
        reject_target: RejectTarget,
    ) -> anyhow::Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        let command = DispatcherCommand::Enqueue {
            scope_key,
            // command channel 只负责把消息转交给调度 actor；
            // 这里装箱是为了避免控制命令枚举被大消息体撑大，触发 Clippy large_enum_variant。
            message: Box::new(QueuedMessage {
                envelope,
                reject_target,
            }),
            ack: ack_tx,
        };
        if let Err(error) = self.command_tx.try_send(command) {
            match error {
                mpsc::error::TrySendError::Full(DispatcherCommand::Enqueue {
                    scope_key,
                    message,
                    ..
                }) => {
                    self.reject_from_handle(
                        scope_key,
                        message.reject_target,
                        "dispatcher_command_queue_full",
                    );
                    return Err(anyhow!("dispatcher command queue full"));
                }
                mpsc::error::TrySendError::Full(_) => {
                    return Err(anyhow!("dispatcher command queue full"));
                }
                mpsc::error::TrySendError::Closed(_) => return Err(anyhow!("dispatcher closed")),
            }
        }
        match ack_rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow!("dispatcher unavailable")),
        }
    }

    fn reject_from_handle(&self, scope_key: String, target: RejectTarget, reason: &'static str) {
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
        }
    }
}

pub(super) struct MessageDispatcher {
    handle: MessageDispatcherHandle,
    join_handle: JoinHandle<()>,
    shutdown_token: CancellationToken,
}

impl MessageDispatcher {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        config: AppConfig,
        auth: AccessTokenManager,
        respond: RespondClient,
        api: QqApiClient,
        dedupe: Arc<MessageDedupe>,
        reply_cache: ReplyCache,
        group_outbound_cache: Arc<std::sync::Mutex<BotOutboundCache>>,
        group_cooldowns: Arc<std::sync::Mutex<GroupCooldowns>>,
        runtime: GatewayRuntimeStatus,
        shutdown_token: CancellationToken,
    ) -> Self {
        let command_capacity = config
            .max_active_conversation_workers
            .saturating_mul(COMMAND_CHANNEL_MULTIPLIER)
            .max(8);
        let (command_tx, command_rx) = mpsc::channel(command_capacity);
        let reject_capacity = config.max_active_conversation_workers.max(1);
        let (reject_tx, reject_rx) = mpsc::channel(reject_capacity);
        let handle_reject_tx = reject_tx.clone();
        let reject_metrics = Arc::new(RejectMetrics::default());
        let handler = Arc::new(RealMessageHandler {
            config: config.clone(),
            auth,
            respond,
            api: api.clone(),
            dedupe,
            reply_cache,
            group_outbound_cache,
            group_cooldowns,
            runtime: runtime.clone(),
        });
        let actor = DispatcherActor::new(
            config,
            api,
            runtime,
            command_rx,
            command_tx.clone(),
            reject_tx,
            reject_rx,
            reject_metrics.clone(),
            handler,
            shutdown_token.clone(),
        );
        let join_handle = tokio::spawn(actor.run());
        Self {
            handle: MessageDispatcherHandle {
                command_tx,
                reject_tx: handle_reject_tx,
                reject_metrics,
            },
            join_handle,
            shutdown_token,
        }
    }

    pub(super) fn handle(&self) -> MessageDispatcherHandle {
        self.handle.clone()
    }

    pub(super) async fn shutdown(self) {
        self.shutdown_token.cancel();
        match timeout(
            Duration::from_secs(SHUTDOWN_DRAIN_TIMEOUT_SECS + WORKER_CANCEL_TIMEOUT_SECS + 1),
            self.join_handle,
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(error = %error, "dispatcher task ended unexpectedly"),
            Err(_) => warn!("dispatcher shutdown timed out"),
        }
    }
}

#[derive(Debug)]
enum DispatcherCommand {
    Enqueue {
        scope_key: String,
        // QueuedMessage 可能携带完整平台消息，装箱后可避免 command 枚举整体尺寸过大。
        message: Box<QueuedMessage>,
        ack: oneshot::Sender<anyhow::Result<()>>,
    },
    WorkerIdleExpired {
        scope_key: String,
        generation: u64,
        reply: oneshot::Sender<IdleDecision>,
    },
    WorkerExited {
        scope_key: String,
        generation: u64,
        reason: WorkerExitReason,
    },
    WorkerDequeued {
        scope_key: String,
        generation: u64,
    },
}

#[derive(Debug, Clone)]
enum InboundEnvelope {
    C2c(C2cMessage),
    Group(GroupMessage),
}

#[derive(Debug, Clone)]
struct QueuedMessage {
    envelope: InboundEnvelope,
    reject_target: RejectTarget,
}

#[derive(Debug, Clone)]
enum RejectTarget {
    C2c {
        user_openid: String,
        message_id: String,
    },
    Group {
        group_openid: String,
        message_id: String,
    },
}

#[derive(Debug)]
struct RejectNotification {
    scope_key: String,
    target: RejectTarget,
    message: String,
}

#[derive(Debug, Default)]
struct RejectMetrics {
    total: AtomicU64,
    dropped: AtomicU64,
}

#[derive(Debug, PartialEq, Eq)]
enum IdleDecision {
    StayActive,
    RetireNow,
}

#[derive(Debug)]
enum WorkerExitReason {
    Completed,
    Cancelled,
    Panic,
}

struct DispatcherActor {
    config: AppConfig,
    api: QqApiClient,
    runtime: GatewayRuntimeStatus,
    command_rx: mpsc::Receiver<DispatcherCommand>,
    command_tx: mpsc::Sender<DispatcherCommand>,
    reject_tx: mpsc::Sender<RejectNotification>,
    reject_rx: mpsc::Receiver<RejectNotification>,
    worker_slots: Arc<Semaphore>,
    active_workers: Arc<AtomicU64>,
    reject_metrics: Arc<RejectMetrics>,
    handler: Arc<dyn MessageHandler>,
    scopes: HashMap<String, ScopeEntry>,
    shutdown_token: CancellationToken,
}

struct ScopeEntry {
    state: ScopeState,
    generation: u64,
    sender: Option<mpsc::Sender<QueuedMessage>>,
    queue_len: usize,
    backlog: VecDeque<QueuedMessage>,
    worker_cancel: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeState {
    Active,
    Retiring,
}

type HandlerFuture<'a> = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;

trait MessageHandler: Send + Sync {
    fn handle<'a>(&'a self, message: InboundEnvelope) -> HandlerFuture<'a>;
}

struct RealMessageHandler {
    config: AppConfig,
    auth: AccessTokenManager,
    respond: RespondClient,
    api: QqApiClient,
    dedupe: Arc<MessageDedupe>,
    reply_cache: ReplyCache,
    group_outbound_cache: Arc<std::sync::Mutex<BotOutboundCache>>,
    group_cooldowns: Arc<std::sync::Mutex<GroupCooldowns>>,
    runtime: GatewayRuntimeStatus,
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
                        &self.reply_cache,
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
                        &self.runtime,
                    )
                    .await
                }
            }
        })
    }
}

impl DispatcherActor {
    #[allow(clippy::too_many_arguments)]
    fn new(
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

    async fn run(mut self) {
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

    async fn handle_command(&mut self, command: DispatcherCommand) {
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

    async fn enqueue(&mut self, scope_key: String, message: QueuedMessage) -> anyhow::Result<()> {
        if self.shutdown_token.is_cancelled() {
            return Err(anyhow!("dispatcher shutdown"));
        }
        if let Some(entry) = self.scopes.get_mut(&scope_key) {
            let total_len = entry.queue_len + entry.backlog.len();
            if total_len >= self.config.conversation_queue_capacity {
                self.reject(scope_key, message.reject_target, "conversation_queue_full")
                    .await;
                return Err(anyhow!("conversation queue full"));
            }
            match entry.state {
                ScopeState::Active => {
                    if let Some(sender) = entry.sender.as_ref() {
                        sender
                            .try_send(message)
                            .map_err(|_| anyhow!("worker queue unavailable"))?;
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
                self.reject(scope_key, message.reject_target, "worker_slot_exhausted")
                    .await;
                return Err(anyhow!("worker slot exhausted"));
            }
        };
        let generation = self.next_generation();
        let worker_cancel = self.shutdown_token.child_token();
        let sender =
            self.spawn_worker(scope_key.clone(), generation, worker_cancel.clone(), permit);
        sender
            .try_send(message)
            .map_err(|_| anyhow!("worker queue unavailable"))?;
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

    fn worker_idle_expired(&mut self, scope_key: &str, generation: u64) -> IdleDecision {
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

    async fn worker_exited(
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

    async fn reject(&mut self, scope_key: String, target: RejectTarget, reason: &'static str) {
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
        }
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

    fn next_generation(&self) -> u64 {
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
        NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
    }
}

struct WorkerContext {
    scope_key: String,
    generation: u64,
    handler: Arc<dyn MessageHandler>,
    command_tx: mpsc::Sender<DispatcherCommand>,
    rx: mpsc::Receiver<QueuedMessage>,
    idle_timeout: Duration,
    shutdown_token: CancellationToken,
}

async fn run_worker(mut ctx: WorkerContext) -> WorkerExitReason {
    loop {
        let next = tokio::select! {
            _ = ctx.shutdown_token.cancelled() => {
                let dropped_messages = ctx.rx.len();
                if dropped_messages > 0 {
                    warn!(
                        scope_key = %mask_scope_key(&ctx.scope_key),
                        generation = ctx.generation,
                        dropped_messages,
                        "dispatcher worker cancelled with queued messages"
                    );
                }
                return WorkerExitReason::Cancelled;
            }
            result = timeout(ctx.idle_timeout, ctx.rx.recv()) => result,
        };
        let message = match next {
            Ok(Some(message)) => message,
            Ok(None) => return WorkerExitReason::Completed,
            Err(_) => {
                let (reply_tx, reply_rx) = oneshot::channel();
                if ctx
                    .command_tx
                    .send(DispatcherCommand::WorkerIdleExpired {
                        scope_key: ctx.scope_key.clone(),
                        generation: ctx.generation,
                        reply: reply_tx,
                    })
                    .await
                    .is_err()
                {
                    return WorkerExitReason::Cancelled;
                }
                match reply_rx.await {
                    Ok(IdleDecision::StayActive) => continue,
                    Ok(IdleDecision::RetireNow) => return WorkerExitReason::Completed,
                    Err(_) => return WorkerExitReason::Cancelled,
                }
            }
        };
        if ctx
            .command_tx
            .send(DispatcherCommand::WorkerDequeued {
                scope_key: ctx.scope_key.clone(),
                generation: ctx.generation,
            })
            .await
            .is_err()
        {
            warn!(
                scope_key = %mask_scope_key(&ctx.scope_key),
                generation = ctx.generation,
                queued_messages = ctx.rx.len(),
                "dispatcher worker dequeued message but command channel is closed"
            );
            return WorkerExitReason::Cancelled;
        }
        let result = ctx.handler.handle(message.envelope).await;
        if let Err(error) = result {
            warn!(
                scope_key = %mask_scope_key(&ctx.scope_key),
                generation = ctx.generation,
                error = %error,
                "dispatcher worker failed to handle message"
            );
        } else {
            debug!(
                scope_key = %mask_scope_key(&ctx.scope_key),
                generation = ctx.generation,
                "dispatcher worker handled message"
            );
        }
    }
}

async fn run_reject_worker(
    api: QqApiClient,
    runtime: GatewayRuntimeStatus,
    mut reject_rx: mpsc::Receiver<RejectNotification>,
    shutdown_token: CancellationToken,
) {
    loop {
        let notification = tokio::select! {
            _ = shutdown_token.cancelled() => break,
            item = reject_rx.recv() => item,
        };
        let Some(notification) = notification else {
            break;
        };
        let masked_target = match &notification.target {
            RejectTarget::C2c { user_openid, .. } => mask_identifier(user_openid),
            RejectTarget::Group { group_openid, .. } => mask_identifier(group_openid),
        };
        let result = match notification.target {
            RejectTarget::C2c {
                user_openid,
                message_id,
            } => {
                send_c2c_text_with_status(
                    &api,
                    &runtime,
                    &user_openid,
                    Some(&message_id),
                    &notification.message,
                )
                .await
            }
            RejectTarget::Group {
                group_openid,
                message_id,
            } => {
                send_group_text_with_status(
                    &api,
                    &runtime,
                    &group_openid,
                    Some(&message_id),
                    &notification.message,
                )
                .await
            }
        };
        if let Err(error) = result {
            warn!(
                scope_key = %mask_scope_key(&notification.scope_key),
                target = %masked_target,
                error = %error.log_summary(),
                "dispatcher reject notification send failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
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
        }
    }

    fn c2c(id: &str, user: &str) -> C2cMessage {
        C2cMessage {
            message_id: id.to_owned(),
            user_openid: user.to_owned(),
            content: "hello".to_owned(),
            reply: None,
            timestamp: None,
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
        let (mut actor, mut command_rx, _) =
            test_actor_with_handler(test_config(), handler.clone());
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
        let (mut actor, mut command_rx, _) =
            test_actor_with_handler(test_config(), handler.clone());

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
        let handler = Arc::new(RecordingHandler {
            block: true,
            ..RecordingHandler::default()
        });
        let (mut actor, mut command_rx, _) =
            test_actor_with_handler(test_config(), handler.clone());
        let scope = "private:user-a".to_owned();

        actor
            .enqueue(scope.clone(), queued_c2c("m1", "user-a"))
            .await
            .unwrap();
        drain_worker_commands(&mut actor, &mut command_rx, 1).await;
        assert_eq!(
            actor.worker_idle_expired(&scope, 0),
            IdleDecision::StayActive
        );
        actor
            .enqueue(scope.clone(), queued_c2c("m2", "user-a"))
            .await
            .unwrap();

        assert_eq!(handler.max_for_scope(&scope), 1);
        handler.release_all();
        wait_for_events(&handler, 4).await;
        drain_worker_commands(&mut actor, &mut command_rx, 1).await;
    }

    #[tokio::test]
    async fn retiring_backlog_replays_in_original_order() {
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
        let (mut actor, _command_rx, mut reject_rx) =
            test_actor_with_handler(test_config(), handler);
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
    async fn command_queue_full_attempts_busy_reject() {
        let (command_tx, _command_rx) = mpsc::channel(1);
        let (reject_tx, mut reject_rx) = mpsc::channel(1);
        let metrics = Arc::new(RejectMetrics::default());
        let handle = MessageDispatcherHandle {
            command_tx: command_tx.clone(),
            reject_tx,
            reject_metrics: metrics.clone(),
        };
        command_tx
            .try_send(DispatcherCommand::WorkerDequeued {
                scope_key: "occupied".to_owned(),
                generation: 1,
            })
            .unwrap();

        let err = handle
            .enqueue(
                InboundEnvelope::C2c(c2c("m1", "user-a")),
                "private:user-a".to_owned(),
                RejectTarget::C2c {
                    user_openid: "user-a".to_owned(),
                    message_id: "m1".to_owned(),
                },
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("dispatcher command queue full"));
        let notification = reject_rx.recv().await.unwrap();
        assert_eq!(notification.scope_key, "private:user-a");
        assert_eq!(notification.message, REJECT_QUEUE_TEXT);
        match notification.target {
            RejectTarget::C2c { message_id, .. } => assert_eq!(message_id, "m1"),
            RejectTarget::Group { .. } => panic!("expected c2c reject target"),
        }
        assert_eq!(metrics.total.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.dropped.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn reject_queue_full_does_not_block_and_counts_drop() {
        let (command_tx, _command_rx) = mpsc::channel(1);
        let (reject_tx, _reject_rx) = mpsc::channel(1);
        let metrics = Arc::new(RejectMetrics::default());
        let handle = MessageDispatcherHandle {
            command_tx: command_tx.clone(),
            reject_tx,
            reject_metrics: metrics.clone(),
        };
        command_tx
            .try_send(DispatcherCommand::WorkerDequeued {
                scope_key: "occupied".to_owned(),
                generation: 1,
            })
            .unwrap();
        handle
            .reject_tx
            .try_send(RejectNotification {
                scope_key: "full".to_owned(),
                target: RejectTarget::C2c {
                    user_openid: "user".to_owned(),
                    message_id: "held".to_owned(),
                },
                message: REJECT_QUEUE_TEXT.to_owned(),
            })
            .unwrap();

        let err = timeout(
            Duration::from_millis(100),
            handle.enqueue(
                InboundEnvelope::C2c(c2c("m1", "user-a")),
                "private:user-a".to_owned(),
                RejectTarget::C2c {
                    user_openid: "user-a".to_owned(),
                    message_id: "m1".to_owned(),
                },
            ),
        )
        .await
        .unwrap()
        .unwrap_err();

        assert!(err.to_string().contains("dispatcher command queue full"));
        assert_eq!(metrics.total.load(Ordering::Relaxed), 1);
        assert_eq!(metrics.dropped.load(Ordering::Relaxed), 1);
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

        assert!(err.to_string().contains("dispatcher shutdown"));
        assert!(actor.scopes.is_empty());
    }

    #[tokio::test]
    async fn group_reject_target_keeps_own_message_id() {
        let handler = Arc::new(RecordingHandler::default());
        let (mut actor, _command_rx, mut reject_rx) =
            test_actor_with_handler(test_config(), handler);
        let scope = "group:group-a".to_owned();
        let generation = actor.next_generation();
        actor.scopes.insert(
            scope.clone(),
            ScopeEntry {
                state: ScopeState::Retiring,
                generation,
                sender: None,
                queue_len: 0,
                backlog: VecDeque::from([
                    queued_group("g1", "group-a"),
                    queued_group("g2", "group-a"),
                ]),
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
            (
                RejectTarget::Group { message_id: a, .. },
                RejectTarget::Group { message_id: b, .. },
            ) => assert_eq!((a, b), ("g1".to_owned(), "g2".to_owned())),
            _ => panic!("expected group reject targets"),
        }
    }
}

//! Gateway 会话级消息调度器。
//!
//! 该模块把 QQ 入站消息从 WebSocket 读循环中解耦出来：同一 scope 串行、不同 scope 并发，
//! 并通过有界 command channel / worker queue / reject channel 避免无界积压。

use std::{
    collections::{HashMap, VecDeque},
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
        self.command_tx
            .try_send(DispatcherCommand::Enqueue {
                scope_key,
                envelope,
                reject_target,
                ack: ack_tx,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => anyhow!("dispatcher command queue full"),
                mpsc::error::TrySendError::Closed(_) => anyhow!("dispatcher closed"),
            })?;
        match ack_rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow!("dispatcher unavailable")),
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
        let actor = DispatcherActor::new(
            config,
            auth,
            respond,
            api,
            dedupe,
            reply_cache,
            group_outbound_cache,
            group_cooldowns,
            runtime,
            command_rx,
            command_tx.clone(),
            reject_tx,
            reject_rx,
            shutdown_token.clone(),
        );
        let join_handle = tokio::spawn(actor.run());
        Self {
            handle: MessageDispatcherHandle { command_tx },
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
        envelope: InboundEnvelope,
        reject_target: RejectTarget,
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

impl InboundEnvelope {}

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

#[derive(Debug)]
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
    auth: AccessTokenManager,
    respond: RespondClient,
    api: QqApiClient,
    dedupe: Arc<MessageDedupe>,
    reply_cache: ReplyCache,
    group_outbound_cache: Arc<std::sync::Mutex<BotOutboundCache>>,
    group_cooldowns: Arc<std::sync::Mutex<GroupCooldowns>>,
    runtime: GatewayRuntimeStatus,
    command_rx: mpsc::Receiver<DispatcherCommand>,
    command_tx: mpsc::Sender<DispatcherCommand>,
    reject_tx: mpsc::Sender<RejectNotification>,
    reject_rx: mpsc::Receiver<RejectNotification>,
    worker_slots: Arc<Semaphore>,
    active_workers: Arc<AtomicU64>,
    reject_total: u64,
    reject_dropped: u64,
    scopes: HashMap<String, ScopeEntry>,
    shutdown_token: CancellationToken,
}

struct ScopeEntry {
    state: ScopeState,
    generation: u64,
    sender: Option<mpsc::Sender<InboundEnvelope>>,
    queue_len: usize,
    backlog: VecDeque<InboundEnvelope>,
    reject_target: RejectTarget,
    worker_cancel: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeState {
    Active,
    Retiring,
}

impl DispatcherActor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        config: AppConfig,
        auth: AccessTokenManager,
        respond: RespondClient,
        api: QqApiClient,
        dedupe: Arc<MessageDedupe>,
        reply_cache: ReplyCache,
        group_outbound_cache: Arc<std::sync::Mutex<BotOutboundCache>>,
        group_cooldowns: Arc<std::sync::Mutex<GroupCooldowns>>,
        runtime: GatewayRuntimeStatus,
        command_rx: mpsc::Receiver<DispatcherCommand>,
        command_tx: mpsc::Sender<DispatcherCommand>,
        reject_tx: mpsc::Sender<RejectNotification>,
        reject_rx: mpsc::Receiver<RejectNotification>,
        shutdown_token: CancellationToken,
    ) -> Self {
        Self {
            worker_slots: Arc::new(Semaphore::new(config.max_active_conversation_workers)),
            active_workers: Arc::new(AtomicU64::new(0)),
            config,
            auth,
            respond,
            api,
            dedupe,
            reply_cache,
            group_outbound_cache,
            group_cooldowns,
            runtime,
            command_rx,
            command_tx,
            reject_tx,
            reject_rx,
            reject_total: 0,
            reject_dropped: 0,
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
                envelope,
                reject_target,
                ack,
            } => {
                let result = self.enqueue(scope_key, envelope, reject_target).await;
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

    async fn enqueue(
        &mut self,
        scope_key: String,
        envelope: InboundEnvelope,
        reject_target: RejectTarget,
    ) -> anyhow::Result<()> {
        if self.shutdown_token.is_cancelled() {
            return Err(anyhow!("dispatcher shutdown"));
        }
        if let Some(entry) = self.scopes.get_mut(&scope_key) {
            let total_len = entry.queue_len + entry.backlog.len();
            if total_len >= self.config.conversation_queue_capacity {
                self.reject(scope_key, reject_target, "conversation_queue_full")
                    .await;
                return Err(anyhow!("conversation queue full"));
            }
            entry.reject_target = reject_target.clone();
            match entry.state {
                ScopeState::Active => {
                    if let Some(sender) = entry.sender.as_ref() {
                        sender
                            .try_send(envelope)
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
                    entry.backlog.push_back(envelope);
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
                self.reject(scope_key, reject_target, "worker_slot_exhausted")
                    .await;
                return Err(anyhow!("worker slot exhausted"));
            }
        };
        let generation = self.next_generation();
        let worker_cancel = self.shutdown_token.child_token();
        let sender =
            self.spawn_worker(scope_key.clone(), generation, worker_cancel.clone(), permit);
        sender
            .try_send(envelope)
            .map_err(|_| anyhow!("worker queue unavailable"))?;
        self.scopes.insert(
            scope_key.clone(),
            ScopeEntry {
                state: ScopeState::Active,
                generation,
                sender: Some(sender),
                queue_len: 1,
                backlog: VecDeque::new(),
                reject_target,
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
                "dispatcher observed cancelled worker"
            ),
            WorkerExitReason::Panic => warn!(
                scope_key = %mask_scope_key(&scope_key),
                generation,
                "dispatcher observed panicked worker"
            ),
        }
        if entry.backlog.is_empty() || self.shutdown_token.is_cancelled() {
            return;
        }
        let permit = match self.worker_slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let reject_target = entry.reject_target.clone();
                while entry.backlog.pop_front().is_some() {
                    self.reject(
                        scope_key.clone(),
                        reject_target.clone(),
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
        while let Some(envelope) = entry.backlog.pop_front() {
            if sender.try_send(envelope).is_ok() {
                queue_len += 1;
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
                reject_target: entry.reject_target,
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
    ) -> mpsc::Sender<InboundEnvelope> {
        let (tx, rx) = mpsc::channel(self.config.conversation_queue_capacity);
        let command_tx = self.command_tx.clone();
        let config = self.config.clone();
        let auth = self.auth.clone();
        let respond = self.respond.clone();
        let api = self.api.clone();
        let dedupe = self.dedupe.clone();
        let reply_cache = self.reply_cache.clone();
        let group_outbound_cache = self.group_outbound_cache.clone();
        let group_cooldowns = self.group_cooldowns.clone();
        let runtime = self.runtime.clone();
        let idle_timeout = self.config.conversation_worker_idle_timeout;
        self.active_workers.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            let worker = tokio::spawn(run_worker(WorkerContext {
                scope_key: scope_key.clone(),
                generation,
                config,
                auth,
                respond,
                api,
                dedupe,
                reply_cache,
                group_outbound_cache,
                group_cooldowns,
                runtime,
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
        self.reject_total += 1;
        let notification = RejectNotification {
            scope_key: scope_key.clone(),
            target,
            message: REJECT_QUEUE_TEXT.to_owned(),
        };
        if self.reject_tx.try_send(notification).is_err() {
            self.reject_dropped += 1;
            warn!(
                scope_key = %mask_scope_key(&scope_key),
                reject_total = self.reject_total,
                reject_dropped = self.reject_dropped,
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
                reject_total = self.reject_total,
                reject_dropped = self.reject_dropped,
                "dispatcher shutdown drained with remaining work"
            );
        } else {
            info!(
                reject_total = self.reject_total,
                reject_dropped = self.reject_dropped,
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
    config: AppConfig,
    auth: AccessTokenManager,
    respond: RespondClient,
    api: QqApiClient,
    dedupe: Arc<MessageDedupe>,
    reply_cache: ReplyCache,
    group_outbound_cache: Arc<std::sync::Mutex<BotOutboundCache>>,
    group_cooldowns: Arc<std::sync::Mutex<GroupCooldowns>>,
    runtime: GatewayRuntimeStatus,
    command_tx: mpsc::Sender<DispatcherCommand>,
    rx: mpsc::Receiver<InboundEnvelope>,
    idle_timeout: Duration,
    shutdown_token: CancellationToken,
}

async fn run_worker(mut ctx: WorkerContext) -> WorkerExitReason {
    loop {
        let next = tokio::select! {
            _ = ctx.shutdown_token.cancelled() => return WorkerExitReason::Cancelled,
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
            return WorkerExitReason::Cancelled;
        }
        let result = match message {
            InboundEnvelope::C2c(message) => {
                handle_c2c_message(
                    message,
                    &ctx.config,
                    &ctx.auth,
                    &ctx.respond,
                    &ctx.api,
                    &ctx.dedupe,
                    &ctx.reply_cache,
                    &ctx.runtime,
                )
                .await
            }
            InboundEnvelope::Group(message) => {
                handle_group_message(
                    message,
                    &ctx.config,
                    &ctx.respond,
                    &ctx.api,
                    &ctx.dedupe,
                    &ctx.group_outbound_cache,
                    &ctx.group_cooldowns,
                    &ctx.runtime,
                )
                .await
            }
        };
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

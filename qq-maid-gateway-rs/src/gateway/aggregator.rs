//! Gateway 入站消息聚合器。
//!
//! 聚合发生在 Dispatcher 之前：等待用户短暂停止输入时不占用 scope worker、
//! worker slot 或 LLM permit。命令和 pending 分类通过 Core 的轻量接口完成，
//! Gateway 只处理平台字段、/ping 本地命令和附件等自身边界。

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};

use anyhow::anyhow;
use async_trait::async_trait;
use qq_maid_core::service::CoreInboundKind;
use tokio::{
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::{Instant, sleep_until, timeout},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{
    ReplyCache,
    dedupe::{Duplicate, MessageDedupe, MessageReservation, dedupe_event_key, dedupe_message_key},
    dispatcher::MessageDispatcherHandle,
    event::{C2cMessage, GroupMessage},
    logging::mask_scope_key,
    ping::is_ping_command,
    resolve_signals,
};
use crate::{
    config::{AppConfig, MessageAggregationConfig},
    respond::{RespondClient, build_respond_content, scope_key_from_c2c_message},
};

const AGGREGATOR_SHUTDOWN_TIMEOUT_SECS: u64 = 30;

#[async_trait]
pub(super) trait AggregationDispatcher: Send + Sync {
    async fn enqueue_c2c(&self, message: C2cMessage) -> anyhow::Result<()>;

    async fn enqueue_c2c_with_processed_ack(
        &self,
        message: C2cMessage,
        processed_ack: oneshot::Sender<()>,
    ) -> anyhow::Result<()>;

    async fn enqueue_group(&self, message: GroupMessage) -> anyhow::Result<()>;
}

#[async_trait]
impl AggregationDispatcher for MessageDispatcherHandle {
    async fn enqueue_c2c(&self, message: C2cMessage) -> anyhow::Result<()> {
        MessageDispatcherHandle::enqueue_c2c(self, message).await
    }

    async fn enqueue_c2c_with_processed_ack(
        &self,
        message: C2cMessage,
        processed_ack: oneshot::Sender<()>,
    ) -> anyhow::Result<()> {
        MessageDispatcherHandle::enqueue_c2c_with_processed_ack(self, message, processed_ack).await
    }

    async fn enqueue_group(&self, message: GroupMessage) -> anyhow::Result<()> {
        MessageDispatcherHandle::enqueue_group(self, message).await
    }
}

#[derive(Clone)]
pub(super) struct MessageAggregatorHandle {
    command_tx: mpsc::Sender<AggregatorCommand>,
}

impl MessageAggregatorHandle {
    pub(super) async fn enqueue_c2c(&self, message: C2cMessage) -> anyhow::Result<()> {
        let (ack, reply) = oneshot::channel();
        self.command_tx
            .send(AggregatorCommand::EnqueueC2c {
                message: Box::new(message),
                ack,
            })
            .await
            .map_err(|_| anyhow!("message aggregator closed"))?;
        reply
            .await
            .map_err(|_| anyhow!("message aggregator unavailable"))?
    }

    pub(super) async fn enqueue_group(&self, message: GroupMessage) -> anyhow::Result<()> {
        let (ack, reply) = oneshot::channel();
        self.command_tx
            .send(AggregatorCommand::EnqueueGroup {
                message: Box::new(message),
                ack,
            })
            .await
            .map_err(|_| anyhow!("message aggregator closed"))?;
        reply
            .await
            .map_err(|_| anyhow!("message aggregator unavailable"))?
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let (ack, reply) = oneshot::channel();
        self.command_tx
            .send(AggregatorCommand::Shutdown { ack })
            .await
            .map_err(|_| anyhow!("message aggregator closed"))?;
        reply
            .await
            .map_err(|_| anyhow!("message aggregator unavailable"))?
    }

    #[cfg(test)]
    async fn debug_barrier_state(&self) -> BarrierDebugState {
        let (ack, reply) = oneshot::channel();
        self.command_tx
            .send(AggregatorCommand::DebugBarrierState { ack })
            .await
            .expect("message aggregator should be available");
        reply
            .await
            .expect("message aggregator debug state should be returned")
    }
}

pub(super) struct MessageAggregator {
    handle: MessageAggregatorHandle,
    join_handle: JoinHandle<()>,
    shutdown_token: CancellationToken,
}

impl MessageAggregator {
    pub(super) fn new(
        config: AppConfig,
        respond: RespondClient,
        dispatcher: MessageDispatcherHandle,
        dedupe: Arc<MessageDedupe>,
        reply_cache: ReplyCache,
        shutdown_token: CancellationToken,
    ) -> Self {
        Self::new_with_dispatcher(
            config,
            respond,
            Arc::new(dispatcher),
            dedupe,
            reply_cache,
            shutdown_token,
        )
    }

    fn new_with_dispatcher(
        config: AppConfig,
        respond: RespondClient,
        dispatcher: Arc<dyn AggregationDispatcher>,
        dedupe: Arc<MessageDedupe>,
        reply_cache: ReplyCache,
        shutdown_token: CancellationToken,
    ) -> Self {
        let capacity = config
            .message_aggregation
            .max_active_keys
            .saturating_mul(2)
            .max(8);
        let (command_tx, command_rx) = mpsc::channel(capacity);
        let actor = AggregatorActor {
            config: config.message_aggregation.clone(),
            bot_instance: config.app_id,
            respond,
            dispatcher,
            dedupe,
            reply_cache,
            command_rx,
            command_tx: command_tx.clone(),
            batches: HashMap::new(),
            barriers: HashMap::new(),
            next_barrier_token: 1,
            barrier_tasks: JoinSet::new(),
            shutdown_token: shutdown_token.clone(),
        };
        let join_handle = tokio::spawn(actor.run());
        Self {
            handle: MessageAggregatorHandle { command_tx },
            join_handle,
            shutdown_token,
        }
    }

    pub(super) fn handle(&self) -> MessageAggregatorHandle {
        self.handle.clone()
    }

    pub(super) async fn shutdown(mut self) {
        let graceful = timeout(
            Duration::from_secs(AGGREGATOR_SHUTDOWN_TIMEOUT_SECS),
            self.handle.shutdown(),
        )
        .await;
        match graceful {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(error = %error, "message aggregator shutdown command failed"),
            Err(_) => warn!("message aggregator shutdown command timed out"),
        }

        // 这里取消的是 actor 实际监听的同一个 token；若 join 仍超时则 abort，避免遗留 detached task。
        self.shutdown_token.cancel();
        let join = &mut self.join_handle;
        match timeout(Duration::from_secs(1), join).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if error.is_cancelled() => {}
            Ok(Err(error)) => warn!(error = %error, "message aggregator task ended unexpectedly"),
            Err(_) => {
                self.join_handle.abort();
                match self.join_handle.await {
                    Ok(()) => {}
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => {
                        warn!(error = %error, "message aggregator aborted task ended unexpectedly")
                    }
                }
            }
        }
    }
}

enum AggregatorCommand {
    EnqueueC2c {
        message: Box<C2cMessage>,
        ack: oneshot::Sender<anyhow::Result<()>>,
    },
    EnqueueGroup {
        message: Box<GroupMessage>,
        ack: oneshot::Sender<anyhow::Result<()>>,
    },
    Timer {
        key: AggregationKey,
        generation: u64,
    },
    Shutdown {
        ack: oneshot::Sender<anyhow::Result<()>>,
    },
    #[cfg(test)]
    DebugBarrierState {
        ack: oneshot::Sender<BarrierDebugState>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AggregationKey {
    bot_instance: String,
    platform: &'static str,
    chat_type: &'static str,
    conversation_id: String,
    sender_id: String,
}

struct PendingAggregation {
    first_received_at: Instant,
    last_received_at: Instant,
    quiet_deadline: Instant,
    hard_deadline: Instant,
    generation: u64,
    messages: Vec<C2cMessage>,
    message_ids: HashSet<String>,
    event_ids: HashSet<String>,
    reservations: Vec<MessageReservation>,
    total_chars: usize,
}

#[derive(Debug, Clone, Copy)]
enum FlushReason {
    QuietTimeout,
    MaxWait,
    MaxMessages,
    MaxChars,
    Barrier,
    Shutdown,
}

impl FlushReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::QuietTimeout => "quiet_timeout",
            Self::MaxWait => "max_wait",
            Self::MaxMessages => "max_messages",
            Self::MaxChars => "max_chars",
            Self::Barrier => "barrier",
            Self::Shutdown => "shutdown",
        }
    }
}

enum AggregationDecision {
    Aggregate,
    Immediate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarrierStatus {
    Completed,
    Closed,
    Cancelled,
}

#[derive(Debug)]
struct BarrierEvent {
    key: AggregationKey,
    token: u64,
    status: BarrierStatus,
}

#[derive(Debug)]
struct BarrierEntry {
    token: u64,
    resolved: Option<BarrierStatus>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BarrierDebugState {
    barrier_count: usize,
    task_count: usize,
}

struct AggregatorActor {
    config: MessageAggregationConfig,
    bot_instance: String,
    respond: RespondClient,
    dispatcher: Arc<dyn AggregationDispatcher>,
    dedupe: Arc<MessageDedupe>,
    reply_cache: ReplyCache,
    command_rx: mpsc::Receiver<AggregatorCommand>,
    command_tx: mpsc::Sender<AggregatorCommand>,
    batches: HashMap<AggregationKey, PendingAggregation>,
    barriers: HashMap<AggregationKey, VecDeque<BarrierEntry>>,
    next_barrier_token: u64,
    barrier_tasks: JoinSet<BarrierEvent>,
    shutdown_token: CancellationToken,
}

impl AggregatorActor {
    async fn run(mut self) {
        loop {
            tokio::select! {
                _ = self.shutdown_token.cancelled() => break,
                event = self.barrier_tasks.join_next(), if !self.barrier_tasks.is_empty() => {
                    self.handle_barrier_join_result(event).await;
                }
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    if self.handle_command(command).await {
                        self.shutdown_barrier_tasks().await;
                        return;
                    }
                }
            }
        }
        self.flush_all(FlushReason::Shutdown).await;
        self.shutdown_barrier_tasks().await;
    }

    async fn handle_command(&mut self, command: AggregatorCommand) -> bool {
        match command {
            AggregatorCommand::EnqueueC2c { message, ack } => {
                let result = self.handle_c2c(*message).await;
                let _ = ack.send(result);
                false
            }
            AggregatorCommand::EnqueueGroup { message, ack } => {
                let result = self.dispatcher.enqueue_group(*message).await;
                let _ = ack.send(result);
                false
            }
            AggregatorCommand::Timer { key, generation } => {
                self.handle_timer(key, generation).await;
                false
            }
            AggregatorCommand::Shutdown { ack } => {
                self.command_rx.close();
                self.drain_closed_commands().await;
                self.flush_all(FlushReason::Shutdown).await;
                let _ = ack.send(Ok(()));
                true
            }
            #[cfg(test)]
            AggregatorCommand::DebugBarrierState { ack } => {
                let _ = ack.send(self.barrier_debug_state());
                false
            }
        }
    }

    async fn drain_closed_commands(&mut self) {
        while let Ok(command) = self.command_rx.try_recv() {
            match command {
                AggregatorCommand::EnqueueC2c { message, ack } => {
                    let result = self.handle_c2c(*message).await;
                    let _ = ack.send(result);
                }
                AggregatorCommand::EnqueueGroup { message, ack } => {
                    let result = self.dispatcher.enqueue_group(*message).await;
                    let _ = ack.send(result);
                }
                AggregatorCommand::Timer { key, generation } => {
                    self.handle_timer(key, generation).await;
                }
                AggregatorCommand::Shutdown { ack } => {
                    let _ = ack.send(Ok(()));
                }
                #[cfg(test)]
                AggregatorCommand::DebugBarrierState { ack } => {
                    let _ = ack.send(self.barrier_debug_state());
                }
            }
        }
    }

    async fn handle_c2c(&mut self, mut message: C2cMessage) -> anyhow::Result<()> {
        let key = self.key_for(&message);
        // C2C 去重在物理消息进入聚合/立即调度前只做 reservation；
        // 只有成功转交 Dispatcher 后才 commit，失败路径依靠 token 化 rollback 允许平台重试。
        let reservation = match self.reserve_c2c_message(&message) {
            Ok(reservation) => reservation,
            Err(_) => {
                debug!(
                    scope_key = %mask_scope_key(&scope_key_from_c2c_message(&message)),
                    message_id = %message.message_id,
                    "duplicate C2C message ignored before aggregation dispatch"
                );
                return Ok(());
            }
        };
        resolve_signals(&mut message, &self.reply_cache);
        self.drain_ready_barrier_events().await;

        if self.has_active_barrier(&key) {
            self.flush_key(&key, FlushReason::Barrier).await?;
            return self
                .dispatch_with_barrier(key, message, vec![reservation], "active_barrier")
                .await;
        }

        match self.classify(&message).await {
            AggregationDecision::Immediate => {
                self.flush_key(&key, FlushReason::Barrier).await?;
                self.dispatch_with_barrier(key, message, vec![reservation], "immediate")
                    .await
            }
            AggregationDecision::Aggregate => self.aggregate(key, message, reservation).await,
        }
    }

    async fn classify(&self, message: &C2cMessage) -> AggregationDecision {
        if !self.config.private_enabled
            || message.content.trim().is_empty()
            || !message.attachments.is_empty()
            || message.reply.is_some()
            || is_ping_command(&message.content)
        {
            return AggregationDecision::Immediate;
        }
        let content = build_respond_content(message);
        match self.respond.classify_c2c(message, content).await {
            Ok(classification) if classification.kind == CoreInboundKind::NormalChat => {
                AggregationDecision::Aggregate
            }
            Ok(_) => AggregationDecision::Immediate,
            Err(error) => {
                warn!(
                    scope_key = %mask_scope_key(&scope_key_from_c2c_message(message)),
                    message_id = %message.message_id,
                    error = %error.log_summary(),
                    "message aggregation classification failed; dispatching immediately"
                );
                AggregationDecision::Immediate
            }
        }
    }

    async fn aggregate(
        &mut self,
        key: AggregationKey,
        message: C2cMessage,
        reservation: MessageReservation,
    ) -> anyhow::Result<()> {
        if self.is_duplicate_for_open_batch(&key, &message) {
            debug!(
                scope_key = %mask_scope_key(&scope_key_from_c2c_message(&message)),
                message_id = %message.message_id,
                "duplicate C2C message ignored by aggregation batch"
            );
            return Ok(());
        }

        let message_chars = message.content.chars().count();
        if message_chars > self.config.max_chars {
            self.flush_key(&key, FlushReason::Barrier).await?;
            return self
                .dispatch_without_barrier(&key, message, vec![reservation], "oversized_message")
                .await;
        }

        if !self.batches.contains_key(&key) && self.batches.len() >= self.config.max_active_keys {
            warn!(
                scope_key = %mask_scope_key(&scope_key_from_c2c_message(&message)),
                active_keys = self.batches.len(),
                max_active_keys = self.config.max_active_keys,
                "message aggregation active key limit reached; dispatching immediately"
            );
            return self
                .dispatch_without_barrier(&key, message, vec![reservation], "active_key_limit")
                .await;
        }

        if let Some(batch) = self.batches.get(&key) {
            let projected_count = batch.messages.len() + 1;
            let projected_chars = batch.total_chars + message_chars;
            if projected_count > self.config.max_messages || projected_chars > self.config.max_chars
            {
                let reason = if projected_count > self.config.max_messages {
                    FlushReason::MaxMessages
                } else {
                    FlushReason::MaxChars
                };
                self.flush_key(&key, reason).await?;
            }
        }

        let now = Instant::now();
        let mut flush_reason = None;
        let generation = if let Some(batch) = self.batches.get_mut(&key) {
            append_to_batch(
                batch,
                message,
                reservation,
                message_chars,
                now,
                &self.config,
            );
            if batch.messages.len() == self.config.max_messages {
                flush_reason = Some(FlushReason::MaxMessages);
            } else if batch.total_chars == self.config.max_chars {
                flush_reason = Some(FlushReason::MaxChars);
            }
            batch.generation
        } else {
            let mut batch = PendingAggregation {
                first_received_at: now,
                last_received_at: now,
                quiet_deadline: now + self.config.quiet,
                hard_deadline: now + self.config.max_wait,
                generation: 1,
                messages: Vec::new(),
                message_ids: HashSet::new(),
                event_ids: HashSet::new(),
                reservations: Vec::new(),
                total_chars: 0,
            };
            append_to_batch(
                &mut batch,
                message,
                reservation,
                message_chars,
                now,
                &self.config,
            );
            if batch.messages.len() == self.config.max_messages {
                flush_reason = Some(FlushReason::MaxMessages);
            } else if batch.total_chars == self.config.max_chars {
                flush_reason = Some(FlushReason::MaxChars);
            }
            let generation = batch.generation;
            self.batches.insert(key.clone(), batch);
            generation
        };

        if let Some(reason) = flush_reason {
            self.flush_key(&key, reason).await?;
        } else {
            self.spawn_timer(key, generation);
        }
        Ok(())
    }

    async fn dispatch_with_barrier(
        &mut self,
        key: AggregationKey,
        message: C2cMessage,
        reservations: Vec<MessageReservation>,
        stage: &'static str,
    ) -> anyhow::Result<()> {
        let batch_size = reservations.len();
        let (processed_tx, processed_rx) = oneshot::channel();
        if let Err(error) = self
            .dispatcher
            .enqueue_c2c_with_processed_ack(message, processed_tx)
            .await
        {
            rollback_reservations(reservations);
            warn!(
                scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                error = %error,
                stage,
                aggregation_batch_size = batch_size,
                reservation_released = true,
                "message aggregation immediate dispatch failed; rolled back reservation"
            );
            return Err(error);
        }
        commit_reservations(reservations);
        let token = self.next_barrier_token;
        self.next_barrier_token = self.next_barrier_token.saturating_add(1);
        self.barriers
            .entry(key.clone())
            .or_default()
            .push_back(BarrierEntry {
                token,
                resolved: None,
            });
        self.spawn_barrier_task(key, token, processed_rx);
        Ok(())
    }

    async fn dispatch_without_barrier(
        &self,
        key: &AggregationKey,
        message: C2cMessage,
        reservations: Vec<MessageReservation>,
        stage: &'static str,
    ) -> anyhow::Result<()> {
        let batch_size = reservations.len();
        if let Err(error) = self.dispatcher.enqueue_c2c(message).await {
            rollback_reservations(reservations);
            warn!(
                scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                error = %error,
                stage,
                aggregation_batch_size = batch_size,
                reservation_released = true,
                "message aggregation immediate dispatch failed; rolled back reservation"
            );
            return Err(error);
        }
        commit_reservations(reservations);
        Ok(())
    }

    fn reserve_c2c_message(&self, message: &C2cMessage) -> Result<MessageReservation, Duplicate> {
        self.dedupe
            .reserve_many(dedupe_keys(message), std::time::Instant::now())
    }

    fn has_active_barrier(&self, key: &AggregationKey) -> bool {
        self.barriers
            .get(key)
            .is_some_and(|queue| !queue.is_empty())
    }

    fn is_duplicate_for_open_batch(&self, key: &AggregationKey, message: &C2cMessage) -> bool {
        let Some(batch) = self.batches.get(key) else {
            return false;
        };
        message_id_values(message)
            .iter()
            .any(|id| batch.message_ids.contains(id))
            || event_id_values(message)
                .iter()
                .any(|id| batch.event_ids.contains(id))
    }

    async fn handle_timer(&mut self, key: AggregationKey, generation: u64) {
        let Some(batch) = self.batches.get(&key) else {
            return;
        };
        if batch.generation != generation {
            return;
        }
        let now = Instant::now();
        let reason = if now >= batch.hard_deadline {
            FlushReason::MaxWait
        } else if now >= batch.quiet_deadline {
            FlushReason::QuietTimeout
        } else {
            self.spawn_timer(key, generation);
            return;
        };
        if let Err(error) = self.flush_key(&key, reason).await {
            warn!(error = %error, "message aggregation timer flush failed");
        }
    }

    async fn flush_key(&mut self, key: &AggregationKey, reason: FlushReason) -> anyhow::Result<()> {
        let Some(mut batch) = self.batches.remove(key) else {
            return Ok(());
        };
        let batch_size = batch.messages.len();
        let message = merge_batch(&batch, reason);
        if let Err(error) = self.dispatcher.enqueue_c2c(message).await {
            if matches!(reason, FlushReason::Shutdown) {
                let reservations = std::mem::take(&mut batch.reservations);
                rollback_reservations(reservations);
                warn!(
                    scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                    error = %error,
                    stage = reason.as_str(),
                    aggregation_batch_size = batch_size,
                    reservation_released = true,
                    batch_restored = false,
                    "message aggregation shutdown flush failed; rolled back batch reservations"
                );
            } else {
                self.restore_failed_batch(key.clone(), batch, reason, &error);
            }
            return Err(error);
        }
        commit_reservations(batch.reservations);
        Ok(())
    }

    fn restore_failed_batch(
        &mut self,
        key: AggregationKey,
        mut batch: PendingAggregation,
        reason: FlushReason,
        error: &anyhow::Error,
    ) {
        // 非 shutdown flush 失败时保留原批次和 reservation 所有权，等待下一次 timer 或边界触发重试。
        // 不释放 reservation 可避免同一物理事件在恢复窗口内被平台重试重复并入新批次。
        let now = Instant::now();
        let batch_size = batch.messages.len();
        batch.last_received_at = now;
        batch.quiet_deadline = now + self.config.quiet;
        batch.hard_deadline = batch.quiet_deadline;
        batch.generation = batch.generation.saturating_add(1);
        let generation = batch.generation;
        self.batches.insert(key.clone(), batch);
        self.spawn_timer(key.clone(), generation);
        warn!(
            scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
            error = %error,
            stage = reason.as_str(),
            aggregation_batch_size = batch_size,
            reservation_released = false,
            batch_restored = true,
            "message aggregation flush failed; restored batch for retry"
        );
    }

    async fn flush_all(&mut self, reason: FlushReason) {
        let keys = self.batches.keys().cloned().collect::<Vec<_>>();
        let mut failed = 0usize;
        for key in keys {
            if let Err(error) = self.flush_key(&key, reason).await {
                failed += 1;
                warn!(
                    scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                    error = %error,
                    remaining_failed_batches = failed,
                    "message aggregation shutdown flush failed"
                );
            }
        }
        if failed > 0 || !self.batches.is_empty() {
            warn!(
                failed_batches = failed,
                remaining_batches = self.batches.len(),
                "message aggregation shutdown left unsubmitted batches"
            );
        }
    }

    fn spawn_timer(&self, key: AggregationKey, generation: u64) {
        let Some(batch) = self.batches.get(&key) else {
            return;
        };
        let deadline = batch.quiet_deadline.min(batch.hard_deadline);
        let command_tx = self.command_tx.clone();
        let shutdown_token = self.shutdown_token.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown_token.cancelled() => {}
                _ = sleep_until(deadline) => {
                    let _ = command_tx
                        .send(AggregatorCommand::Timer { key, generation })
                        .await;
                }
            }
        });
    }

    fn spawn_barrier_task(
        &mut self,
        key: AggregationKey,
        token: u64,
        processed_rx: oneshot::Receiver<()>,
    ) {
        let shutdown_token = self.shutdown_token.clone();
        self.barrier_tasks.spawn(async move {
            let status = tokio::select! {
                _ = shutdown_token.cancelled() => BarrierStatus::Cancelled,
                result = processed_rx => match result {
                    Ok(()) => BarrierStatus::Completed,
                    Err(_) => BarrierStatus::Closed,
                },
            };
            BarrierEvent { key, token, status }
        });
    }

    async fn handle_barrier_join_result(
        &mut self,
        result: Option<Result<BarrierEvent, tokio::task::JoinError>>,
    ) {
        match result {
            Some(Ok(event)) => self.handle_barrier_event(event),
            Some(Err(error)) if error.is_cancelled() => {}
            Some(Err(error)) => warn!(error = %error, "message aggregation barrier task failed"),
            None => {}
        }
    }

    async fn drain_ready_barrier_events(&mut self) {
        while let Some(result) = self.barrier_tasks.try_join_next() {
            self.handle_barrier_join_result(Some(result)).await;
        }
    }

    fn handle_barrier_event(&mut self, event: BarrierEvent) {
        if event.status == BarrierStatus::Cancelled {
            return;
        }
        let Some(queue) = self.barriers.get_mut(&event.key) else {
            debug!(
                barrier_token = event.token,
                barrier_status = ?event.status,
                "message aggregation ignored stale barrier event"
            );
            return;
        };
        let Some(entry) = queue.iter_mut().find(|entry| entry.token == event.token) else {
            debug!(
                barrier_token = event.token,
                barrier_status = ?event.status,
                "message aggregation ignored unknown barrier token"
            );
            return;
        };
        entry.resolved = Some(event.status);
        if event.status == BarrierStatus::Closed {
            warn!(
                scope_key = %mask_scope_key(&format!("private:{}", event.key.conversation_id)),
                barrier_token = event.token,
                "message aggregation barrier processed ack closed; releasing scope barrier"
            );
        } else {
            debug!(
                scope_key = %mask_scope_key(&format!("private:{}", event.key.conversation_id)),
                barrier_token = event.token,
                "message aggregation barrier resolved"
            );
        }
        self.release_resolved_barriers(&event.key);
    }

    fn release_resolved_barriers(&mut self, key: &AggregationKey) {
        let Some(queue) = self.barriers.get_mut(key) else {
            return;
        };
        while queue.front().is_some_and(|entry| entry.resolved.is_some()) {
            queue.pop_front();
        }
        if queue.is_empty() {
            self.barriers.remove(key);
        }
    }

    async fn shutdown_barrier_tasks(&mut self) {
        self.shutdown_token.cancel();
        while let Some(result) = self.barrier_tasks.join_next().await {
            self.handle_barrier_join_result(Some(result)).await;
        }
        self.barriers.clear();
    }

    #[cfg(test)]
    fn barrier_debug_state(&self) -> BarrierDebugState {
        BarrierDebugState {
            barrier_count: self.barriers.values().map(VecDeque::len).sum(),
            task_count: self.barrier_tasks.len(),
        }
    }

    fn key_for(&self, message: &C2cMessage) -> AggregationKey {
        AggregationKey {
            bot_instance: self.bot_instance.clone(),
            platform: "qq_official",
            chat_type: "private",
            conversation_id: message.user_openid.clone(),
            sender_id: message.user_openid.clone(),
        }
    }
}

fn append_to_batch(
    batch: &mut PendingAggregation,
    message: C2cMessage,
    reservation: MessageReservation,
    message_chars: usize,
    now: Instant,
    config: &MessageAggregationConfig,
) {
    for id in message_id_values(&message) {
        batch.message_ids.insert(id);
    }
    for id in event_id_values(&message) {
        batch.event_ids.insert(id);
    }
    batch.messages.push(message);
    batch.reservations.push(reservation);
    batch.total_chars += message_chars;
    batch.last_received_at = now;
    batch.quiet_deadline = (now + config.quiet).min(batch.hard_deadline);
    batch.generation = batch.generation.saturating_add(1);
}

fn message_id_values(message: &C2cMessage) -> Vec<String> {
    let mut ids = message
        .source_message_ids
        .iter()
        .filter(|id| !id.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if !message.message_id.trim().is_empty() && !ids.iter().any(|id| id == &message.message_id) {
        ids.push(message.message_id.clone());
    }
    ids
}

fn event_id_values(message: &C2cMessage) -> Vec<String> {
    let mut ids = message
        .source_event_ids
        .iter()
        .filter(|id| !id.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if let Some(event_id) = message.event_id.as_ref().filter(|id| !id.trim().is_empty())
        && !ids.iter().any(|id| id == event_id)
    {
        ids.push(event_id.clone());
    }
    ids
}

fn dedupe_keys(message: &C2cMessage) -> Vec<String> {
    message_id_values(message)
        .into_iter()
        .map(|id| dedupe_message_key(&id))
        .chain(
            event_id_values(message)
                .into_iter()
                .map(|id| dedupe_event_key(&id)),
        )
        .collect()
}

fn commit_reservations(reservations: Vec<MessageReservation>) {
    for reservation in reservations {
        reservation.commit();
    }
}

fn rollback_reservations(reservations: Vec<MessageReservation>) {
    for reservation in reservations {
        reservation.rollback();
    }
}

fn merge_batch(batch: &PendingAggregation, reason: FlushReason) -> C2cMessage {
    let all_messages = batch.messages.clone();
    let mut merged = all_messages
        .last()
        .cloned()
        .expect("aggregation batch should never be empty when flushed");
    merged.content = all_messages
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    merged.source_message_ids = all_messages.iter().flat_map(message_id_values).collect();
    merged.source_event_ids = all_messages.iter().flat_map(event_id_values).collect();
    merged.first_message_timestamp = all_messages.first().and_then(|message| {
        message
            .first_message_timestamp
            .clone()
            .or_else(|| message.timestamp.clone())
    });
    merged.last_message_timestamp = all_messages.last().and_then(|message| {
        message
            .last_message_timestamp
            .clone()
            .or_else(|| message.timestamp.clone())
    });
    merged.timestamp = merged.last_message_timestamp.clone();
    info!(
        scope_key = %mask_scope_key(&scope_key_from_c2c_message(&merged)),
        aggregation_batch_size = all_messages.len(),
        aggregation_total_chars = batch.total_chars,
        aggregation_wait_ms = batch.first_received_at.elapsed().as_millis() as u64,
        aggregation_flush_reason = reason.as_str(),
        "message aggregation flushed batch"
    );
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GroupMessageMode;
    use qq_maid_core::service::{
        CoreActor, CoreConversation, CoreError, CoreHealthSnapshot, CoreInboundClassification,
        CoreRequest, CoreRespondOutput, CoreResponse, CoreService, Platform,
        UpstreamStatusSnapshot,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::time::{advance, pause};

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
        pending_acks: Mutex<VecDeque<(C2cMessage, oneshot::Sender<()>)>>,
        closed: AtomicBool,
        fail_next_enqueues: AtomicUsize,
    }

    #[async_trait]
    impl AggregationDispatcher for MockDispatcher {
        async fn enqueue_c2c(&self, message: C2cMessage) -> anyhow::Result<()> {
            if self.should_fail_enqueue() {
                return Err(anyhow!("dispatcher injected failure"));
            }
            if self.closed.load(Ordering::Relaxed) {
                return Err(anyhow!("dispatcher closed"));
            }
            self.c2c.lock().unwrap().push(message);
            Ok(())
        }

        async fn enqueue_c2c_with_processed_ack(
            &self,
            message: C2cMessage,
            processed_ack: oneshot::Sender<()>,
        ) -> anyhow::Result<()> {
            if self.should_fail_enqueue() {
                return Err(anyhow!("dispatcher injected failure"));
            }
            if self.closed.load(Ordering::Relaxed) {
                return Err(anyhow!("dispatcher closed"));
            }
            self.c2c.lock().unwrap().push(message.clone());
            self.pending_acks
                .lock()
                .unwrap()
                .push_back((message, processed_ack));
            Ok(())
        }

        async fn enqueue_group(&self, _message: GroupMessage) -> anyhow::Result<()> {
            Ok(())
        }
    }

    impl MockDispatcher {
        fn should_fail_enqueue(&self) -> bool {
            self.fail_next_enqueues
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    value.checked_sub(1)
                })
                .is_ok()
        }

        fn fail_next(&self, count: usize) {
            self.fail_next_enqueues.store(count, Ordering::Relaxed);
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
        }
    }

    fn harness_with_config(config: AppConfig) -> Harness {
        let core = Arc::new(MockCore::default());
        let dispatcher = Arc::new(MockDispatcher {
            core: core.clone(),
            ..MockDispatcher::default()
        });
        let dedupe = Arc::new(MessageDedupe::new(Duration::from_secs(60)));
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
    async fn quiet_timeout_flush_failure_restores_batch_for_retry() {
        pause();
        let h = harness();
        let handle = h.aggregator.handle();
        enqueue(&handle, c2c("1", "u1", "hello")).await;
        h.dispatcher.fail_next(1);
        advance(Duration::from_millis(101)).await;
        yield_actor().await;
        assert!(h.dispatcher.messages().is_empty());

        advance(Duration::from_millis(101)).await;
        wait_for_messages(&h.dispatcher, 1).await;
        assert_eq!(h.dispatcher.messages()[0].content, "hello");
        h.aggregator.shutdown().await;
    }

    #[tokio::test]
    async fn max_messages_flush_failure_restores_batch_for_retry() {
        pause();
        let h = harness();
        let handle = h.aggregator.handle();
        h.dispatcher.fail_next(1);
        enqueue(&handle, c2c("1", "u1", "a")).await;
        enqueue(&handle, c2c("2", "u1", "b")).await;
        assert!(handle.enqueue_c2c(c2c("3", "u1", "c")).await.is_err());
        yield_actor().await;
        assert!(h.dispatcher.messages().is_empty());

        advance(Duration::from_millis(101)).await;
        wait_for_messages(&h.dispatcher, 1).await;
        assert_eq!(h.dispatcher.messages()[0].content, "a\nb\nc");
        h.aggregator.shutdown().await;
    }

    #[tokio::test]
    async fn max_chars_flush_failure_restores_batch_for_retry() {
        pause();
        let h = harness();
        let handle = h.aggregator.handle();
        h.dispatcher.fail_next(1);
        enqueue(&handle, c2c("1", "u1", "123456")).await;
        assert!(handle.enqueue_c2c(c2c("2", "u1", "123456")).await.is_err());
        yield_actor().await;
        assert!(h.dispatcher.messages().is_empty());

        advance(Duration::from_millis(101)).await;
        wait_for_messages(&h.dispatcher, 1).await;
        assert_eq!(h.dispatcher.messages()[0].content, "123456\n123456");
        h.aggregator.shutdown().await;
    }

    #[tokio::test]
    async fn boundary_message_does_not_cross_failed_old_batch_flush() {
        pause();
        let h = harness();
        let handle = h.aggregator.handle();
        enqueue(&handle, c2c("1", "u1", "123456")).await;
        h.dispatcher.fail_next(1);
        assert!(handle.enqueue_c2c(c2c("2", "u1", "1234567")).await.is_err());
        assert!(h.dispatcher.messages().is_empty());

        advance(Duration::from_millis(101)).await;
        wait_for_messages(&h.dispatcher, 1).await;
        assert_eq!(h.dispatcher.messages()[0].content, "123456");
        enqueue(&handle, c2c("2", "u1", "1234567")).await;
        advance(Duration::from_millis(101)).await;
        wait_for_messages(&h.dispatcher, 2).await;
        assert_eq!(h.dispatcher.messages()[1].content, "1234567");
        h.aggregator.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_flush_failure_rolls_back_reservations_without_detached_retry() {
        pause();
        let h = harness();
        let handle = h.aggregator.handle();
        enqueue(&handle, c2c("1", "u1", "hello")).await;
        h.dispatcher.fail_next(1);
        h.aggregator.shutdown().await;

        assert!(h.dispatcher.messages().is_empty());
        assert!(!h.dedupe.contains_recent("1"));
        assert!(
            !h.dedupe
                .contains_recent_event("event-1", std::time::Instant::now())
        );
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
            },
            conversation: CoreConversation::Private {
                peer_id: "u1".to_owned(),
            },
        };
        assert_eq!(request.scope_key(), "private:u1");
    }
}

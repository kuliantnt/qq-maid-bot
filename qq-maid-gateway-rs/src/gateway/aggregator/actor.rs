use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use anyhow::anyhow;
use qq_maid_core::service::CoreInboundKind;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
    time::{Instant, sleep_until},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{
    batch::{
        append_to_batch, commit_reservations, dedupe_keys, event_id_values, merge_batch,
        message_id_values, rollback_reservations,
    },
    handle::AggregationDispatcher,
    types::{
        AggregateError, AggregationDecision, AggregationKey, AggregatorCommand, BarrierEntry,
        BarrierEvent, BarrierStatus, DeferredC2cMessage, DeferredProcessError, DispatchFailure,
        FlushReason, PendingAggregation,
    },
};
use crate::{
    config::MessageAggregationConfig,
    gateway::{
        ReplyCache,
        dedupe::{Duplicate, MessageDedupe, MessageReservation},
        event::C2cMessage,
        logging::mask_scope_key,
        ping::is_ping_command,
        resolve_signals,
    },
    respond::{RespondClient, build_respond_content, scope_key_from_c2c_message},
};

#[cfg(test)]
use super::types::BarrierDebugState;

pub(super) struct AggregatorActor {
    pub(super) config: MessageAggregationConfig,
    pub(super) bot_instance: String,
    pub(super) respond: RespondClient,
    pub(super) dispatcher: Arc<dyn AggregationDispatcher>,
    pub(super) dedupe: Arc<MessageDedupe>,
    pub(super) reply_cache: ReplyCache,
    pub(super) command_rx: mpsc::Receiver<AggregatorCommand>,
    pub(super) command_tx: mpsc::Sender<AggregatorCommand>,
    pub(super) batches: HashMap<AggregationKey, PendingAggregation>,
    pub(super) barriers: HashMap<AggregationKey, VecDeque<BarrierEntry>>,
    pub(super) deferred: HashMap<AggregationKey, VecDeque<DeferredC2cMessage>>,
    pub(super) deferred_capacity_per_key: usize,
    pub(super) deferred_capacity_total: usize,
    pub(super) deferred_total_len: usize,
    pub(super) deferred_retry_generations: HashMap<AggregationKey, u64>,
    pub(super) next_barrier_token: u64,
    pub(super) barrier_tasks: JoinSet<BarrierEvent>,
    pub(super) shutdown_token: CancellationToken,
    pub(super) shutting_down: bool,
}

impl AggregatorActor {
    pub(super) async fn run(mut self) {
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
        self.shutting_down = true;
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
            AggregatorCommand::DeferredRetry { key, generation } => {
                self.handle_deferred_retry(key, generation).await;
                false
            }
            AggregatorCommand::Shutdown { ack } => {
                self.shutting_down = true;
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
            #[cfg(test)]
            AggregatorCommand::DebugInjectBarrier { message, ack } => {
                self.debug_inject_barrier_for_message(&message);
                let _ = ack.send(());
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
                AggregatorCommand::DeferredRetry { key, generation } => {
                    self.handle_deferred_retry(key, generation).await;
                }
                AggregatorCommand::Shutdown { ack } => {
                    let _ = ack.send(Ok(()));
                }
                #[cfg(test)]
                AggregatorCommand::DebugBarrierState { ack } => {
                    let _ = ack.send(self.barrier_debug_state());
                }
                #[cfg(test)]
                AggregatorCommand::DebugInjectBarrier { message, ack } => {
                    self.debug_inject_barrier_for_message(&message);
                    let _ = ack.send(());
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
        if self.deferred.contains_key(&key) {
            self.defer_c2c(
                key.clone(),
                DeferredC2cMessage {
                    message,
                    reservation,
                },
            )?;
            if let Err(error) = self.drain_deferred_for_key(&key).await {
                warn!(error = %error, "message aggregation deferred retry failed");
            }
            return Ok(());
        }
        match self
            .process_reserved_c2c(key.clone(), message, reservation, false)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if !self.shutting_down => {
                if let Some(deferred) = error.deferred {
                    self.defer_c2c(key, deferred)?;
                    Ok(())
                } else {
                    Err(error.error)
                }
            }
            Err(error) => {
                if let Some(deferred) = error.deferred {
                    deferred.reservation.rollback();
                }
                Err(error.error)
            }
        }
    }

    async fn process_reserved_c2c(
        &mut self,
        key: AggregationKey,
        message: C2cMessage,
        reservation: MessageReservation,
        retain_on_dispatch_failure: bool,
    ) -> Result<(), DeferredProcessError> {
        if self.has_active_barrier(&key) {
            if let Err(error) = self.flush_key(&key, FlushReason::Barrier).await {
                return Err(DeferredProcessError::blocked(message, reservation, error));
            }
            return self
                .dispatch_with_barrier(
                    key,
                    message,
                    vec![reservation],
                    "active_barrier",
                    retain_on_dispatch_failure,
                )
                .await
                .map_err(DispatchFailure::into_single_deferred);
        }

        match self.classify(&message).await {
            AggregationDecision::Immediate => {
                if let Err(error) = self.flush_key(&key, FlushReason::Barrier).await {
                    return Err(DeferredProcessError::blocked(message, reservation, error));
                }
                self.dispatch_with_barrier(
                    key,
                    message,
                    vec![reservation],
                    "immediate",
                    retain_on_dispatch_failure,
                )
                .await
                .map_err(DispatchFailure::into_single_deferred)
            }
            AggregationDecision::Aggregate => self
                .aggregate(key, message, reservation, retain_on_dispatch_failure)
                .await
                .map_err(|error| match error {
                    AggregateError::Blocked(deferred, error) => {
                        DeferredProcessError::from_deferred(*deferred, error)
                    }
                    AggregateError::Plain(error) => DeferredProcessError::plain(error),
                }),
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
        retain_on_dispatch_failure: bool,
    ) -> Result<(), AggregateError> {
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
            if let Err(error) = self.flush_key(&key, FlushReason::Barrier).await {
                return Err(AggregateError::Blocked(
                    Box::new(DeferredC2cMessage {
                        message,
                        reservation,
                    }),
                    error,
                ));
            }
            return self
                .dispatch_without_barrier(
                    &key,
                    message,
                    vec![reservation],
                    "oversized_message",
                    retain_on_dispatch_failure,
                )
                .await
                .map_err(|error| match error {
                    DispatchFailure::RolledBack(error) => AggregateError::Plain(error),
                    DispatchFailure::Retained {
                        message,
                        mut reservations,
                        error,
                    } => AggregateError::Blocked(
                        Box::new(DeferredC2cMessage {
                            message: *message,
                            reservation: reservations
                                .pop()
                                .expect("single oversized message has one reservation"),
                        }),
                        error,
                    ),
                });
        }

        if !self.batches.contains_key(&key) && self.batches.len() >= self.config.max_active_keys {
            warn!(
                scope_key = %mask_scope_key(&scope_key_from_c2c_message(&message)),
                active_keys = self.batches.len(),
                max_active_keys = self.config.max_active_keys,
                "message aggregation active key limit reached; dispatching immediately"
            );
            return self
                .dispatch_without_barrier(
                    &key,
                    message,
                    vec![reservation],
                    "active_key_limit",
                    retain_on_dispatch_failure,
                )
                .await
                .map_err(|error| match error {
                    DispatchFailure::RolledBack(error) => AggregateError::Plain(error),
                    DispatchFailure::Retained {
                        message,
                        mut reservations,
                        error,
                    } => AggregateError::Blocked(
                        Box::new(DeferredC2cMessage {
                            message: *message,
                            reservation: reservations
                                .pop()
                                .expect("single active-key-limit message has one reservation"),
                        }),
                        error,
                    ),
                });
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
                if let Err(error) = self.flush_key(&key, reason).await {
                    return Err(AggregateError::Blocked(
                        Box::new(DeferredC2cMessage {
                            message,
                            reservation,
                        }),
                        error,
                    ));
                }
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
            self.flush_key(&key, reason)
                .await
                .map_err(AggregateError::Plain)?;
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
        retain_on_failure: bool,
    ) -> Result<(), DispatchFailure> {
        let batch_size = reservations.len();
        let retained_message = message.clone();
        let (processed_tx, processed_rx) = oneshot::channel();
        if let Err(error) = self
            .dispatcher
            .enqueue_c2c_with_processed_ack(message, processed_tx)
            .await
        {
            let reservation_released = !retain_on_failure;
            if retain_on_failure {
                warn!(
                    scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                    error = %error,
                    stage,
                    aggregation_batch_size = batch_size,
                    reservation_released,
                    "message aggregation immediate dispatch failed; retained reservation for retry"
                );
                return Err(DispatchFailure::Retained {
                    message: Box::new(retained_message),
                    reservations,
                    error,
                });
            }
            rollback_reservations(reservations);
            warn!(
                scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                error = %error,
                stage,
                aggregation_batch_size = batch_size,
                reservation_released,
                "message aggregation immediate dispatch failed; rolled back reservation"
            );
            return Err(DispatchFailure::RolledBack(error));
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
        retain_on_failure: bool,
    ) -> Result<(), DispatchFailure> {
        let batch_size = reservations.len();
        let retained_message = message.clone();
        if let Err(error) = self.dispatcher.enqueue_c2c(message).await {
            let reservation_released = !retain_on_failure;
            if retain_on_failure {
                warn!(
                    scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                    error = %error,
                    stage,
                    aggregation_batch_size = batch_size,
                    reservation_released,
                    "message aggregation immediate dispatch failed; retained reservation for retry"
                );
                return Err(DispatchFailure::Retained {
                    message: Box::new(retained_message),
                    reservations,
                    error,
                });
            }
            rollback_reservations(reservations);
            warn!(
                scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                error = %error,
                stage,
                aggregation_batch_size = batch_size,
                reservation_released,
                "message aggregation immediate dispatch failed; rolled back reservation"
            );
            return Err(DispatchFailure::RolledBack(error));
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
        } else if let Err(error) = self.drain_deferred_for_key(&key).await {
            warn!(error = %error, "message aggregation deferred retry failed");
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

    fn defer_c2c(
        &mut self,
        key: AggregationKey,
        deferred: DeferredC2cMessage,
    ) -> anyhow::Result<()> {
        let queue_len = self.deferred.get(&key).map_or(0, VecDeque::len);
        if queue_len >= self.deferred_capacity_per_key
            || self.deferred_total_len >= self.deferred_capacity_total
        {
            let message_id = deferred.message.message_id.clone();
            deferred.reservation.rollback();
            warn!(
                scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                message_id = %message_id,
                deferred_len = queue_len,
                deferred_total_len = self.deferred_total_len,
                deferred_capacity = self.deferred_capacity_per_key,
                deferred_total_capacity = self.deferred_capacity_total,
                reservation_released = true,
                "message aggregation deferred queue full; current message not retained"
            );
            return Err(anyhow!("message aggregation deferred queue full"));
        }
        let queue = self.deferred.entry(key.clone()).or_default();
        queue.push_back(deferred);
        self.deferred_total_len = self.deferred_total_len.saturating_add(1);
        debug!(
            scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
            deferred_len = queue.len(),
            deferred_total_len = self.deferred_total_len,
            deferred_capacity = self.deferred_capacity_per_key,
            deferred_total_capacity = self.deferred_capacity_total,
            "message aggregation deferred current message behind restored batch"
        );
        self.schedule_deferred_retry(key);
        Ok(())
    }

    async fn drain_deferred_for_key(&mut self, key: &AggregationKey) -> anyhow::Result<()> {
        loop {
            let Some(deferred) = self.pop_front_deferred(key) else {
                self.deferred_retry_generations.remove(key);
                return Ok(());
            };

            // deferred 消息已经持有 reservation；重放时必须重新走分类、barrier 和容量检查。
            // 普通运行失败时继续保留队头并安排 deferred-only retry，避免依赖后续入站消息或平台重投。
            match self
                .process_reserved_c2c(
                    key.clone(),
                    deferred.message,
                    deferred.reservation,
                    !self.shutting_down,
                )
                .await
            {
                Ok(()) => {}
                Err(error) => {
                    if let Some(deferred) = error.deferred {
                        self.push_front_deferred(key.clone(), deferred);
                    }
                    if !self.shutting_down {
                        self.schedule_deferred_retry(key.clone());
                    }
                    return Err(error.error);
                }
            }
        }
    }

    fn pop_front_deferred(&mut self, key: &AggregationKey) -> Option<DeferredC2cMessage> {
        let mut queue = self.deferred.remove(key)?;
        let deferred = queue.pop_front();
        if deferred.is_some() {
            self.deferred_total_len = self.deferred_total_len.saturating_sub(1);
        }
        if !queue.is_empty() {
            self.deferred.insert(key.clone(), queue);
        }
        deferred
    }

    fn push_front_deferred(&mut self, key: AggregationKey, deferred: DeferredC2cMessage) {
        self.deferred.entry(key).or_default().push_front(deferred);
        self.deferred_total_len = self.deferred_total_len.saturating_add(1);
    }

    fn rollback_deferred_for_key(&mut self, key: &AggregationKey) -> usize {
        let Some(queue) = self.deferred.remove(key) else {
            self.deferred_retry_generations.remove(key);
            return 0;
        };
        let count = queue.len();
        self.deferred_total_len = self.deferred_total_len.saturating_sub(count);
        self.deferred_retry_generations.remove(key);
        for deferred in queue {
            deferred.reservation.rollback();
        }
        count
    }

    async fn flush_all(&mut self, reason: FlushReason) {
        let mut failed = 0usize;
        let mut rolled_back_deferred = 0usize;
        loop {
            let mut keys = self.batches.keys().cloned().collect::<Vec<_>>();
            for key in self.deferred.keys() {
                if !keys.contains(key) {
                    keys.push(key.clone());
                }
            }
            if keys.is_empty() {
                break;
            }

            let before_batches = self.batches.len();
            let before_deferred = self.deferred_total_len;
            for key in keys {
                if let Err(error) = self.flush_key(&key, reason).await {
                    failed += 1;
                    rolled_back_deferred += self.rollback_deferred_for_key(&key);
                    warn!(
                        scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                        error = %error,
                        remaining_failed_batches = failed,
                        "message aggregation shutdown flush failed"
                    );
                } else if let Err(error) = self.drain_deferred_for_key(&key).await {
                    failed += 1;
                    rolled_back_deferred += self.rollback_deferred_for_key(&key);
                    warn!(
                        scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                        error = %error,
                        remaining_failed_batches = failed,
                        "message aggregation shutdown deferred flush failed"
                    );
                }
            }

            // shutdown 不等待 quiet window：deferred 重放可能新建批次，继续固定点 flush，
            // 直到没有批次/队列，或本轮没有进展（失败路径已 rollback 对应 deferred）。
            if self.batches.len() == before_batches && self.deferred_total_len == before_deferred {
                break;
            }
        }
        let remaining_deferred = self.deferred_total_len;
        if failed > 0 || !self.batches.is_empty() || remaining_deferred > 0 {
            warn!(
                failed_batches = failed,
                remaining_batches = self.batches.len(),
                remaining_deferred,
                rolled_back_deferred,
                "message aggregation shutdown left unsubmitted batch/deferred messages"
            );
        }
    }

    async fn handle_deferred_retry(&mut self, key: AggregationKey, generation: u64) {
        if self.shutting_down || self.batches.contains_key(&key) {
            return;
        }
        if self
            .deferred_retry_generations
            .get(&key)
            .copied()
            .is_none_or(|current| current != generation)
        {
            return;
        }
        self.deferred_retry_generations.remove(&key);
        if let Err(error) = self.drain_deferred_for_key(&key).await {
            warn!(error = %error, "message aggregation deferred retry failed");
        }
    }

    fn schedule_deferred_retry(&mut self, key: AggregationKey) {
        if self.shutting_down
            || self.batches.contains_key(&key)
            || !self.deferred.contains_key(&key)
        {
            return;
        }
        if self.deferred_retry_generations.contains_key(&key) {
            return;
        }
        let generation = self
            .deferred_retry_generations
            .get(&key)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.deferred_retry_generations
            .insert(key.clone(), generation);
        let command_tx = self.command_tx.clone();
        let shutdown_token = self.shutdown_token.clone();
        let deadline = Instant::now() + self.config.quiet;
        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown_token.cancelled() => {}
                _ = sleep_until(deadline) => {
                    let _ = command_tx
                        .send(AggregatorCommand::DeferredRetry { key, generation })
                        .await;
                }
            }
        });
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

    #[cfg(test)]
    fn debug_inject_barrier_for_message(&mut self, message: &C2cMessage) {
        let key = self.key_for(message);
        let token = self.next_barrier_token;
        self.next_barrier_token = self.next_barrier_token.saturating_add(1);
        self.barriers
            .entry(key)
            .or_default()
            .push_back(BarrierEntry {
                token,
                resolved: None,
            });
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

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use qq_maid_common::input_part::MessageInputPart;
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
        AggregationDecision, AggregationKey, AggregatorCommand, BarrierEntry, BarrierEvent,
        BarrierStatus, FlushReason, PendingAggregation,
    },
};
use crate::{
    config::MessageAggregationConfig,
    gateway::{
        ReplyCache,
        dedupe::{Duplicate, MessageDedupe, MessageReservation},
        dispatcher::DispatcherEnqueueError,
        event::C2cMessage,
        logging::mask_scope_key,
        ping::is_ping_command,
        resolve_signals,
    },
    respond::{RespondClient, build_respond_content},
};

#[cfg(test)]
use super::types::BarrierDebugState;

const AGGREGATION_FAILURE_TEXT: &str = "当前服务暂时不可用，请稍后再试。";
const AGGREGATION_CANCELLED_TEXT: &str = "已取消本次图片/文件处理。";
const AGGREGATION_NOTHING_TO_CANCEL_TEXT: &str = "当前没有待取消的图片/文件处理。";

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
                let result = self
                    .dispatcher
                    .enqueue_group(*message)
                    .await
                    .map_err(Into::into);
                let _ = ack.send(result);
                false
            }
            AggregatorCommand::Timer { key, generation } => {
                self.handle_timer(key, generation).await;
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
                    let result = self
                        .dispatcher
                        .enqueue_group(*message)
                        .await
                        .map_err(Into::into);
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
        // 只有成功转交 Dispatcher 后才 commit，失败路径 rollback 后允许用户稍后重发。
        let reservation = match self.reserve_c2c_message(&message) {
            Ok(reservation) => reservation,
            Err(_) => {
                debug!(
                    scope_key = %mask_scope_key(&self.respond.scope_key_from_c2c_message(&message)),
                    message_id = %message.message_id,
                    "duplicate C2C message ignored before aggregation dispatch"
                );
                return Ok(());
            }
        };
        resolve_signals(&mut message, &self.reply_cache);
        self.drain_ready_barrier_events().await;
        if is_aggregation_cancel_command(&message.content)
            && self.batches.get(&key).is_some_and(batch_has_non_text_input)
        {
            return self
                .cancel_pending_aggregation(key, &message, reservation)
                .await;
        }
        self.process_reserved_c2c(key, message, reservation).await
    }

    async fn cancel_pending_aggregation(
        &mut self,
        key: AggregationKey,
        message: &C2cMessage,
        reservation: MessageReservation,
    ) -> anyhow::Result<()> {
        let had_batch = self
            .batches
            .remove(&key)
            .map(|batch| {
                commit_reservations(batch.reservations);
                true
            })
            .unwrap_or(false);
        reservation.commit();
        let text = if had_batch {
            AGGREGATION_CANCELLED_TEXT
        } else {
            AGGREGATION_NOTHING_TO_CANCEL_TEXT
        };
        self.dispatcher.notify_c2c_failure(message, text).await?;
        Ok(())
    }

    async fn process_reserved_c2c(
        &mut self,
        key: AggregationKey,
        message: C2cMessage,
        reservation: MessageReservation,
    ) -> anyhow::Result<()> {
        if self.has_active_barrier(&key) {
            if let Err(error) = self.flush_key(&key, FlushReason::Barrier, false).await {
                reservation.rollback();
                self.notify_failure_if_needed(&message, error.downcast_ref())
                    .await;
                return Err(error);
            }
            return self
                .dispatch_with_barrier_and_notify(key, message, vec![reservation], "active_barrier")
                .await;
        }

        match self.classify(&message).await {
            AggregationDecision::Immediate => {
                if let Err(error) = self.flush_key(&key, FlushReason::Barrier, false).await {
                    reservation.rollback();
                    self.notify_failure_if_needed(&message, error.downcast_ref())
                        .await;
                    return Err(error);
                }
                self.dispatch_with_barrier_and_notify(key, message, vec![reservation], "immediate")
                    .await
            }
            AggregationDecision::Aggregate => self.aggregate(key, message, reservation).await,
        }
    }

    async fn classify(&self, message: &C2cMessage) -> AggregationDecision {
        if !self.config.private_enabled
            || (message.content.trim().is_empty() && message.input_parts.is_empty())
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
                    scope_key = %mask_scope_key(&self.respond.scope_key_from_c2c_message(message)),
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
                scope_key = %mask_scope_key(&self.respond.scope_key_from_c2c_message(&message)),
                message_id = %message.message_id,
                "duplicate C2C message ignored by aggregation batch"
            );
            return Ok(());
        }

        let message_chars = message.content.chars().count();
        if message_chars > self.config.max_chars {
            if let Err(error) = self.flush_key(&key, FlushReason::Barrier, false).await {
                reservation.rollback();
                self.notify_failure_if_needed(&message, error.downcast_ref())
                    .await;
                return Err(error);
            }
            return self
                .dispatch_without_barrier_and_notify(
                    &key,
                    message,
                    vec![reservation],
                    "oversized_message",
                )
                .await;
        }

        if !self.batches.contains_key(&key) && self.batches.len() >= self.config.max_active_keys {
            warn!(
                scope_key = %mask_scope_key(&self.respond.scope_key_from_c2c_message(&message)),
                active_keys = self.batches.len(),
                max_active_keys = self.config.max_active_keys,
                "message aggregation active key limit reached; dispatching immediately"
            );
            return self
                .dispatch_without_barrier_and_notify(
                    &key,
                    message,
                    vec![reservation],
                    "active_key_limit",
                )
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
                if let Err(error) = self.flush_key(&key, reason, false).await {
                    reservation.rollback();
                    self.notify_failure_if_needed(&message, error.downcast_ref())
                        .await;
                    return Err(error);
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
            // 正常运行期因批次刚好触达上限而立即封口，也必须在交付失败时给用户一次明确反馈。
            self.flush_key(&key, reason, true).await?;
        } else {
            self.spawn_timer(key, generation);
        }
        Ok(())
    }

    async fn dispatch_with_barrier_and_notify(
        &mut self,
        key: AggregationKey,
        message: C2cMessage,
        reservations: Vec<MessageReservation>,
        stage: &'static str,
    ) -> anyhow::Result<()> {
        match self
            .dispatch_with_barrier(key, message.clone(), reservations, stage)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => {
                self.notify_failure_if_needed(&message, Some(&error)).await;
                Err(error.into())
            }
        }
    }

    async fn dispatch_with_barrier(
        &mut self,
        key: AggregationKey,
        message: C2cMessage,
        reservations: Vec<MessageReservation>,
        stage: &'static str,
    ) -> Result<(), DispatcherEnqueueError> {
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

    async fn dispatch_without_barrier_and_notify(
        &self,
        key: &AggregationKey,
        message: C2cMessage,
        reservations: Vec<MessageReservation>,
        stage: &'static str,
    ) -> anyhow::Result<()> {
        match self
            .dispatch_without_barrier(key, message.clone(), reservations, stage)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => {
                self.notify_failure_if_needed(&message, Some(&error)).await;
                Err(error.into())
            }
        }
    }

    async fn dispatch_without_barrier(
        &self,
        key: &AggregationKey,
        message: C2cMessage,
        reservations: Vec<MessageReservation>,
        stage: &'static str,
    ) -> Result<(), DispatcherEnqueueError> {
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
        if let Err(error) = self.flush_key(&key, reason, true).await {
            warn!(error = %error, "message aggregation timer flush failed");
        }
    }

    async fn flush_key(
        &mut self,
        key: &AggregationKey,
        reason: FlushReason,
        notify_on_failure: bool,
    ) -> anyhow::Result<()> {
        let Some(batch) = self.batches.remove(key) else {
            return Ok(());
        };
        let batch_size = batch.messages.len();
        let message = merge_batch(&batch, reason);
        let dispatch_result = if self.shutting_down || matches!(reason, FlushReason::Shutdown) {
            self.dispatcher.enqueue_c2c_silent(message.clone()).await
        } else {
            self.dispatcher.enqueue_c2c(message.clone()).await
        };
        if let Err(error) = dispatch_result {
            rollback_reservations(batch.reservations);
            warn!(
                scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                error = %error,
                stage = reason.as_str(),
                aggregation_batch_size = batch_size,
                reservation_released = true,
                "message aggregation flush failed; rolled back batch reservations"
            );
            if notify_on_failure && !self.shutting_down {
                self.notify_failure_if_needed(&message, Some(&error)).await;
            }
            return Err(error.into());
        }
        commit_reservations(batch.reservations);
        Ok(())
    }

    async fn flush_all(&mut self, reason: FlushReason) {
        let keys = self.batches.keys().cloned().collect::<Vec<_>>();
        let mut failed = 0usize;
        for key in keys {
            if let Err(error) = self.flush_key(&key, reason, false).await {
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
                "message aggregation shutdown left unsubmitted batch messages"
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

    async fn notify_failure_if_needed(
        &self,
        message: &C2cMessage,
        error: Option<&DispatcherEnqueueError>,
    ) {
        // Dispatcher 容量拒绝已排队“当前消息较多”提示；Aggregator 只补齐未提示的不可用类失败，避免同一次失败双提示。
        if matches!(
            error,
            Some(DispatcherEnqueueError::RejectedAndNotified { .. })
        ) {
            return;
        }
        if let Err(error) = self
            .dispatcher
            .notify_c2c_failure(message, AGGREGATION_FAILURE_TEXT)
            .await
        {
            warn!(
                scope_key = %mask_scope_key(&self.respond.scope_key_from_c2c_message(message)),
                message_id = %message.message_id,
                error = %error,
                "message aggregation local failure notification send failed"
            );
        }
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

fn is_aggregation_cancel_command(content: &str) -> bool {
    matches!(content.trim(), "取消" | "/取消" | "／取消")
}

fn batch_has_non_text_input(batch: &PendingAggregation) -> bool {
    batch.messages.iter().any(|message| {
        message
            .input_parts
            .iter()
            .any(MessageInputPart::is_non_text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{AgentTypingConfig, AppConfig, GroupMessageMode},
        gateway::{dedupe::MessageDedupe, event::GroupMessage},
        respond::RespondClient,
    };
    use async_trait::async_trait;
    use qq_maid_core::service::{
        CoreError, CoreHealthSnapshot, CoreInboundClassification, CoreRequest, CoreRespondOutput,
        CoreService, UpstreamStatusSnapshot,
    };
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
        time::Duration,
    };

    #[derive(Default)]
    struct NoopCore;

    #[async_trait]
    impl CoreService for NoopCore {
        async fn respond(&self, _request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
            Err(CoreError::new(
                "internal",
                "test",
                "unused in actor unit tests",
            ))
        }

        async fn classify_inbound(
            &self,
            _request: CoreRequest,
        ) -> Result<CoreInboundClassification, CoreError> {
            Err(CoreError::new(
                "internal",
                "test",
                "unused in actor unit tests",
            ))
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

    struct NoopDispatcher;

    #[async_trait]
    impl AggregationDispatcher for NoopDispatcher {
        async fn enqueue_c2c(&self, _message: C2cMessage) -> Result<(), DispatcherEnqueueError> {
            Ok(())
        }

        async fn enqueue_c2c_silent(
            &self,
            _message: C2cMessage,
        ) -> Result<(), DispatcherEnqueueError> {
            Ok(())
        }

        async fn enqueue_c2c_with_processed_ack(
            &self,
            _message: C2cMessage,
            _processed_ack: oneshot::Sender<()>,
        ) -> Result<(), DispatcherEnqueueError> {
            Ok(())
        }

        async fn enqueue_group(
            &self,
            _message: GroupMessage,
        ) -> Result<(), DispatcherEnqueueError> {
            Ok(())
        }

        async fn notify_c2c_failure(
            &self,
            _message: &C2cMessage,
            _text: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn key_for_builds_private_scope_key_parts() {
        let config = AppConfig {
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
            c2c_visible_progress_status_enabled: true,
            agent_typing: AgentTypingConfig {
                enabled: false,
                delay: Duration::from_secs(1),
            },
            markdown_chunk_soft_limit: 1800,
            text_chunk_soft_limit: 1800,
            wechat_service: crate::config::WechatServiceConfig::default(),
        };
        let (command_tx, command_rx) = mpsc::channel(8);
        let actor = AggregatorActor {
            config: config.message_aggregation.clone(),
            bot_instance: config.app_id,
            respond: RespondClient::new(Arc::new(NoopCore)),
            dispatcher: Arc::new(NoopDispatcher),
            dedupe: Arc::new(MessageDedupe::new(Duration::from_secs(60))),
            reply_cache: Arc::new(Mutex::new(HashMap::new())),
            command_rx,
            command_tx,
            batches: HashMap::new(),
            barriers: HashMap::new(),
            next_barrier_token: 1,
            barrier_tasks: JoinSet::new(),
            shutdown_token: CancellationToken::new(),
            shutting_down: false,
        };
        let key = actor.key_for(&C2cMessage {
            message_id: "m1".to_owned(),
            event_id: Some("e1".to_owned()),
            source_message_ids: vec!["m1".to_owned()],
            source_event_ids: vec!["e1".to_owned()],
            user_openid: "u1".to_owned(),
            content: "hello".to_owned(),
            reply: None,
            timestamp: None,
            first_message_timestamp: None,
            last_message_timestamp: None,
            input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("hello")],
            attachments: Vec::new(),
        });
        assert_eq!(key.bot_instance, "appid");
        assert_eq!(key.platform, "qq_official");
        assert_eq!(key.chat_type, "private");
        assert_eq!(key.conversation_id, "u1");
        assert_eq!(key.sender_id, "u1");
    }
}

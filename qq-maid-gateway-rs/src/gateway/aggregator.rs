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
    task::JoinHandle,
    time::{Instant, sleep_until, timeout},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{
    ReplyCache,
    dedupe::MessageDedupe,
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
    barriers: HashMap<AggregationKey, VecDeque<oneshot::Receiver<()>>>,
    shutdown_token: CancellationToken,
}

impl AggregatorActor {
    async fn run(mut self) {
        loop {
            let command = tokio::select! {
                _ = self.shutdown_token.cancelled() => break,
                command = self.command_rx.recv() => command,
            };
            let Some(command) = command else {
                break;
            };
            if self.handle_command(command).await {
                return;
            }
        }
        self.flush_all(FlushReason::Shutdown).await;
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
            }
        }
    }

    async fn handle_c2c(&mut self, mut message: C2cMessage) -> anyhow::Result<()> {
        let key = self.key_for(&message);
        resolve_signals(&mut message, &self.reply_cache);
        self.prune_barriers(&key);

        if self.has_active_barrier(&key) {
            self.flush_key(&key, FlushReason::Barrier).await?;
            return self.dispatch_with_barrier(key, message).await;
        }

        match self.classify(&message).await {
            AggregationDecision::Immediate => {
                self.flush_key(&key, FlushReason::Barrier).await?;
                self.dispatch_with_barrier(key, message).await
            }
            AggregationDecision::Aggregate => self.aggregate(key, message).await,
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

    async fn aggregate(&mut self, key: AggregationKey, message: C2cMessage) -> anyhow::Result<()> {
        if self.is_duplicate_for_open_batch(&key, &message) {
            debug!(
                scope_key = %mask_scope_key(&scope_key_from_c2c_message(&message)),
                message_id = %message.message_id,
                "duplicate C2C message ignored by aggregation batch"
            );
            return Ok(());
        }
        if self.is_duplicate_already_processed(&message) {
            debug!(
                scope_key = %mask_scope_key(&scope_key_from_c2c_message(&message)),
                message_id = %message.message_id,
                "duplicate C2C message ignored before aggregation"
            );
            return Ok(());
        }

        let message_chars = message.content.chars().count();
        if message_chars > self.config.max_chars {
            self.flush_key(&key, FlushReason::Barrier).await?;
            return self.dispatcher.enqueue_c2c(message).await;
        }

        if !self.batches.contains_key(&key) && self.batches.len() >= self.config.max_active_keys {
            warn!(
                scope_key = %mask_scope_key(&scope_key_from_c2c_message(&message)),
                active_keys = self.batches.len(),
                max_active_keys = self.config.max_active_keys,
                "message aggregation active key limit reached; dispatching immediately"
            );
            return self.dispatcher.enqueue_c2c(message).await;
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
            append_to_batch(batch, message, message_chars, now, &self.config);
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
                total_chars: 0,
            };
            append_to_batch(&mut batch, message, message_chars, now, &self.config);
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
    ) -> anyhow::Result<()> {
        let (processed_tx, processed_rx) = oneshot::channel();
        self.dispatcher
            .enqueue_c2c_with_processed_ack(message, processed_tx)
            .await?;
        self.barriers
            .entry(key)
            .or_default()
            .push_back(processed_rx);
        Ok(())
    }

    fn prune_barriers(&mut self, key: &AggregationKey) {
        let Some(queue) = self.barriers.get_mut(key) else {
            return;
        };
        while queue
            .front_mut()
            .is_some_and(|receiver| receiver.try_recv().is_ok())
        {
            queue.pop_front();
        }
        if queue.is_empty() {
            self.barriers.remove(key);
        }
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

    fn is_duplicate_already_processed(&self, message: &C2cMessage) -> bool {
        let now = std::time::Instant::now();
        message_id_values(message)
            .iter()
            .any(|id| self.dedupe.contains_recent_message(id, now))
            || event_id_values(message)
                .iter()
                .any(|id| self.dedupe.contains_recent_event(id, now))
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
        let Some(batch) = self.batches.remove(key) else {
            return Ok(());
        };
        let message = merge_batch(batch, reason);
        self.dispatcher.enqueue_c2c(message).await
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

fn merge_batch(batch: PendingAggregation, reason: FlushReason) -> C2cMessage {
    let mut messages = batch.messages;
    let mut merged = messages
        .pop()
        .expect("aggregation batch should never be empty when flushed");
    let mut all_messages = messages;
    all_messages.push(merged.clone());
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
        atomic::{AtomicBool, Ordering},
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
    }

    #[async_trait]
    impl AggregationDispatcher for MockDispatcher {
        async fn enqueue_c2c(&self, message: C2cMessage) -> anyhow::Result<()> {
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
        fn messages(&self) -> Vec<C2cMessage> {
            self.c2c.lock().unwrap().clone()
        }

        fn process_next(&self) {
            let Some((message, ack)) = self.pending_acks.lock().unwrap().pop_front() else {
                return;
            };
            let scope = scope_key_from_c2c_message(&message);
            let text = message.content.trim();
            if text.starts_with("/todo add") || text.starts_with("/memory") {
                self.core.pending.lock().unwrap().insert(scope);
            } else if matches!(text, "确认" | "取消") {
                self.core.pending.lock().unwrap().remove(&scope);
            }
            let _ = ack.send(());
        }

        fn pending_barriers(&self) -> usize {
            self.pending_acks.lock().unwrap().len()
        }
    }

    struct Harness {
        aggregator: MessageAggregator,
        dispatcher: Arc<MockDispatcher>,
        core: Arc<MockCore>,
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

    async fn enqueue(handle: &MessageAggregatorHandle, message: C2cMessage) {
        handle.enqueue_c2c(message).await.unwrap();
        yield_actor().await;
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
        let h = harness();
        let handle = h.aggregator.handle();
        enqueue(&handle, c2c("1", "u1", "/todo add 无时间买牛奶")).await;
        h.dispatcher.process_next();
        enqueue(&handle, c2c("2", "u1", "取消")).await;
        h.dispatcher.process_next();
        enqueue(&handle, c2c("3", "u1", "普通聊天")).await;
        assert_eq!(h.dispatcher.pending_barriers(), 0);
        h.aggregator.shutdown().await;
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

//! Gateway 入站消息聚合器。
//!
//! 聚合发生在 Dispatcher 之前：等待用户短暂停止输入时不占用 scope worker、
//! worker slot 或 LLM permit。命令和 pending 分类通过 Core 的轻量接口完成，
//! Gateway 只处理平台字段、/ping 本地命令和附件等自身边界。

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use anyhow::anyhow;
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

const AGGREGATOR_SHUTDOWN_TIMEOUT_SECS: u64 = 10;

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
            immediate_after_barrier: HashSet::new(),
            shutdown_token: shutdown_token.child_token(),
        };
        let shutdown_token = shutdown_token.child_token();
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

    pub(super) async fn shutdown(self) {
        self.shutdown_token.cancel();
        match timeout(
            Duration::from_secs(AGGREGATOR_SHUTDOWN_TIMEOUT_SECS),
            self.join_handle,
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(error = %error, "message aggregator task ended unexpectedly"),
            Err(_) => warn!("message aggregator shutdown timed out"),
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

struct AggregatorActor {
    config: MessageAggregationConfig,
    bot_instance: String,
    respond: RespondClient,
    dispatcher: MessageDispatcherHandle,
    dedupe: Arc<MessageDedupe>,
    reply_cache: ReplyCache,
    command_rx: mpsc::Receiver<AggregatorCommand>,
    command_tx: mpsc::Sender<AggregatorCommand>,
    batches: HashMap<AggregationKey, PendingAggregation>,
    immediate_after_barrier: HashSet<AggregationKey>,
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
            self.handle_command(command).await;
        }
        self.flush_all(FlushReason::Shutdown).await;
    }

    async fn handle_command(&mut self, command: AggregatorCommand) {
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
        }
    }

    async fn handle_c2c(&mut self, mut message: C2cMessage) -> anyhow::Result<()> {
        let key = self.key_for(&message);
        resolve_signals(&mut message, &self.reply_cache);
        if self.immediate_after_barrier.remove(&key) {
            self.flush_key(&key, FlushReason::Barrier).await?;
            return self.dispatcher.enqueue_c2c(message).await;
        }
        if !self.config.private_enabled || !self.can_aggregate(&message).await? {
            self.flush_key(&key, FlushReason::Barrier).await?;
            // 边界消息可能在 Dispatcher 中创建 pending。下一条同 scope 文本必须先按原顺序
            // 进入 Dispatcher，让 Core 看到真实 pending 状态，而不是在聚合层提前等待。
            self.immediate_after_barrier.insert(key);
            return self.dispatcher.enqueue_c2c(message).await;
        }

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

    async fn can_aggregate(&self, message: &C2cMessage) -> anyhow::Result<bool> {
        if message.content.trim().is_empty()
            || !message.attachments.is_empty()
            || message.reply.is_some()
            || is_ping_command(&message.content)
        {
            return Ok(false);
        }
        let content = build_respond_content(message);
        let classification = self.respond.classify_c2c(message, content).await?;
        Ok(classification.kind == CoreInboundKind::NormalChat)
    }

    fn is_duplicate_for_open_batch(&self, key: &AggregationKey, message: &C2cMessage) -> bool {
        let Some(batch) = self.batches.get(key) else {
            return false;
        };
        message
            .source_message_ids
            .iter()
            .any(|id| batch.message_ids.contains(id))
            || message
                .source_event_ids
                .iter()
                .any(|id| batch.event_ids.contains(id))
            || message
                .event_id
                .as_ref()
                .is_some_and(|id| batch.event_ids.contains(id))
    }

    fn is_duplicate_already_processed(&self, message: &C2cMessage) -> bool {
        message
            .source_message_ids
            .iter()
            .any(|id| self.dedupe.contains_recent(id))
            || self.dedupe.contains_recent(&message.message_id)
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
        for key in keys {
            if let Err(error) = self.flush_key(&key, reason).await {
                warn!(
                    scope_key = %mask_scope_key(&format!("private:{}", key.conversation_id)),
                    error = %error,
                    "message aggregation shutdown flush failed"
                );
            }
        }
        if !self.batches.is_empty() {
            warn!(
                remaining_batches = self.batches.len(),
                "message aggregation shutdown left unflushed batches"
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
    for id in &message.source_message_ids {
        if !id.trim().is_empty() {
            batch.message_ids.insert(id.clone());
        }
    }
    if !message.message_id.trim().is_empty() {
        batch.message_ids.insert(message.message_id.clone());
    }
    for id in &message.source_event_ids {
        if !id.trim().is_empty() {
            batch.event_ids.insert(id.clone());
        }
    }
    if let Some(event_id) = message.event_id.as_ref().filter(|id| !id.trim().is_empty()) {
        batch.event_ids.insert(event_id.clone());
    }
    batch.messages.push(message);
    batch.total_chars += message_chars;
    batch.last_received_at = now;
    batch.quiet_deadline = (now + config.quiet).min(batch.hard_deadline);
    batch.generation = batch.generation.saturating_add(1);
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
    merged.source_message_ids = all_messages
        .iter()
        .flat_map(|message| message.source_message_ids.iter().cloned())
        .collect();
    merged.source_event_ids = all_messages
        .iter()
        .flat_map(|message| message.source_event_ids.iter().cloned())
        .collect();
    merged.timestamp = all_messages
        .last()
        .and_then(|message| message.timestamp.clone())
        .or_else(|| {
            all_messages
                .first()
                .and_then(|message| message.timestamp.clone())
        });
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

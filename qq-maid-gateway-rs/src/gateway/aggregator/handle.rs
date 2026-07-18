use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::anyhow;
use async_trait::async_trait;
use tokio::{
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{actor::AggregatorActor, types::AggregatorCommand};

#[cfg(test)]
use super::types::BarrierDebugState;
use crate::{
    config::AppConfig,
    gateway::{
        dedupe::MessageDedupe,
        dispatcher::{DispatcherEnqueueError, MessageDispatcherHandle},
        event::{C2cMessage, GroupMessage},
    },
    respond::RespondClient,
};

const AGGREGATOR_SHUTDOWN_TIMEOUT_SECS: u64 = 30;

#[async_trait]
pub(in crate::gateway) trait AggregationDispatcher: Send + Sync {
    async fn enqueue_c2c(&self, message: C2cMessage) -> Result<(), DispatcherEnqueueError>;

    async fn enqueue_c2c_silent(&self, message: C2cMessage) -> Result<(), DispatcherEnqueueError>;

    async fn enqueue_c2c_with_processed_ack(
        &self,
        message: C2cMessage,
        processed_ack: oneshot::Sender<()>,
    ) -> Result<(), DispatcherEnqueueError>;

    async fn enqueue_group(&self, message: GroupMessage) -> Result<(), DispatcherEnqueueError>;

    async fn notify_c2c_failure(&self, message: &C2cMessage, text: &str) -> anyhow::Result<()>;
}

#[async_trait]
impl AggregationDispatcher for MessageDispatcherHandle {
    async fn enqueue_c2c(&self, message: C2cMessage) -> Result<(), DispatcherEnqueueError> {
        MessageDispatcherHandle::enqueue_c2c(self, message).await
    }

    async fn enqueue_c2c_silent(&self, message: C2cMessage) -> Result<(), DispatcherEnqueueError> {
        MessageDispatcherHandle::enqueue_c2c_silent(self, message).await
    }

    async fn enqueue_c2c_with_processed_ack(
        &self,
        message: C2cMessage,
        processed_ack: oneshot::Sender<()>,
    ) -> Result<(), DispatcherEnqueueError> {
        MessageDispatcherHandle::enqueue_c2c_with_processed_ack(self, message, processed_ack).await
    }

    async fn enqueue_group(&self, message: GroupMessage) -> Result<(), DispatcherEnqueueError> {
        MessageDispatcherHandle::enqueue_group(self, message).await
    }

    async fn notify_c2c_failure(&self, message: &C2cMessage, text: &str) -> anyhow::Result<()> {
        MessageDispatcherHandle::notify_c2c_failure(self, message, text).await
    }
}

#[derive(Clone)]
pub(in crate::gateway) struct MessageAggregatorHandle {
    command_tx: mpsc::Sender<AggregatorCommand>,
}

impl MessageAggregatorHandle {
    pub(in crate::gateway) async fn enqueue_c2c(&self, message: C2cMessage) -> anyhow::Result<()> {
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

    pub(in crate::gateway) async fn enqueue_group(
        &self,
        message: GroupMessage,
    ) -> anyhow::Result<()> {
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
    pub(super) async fn debug_barrier_state(&self) -> BarrierDebugState {
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

pub(in crate::gateway) struct MessageAggregator {
    handle: MessageAggregatorHandle,
    join_handle: JoinHandle<()>,
    shutdown_token: CancellationToken,
}

impl MessageAggregator {
    pub(in crate::gateway) fn new(
        config: AppConfig,
        bot_instance: String,
        respond: RespondClient,
        dispatcher: MessageDispatcherHandle,
        dedupe: Arc<MessageDedupe>,
        shutdown_token: CancellationToken,
    ) -> Self {
        Self::new_with_dispatcher(
            config,
            bot_instance,
            respond,
            Arc::new(dispatcher),
            dedupe,
            shutdown_token,
        )
    }

    pub(in crate::gateway) fn new_with_dispatcher(
        config: AppConfig,
        bot_instance: String,
        respond: RespondClient,
        dispatcher: Arc<dyn AggregationDispatcher>,
        dedupe: Arc<MessageDedupe>,
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
            command_prefix: config.command_prefix,
            bot_instance,
            respond,
            dispatcher,
            dedupe,
            command_rx,
            command_tx: command_tx.clone(),
            batches: HashMap::new(),
            barriers: HashMap::new(),
            next_barrier_token: 1,
            barrier_tasks: JoinSet::new(),
            shutdown_token: shutdown_token.clone(),
            shutting_down: false,
        };
        let join_handle = tokio::spawn(actor.run());
        Self {
            handle: MessageAggregatorHandle { command_tx },
            join_handle,
            shutdown_token,
        }
    }

    pub(in crate::gateway) fn handle(&self) -> MessageAggregatorHandle {
        self.handle.clone()
    }

    pub(in crate::gateway) async fn shutdown(mut self) {
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

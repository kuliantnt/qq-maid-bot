//! Gateway 会话级消息调度器。
//!
//! 该模块把 QQ 入站消息从 WebSocket 读循环中解耦出来：同一 scope 串行、不同 scope 并发，
//! 并通过有界 command channel / worker queue / reject channel 避免无界积压。
//!
//! 实现按职责拆分到子模块，对外公开入口仍只暴露 `MessageDispatcher` /
//! `MessageDispatcherHandle` / `DispatcherEnqueueError`，gateway 侧调用方式不变：
//! - [`types`]：命令、入站封装、拒绝通知等内部数据类型与常量。
//! - [`actor`]：scope 状态机、入队分发与 worker 生命周期管理。
//! - [`worker`]：会话 worker 执行循环与 idle 回收。
//! - [`reject`]：拒绝通知发送 worker。

mod actor;
mod reject;
mod types;
mod worker;

// 子模块内部类型原本是 mod.rs 的私有类型，dispatcher 内联测试通过 `use super::*`
// 窥探它们；这里把它们引入 dispatcher 命名空间，使 `dispatcher/tests` 仍能保持
// 原写法不变。`DispatcherEnqueueError` 还需要 `pub(super)` 再导出给 gateway 调用方。
pub(super) use types::DispatcherEnqueueError;
use types::{
    COMMAND_CHANNEL_MULTIPLIER, DispatcherCommand, DispatcherEnqueueResult, InboundEnvelope,
    QueuedMessage, RejectMetrics, RejectNotification, RejectTarget, SHUTDOWN_DRAIN_TIMEOUT_SECS,
    WORKER_CANCEL_TIMEOUT_SECS,
};
// 仅内联测试用到的类型与 trait 单独按 cfg(test) 引入，避免非测试构建报应未使用导入。
#[cfg(test)]
use types::{IdleDecision, ScopeEntry, ScopeState, WorkerExitReason};

// `DispatcherActor` / `RealMessageHandler` 由 actor 子模块提供，mod.rs 负责装配。
// `RealMessageHandler::new` 已直接返回 `Arc<dyn MessageHandler>`，因此 mod.rs 无须
// 为类型转换再引入 `MessageHandler` / `HandlerFuture`；这两者仅被内联测试使用。
use actor::{DispatcherActor, RealMessageHandler};
#[cfg(test)]
use actor::{HandlerFuture, MessageHandler};

use std::{sync::Arc, time::Duration};
// 这几个标准类型/枚举仅被 dispatcher 内联测试使用（`use super::*` 窥探），按 cfg(test)
// 引入，避免非测试构建报应未使用导入。
#[cfg(test)]
use std::{
    collections::{HashMap, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::anyhow;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{
    BotOutboundCache,
    bot_identity::SharedBotIdentity,
    dedupe::MessageDedupe,
    event::{C2cMessage, GroupMessage},
    group_filter::GroupCooldowns,
    ping::GatewayRuntimeStatus,
    ref_index::SharedRefIndex,
};
use crate::{
    api::QqApiClient, auth::AccessTokenManager, config::AppConfig, respond::RespondClient,
};

#[derive(Clone)]
pub(super) struct MessageDispatcherHandle {
    command_tx: mpsc::Sender<DispatcherCommand>,
    reject_tx: mpsc::Sender<RejectNotification>,
    respond: RespondClient,
}

impl MessageDispatcherHandle {
    pub(super) async fn enqueue_c2c(&self, message: C2cMessage) -> DispatcherEnqueueResult {
        self.enqueue_c2c_inner(message, None, true).await
    }

    pub(super) async fn enqueue_c2c_silent(&self, message: C2cMessage) -> DispatcherEnqueueResult {
        self.enqueue_c2c_inner(message, None, false).await
    }

    pub(super) async fn enqueue_c2c_with_processed_ack(
        &self,
        message: C2cMessage,
        processed_ack: oneshot::Sender<()>,
    ) -> DispatcherEnqueueResult {
        self.enqueue_c2c_inner(message, Some(processed_ack), true)
            .await
    }

    async fn enqueue_c2c_inner(
        &self,
        message: C2cMessage,
        processed_ack: Option<oneshot::Sender<()>>,
        notify_on_reject: bool,
    ) -> DispatcherEnqueueResult {
        let scope_key = self.respond.scope_key_from_c2c_message(&message);
        let target = RejectTarget::C2c {
            user_openid: message.user_openid.clone(),
            message_id: message.message_id.clone(),
        };
        self.enqueue(
            InboundEnvelope::C2c(message),
            scope_key,
            target,
            processed_ack,
            notify_on_reject,
        )
        .await
    }

    pub(super) async fn enqueue_group(&self, message: GroupMessage) -> DispatcherEnqueueResult {
        let scope_key = self.respond.scope_key_from_group_message(&message);
        let target = RejectTarget::Group {
            group_openid: message.group_openid.clone(),
            message_id: message.message_id.clone(),
        };
        self.enqueue(
            InboundEnvelope::Group(message),
            scope_key,
            target,
            None,
            true,
        )
        .await
    }

    async fn enqueue(
        &self,
        envelope: InboundEnvelope,
        scope_key: String,
        reject_target: RejectTarget,
        processed_ack: Option<oneshot::Sender<()>>,
        notify_on_reject: bool,
    ) -> DispatcherEnqueueResult {
        let (ack_tx, ack_rx) = oneshot::channel();
        let command = DispatcherCommand::Enqueue {
            scope_key,
            // command channel 满时优先做短暂背压等待，不把瞬时积压直接放大成用户可见失败。
            // 真正 closed/unavailable 的情况仍然快速返回错误，由上层决定是否提示用户稍后再试。
            message: Box::new(QueuedMessage {
                envelope,
                reject_target,
                processed_ack,
                notify_on_reject,
            }),
            ack: ack_tx,
        };
        self.command_tx
            .send(command)
            .await
            .map_err(|_| DispatcherEnqueueError::Unavailable {
                reason: "dispatcher_closed",
            })?;
        match ack_rx.await {
            Ok(result) => result,
            Err(_) => Err(DispatcherEnqueueError::Unavailable {
                reason: "dispatcher_unavailable",
            }),
        }
    }

    pub(super) async fn notify_c2c_failure(
        &self,
        message: &C2cMessage,
        text: &str,
    ) -> anyhow::Result<()> {
        let notification = RejectNotification {
            scope_key: self.respond.scope_key_from_c2c_message(message),
            target: RejectTarget::C2c {
                user_openid: message.user_openid.clone(),
                message_id: message.message_id.clone(),
            },
            message: text.to_owned(),
        };
        self.reject_tx
            .send(notification)
            .await
            .map_err(|_| anyhow!("dispatcher reject channel closed"))
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
        ref_index: SharedRefIndex,
        group_outbound_cache: Arc<std::sync::Mutex<BotOutboundCache>>,
        group_cooldowns: Arc<std::sync::Mutex<GroupCooldowns>>,
        bot_identity: SharedBotIdentity,
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
        let handle_respond = respond.clone();
        let handler = RealMessageHandler::new(
            config.clone(),
            auth,
            respond,
            api.clone(),
            dedupe,
            ref_index,
            group_outbound_cache,
            group_cooldowns,
            bot_identity,
            runtime.clone(),
        );
        let actor = DispatcherActor::new(
            config,
            api,
            runtime,
            command_rx,
            command_tx.clone(),
            reject_tx,
            reject_rx,
            reject_metrics,
            handler,
            shutdown_token.clone(),
        );
        let join_handle = tokio::spawn(actor.run());
        Self {
            handle: MessageDispatcherHandle {
                command_tx,
                reject_tx: handle_reject_tx,
                respond: handle_respond,
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

#[cfg(test)]
mod tests;

//! Dispatcher 会话 worker 执行循环与 idle 回收。
//!
//! `run_worker` 在独立 task 中串行消费某 scope 的队列消息，并在 idle 超时后向
//! actor 询问是否回收。串行语义、idle 判定与取消处理保持与拆分前一致，没有
//! 改变 QQ 消息串行化或 worker 回收行为。

use std::{sync::Arc, time::Duration};

use tokio::{
    sync::{mpsc, oneshot},
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::super::logging::mask_scope_key;
// `MessageHandler` 定义在 actor 子模块，actor 通过 `pub(super)` 暴露给 dispatcher
// 后代；worker 作为 dispatcher 的子模块可以直接引用。`HandlerFuture` 仅作为
// `handler.handle` 的返回类型被隐式使用，这里不需要具名导入。
use super::actor::MessageHandler;
use super::types::{DispatcherCommand, IdleDecision, QueuedMessage, WorkerExitReason};

/// 单个 scope worker 的执行上下文，由 actor 在 `spawn_worker` 时构造。
pub(super) struct WorkerContext {
    pub(super) scope_key: String,
    pub(super) generation: u64,
    pub(super) handler: Arc<dyn MessageHandler>,
    pub(super) command_tx: mpsc::Sender<DispatcherCommand>,
    pub(super) rx: mpsc::Receiver<QueuedMessage>,
    pub(super) idle_timeout: Duration,
    pub(super) shutdown_token: CancellationToken,
}

pub(super) async fn run_worker(mut ctx: WorkerContext) -> WorkerExitReason {
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
        let QueuedMessage {
            envelope,
            processed_ack,
            ..
        } = message;
        let result = ctx.handler.handle(envelope).await;
        if let Some(ack) = processed_ack {
            let _ = ack.send(());
        }
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

//! Dispatcher 内部数据类型定义。
//!
//! 这里集中放置 dispatcher 用到的命令、入站封装、队列消息、拒绝通知和 worker
//! 状态枚举等纯数据类型，便于 `actor`、`worker`、`reject` 子模块共享，并降低
//! `mod.rs` 的单文件复杂度。类型语义与拆分前完全一致，没有改变可观测行为。
//!
//! 注意：这些类型原本是 `dispatcher/mod.rs` 内的私有类型，拆分后统一标注为
//! `pub(super)`（相对于 `dispatcher::types` 的父 `dispatcher`），仅对
//! `gateway::dispatcher` 模块及其子模块可见；gateway 外部仍只能看到 `mod.rs`
//! 暴露的 `MessageDispatcher` / `MessageDispatcherHandle` / `DispatcherEnqueueError`。

use std::{collections::VecDeque, sync::atomic::AtomicU64};

use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

// 平台消息类型来自 gateway::event，pub(super) 在 event 中对 gateway 可见，
// dispatcher 作为 gateway 的后代可以引用。
use super::super::event::{C2cMessage, GroupMessage};

// 这些常量直接服务于 dispatcher 的容量、提示文案与超时语义，集中在此处便于引用。
// 改动它们会影响 QQ 入站消息串行化、队列容量提示和 shutdown 回收行为，需谨慎。

/// Dispatcher 通过拒绝通道给用户发送容量提示时使用的固定文案。
pub(super) const REJECT_QUEUE_TEXT: &str = "当前消息较多，请稍后再试。";
/// shutdown flush 阶段回收 worker 的最长等待秒数。
pub(super) const SHUTDOWN_DRAIN_TIMEOUT_SECS: u64 = 10;
/// worker 取消超时窗口，给被取消的 worker 一点收尾时间。
pub(super) const WORKER_CANCEL_TIMEOUT_SECS: u64 = 1;
/// command channel 容量相对活跃 worker 上限的倍数，避免指令通道成为瓶颈。
pub(super) const COMMAND_CHANNEL_MULTIPLIER: usize = 4;

pub(super) type DispatcherEnqueueResult = Result<(), DispatcherEnqueueError>;

// `DispatcherEnqueueError` 会由 `mod.rs` 以 `pub(super)` 再导出给 gateway 调用方
// （aggregator 等），因此这里需要比 `pub(super)` 更宽一点：可见到整个 gateway 模块。
#[derive(Debug, Error)]
pub(in crate::gateway) enum DispatcherEnqueueError {
    /// Dispatcher 已经通过拒绝通道给用户发送过容量提示，上层不得重复发送服务不可用提示。
    #[error("dispatcher rejected message and queued user notification: {reason}")]
    RejectedAndNotified { reason: &'static str },
    /// Dispatcher 已关闭或不可用且没有自行提示用户，上层需要决定是否给出兜底提示。
    #[error("dispatcher unavailable: {reason}")]
    Unavailable { reason: &'static str },
}

#[derive(Debug)]
pub(super) enum DispatcherCommand {
    Enqueue {
        scope_key: String,
        // QueuedMessage 可能携带完整平台消息，装箱后可避免 command 枚举整体尺寸过大。
        message: Box<QueuedMessage>,
        ack: oneshot::Sender<DispatcherEnqueueResult>,
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
pub(super) enum InboundEnvelope {
    C2c(C2cMessage),
    Group(GroupMessage),
}

#[derive(Debug)]
pub(super) struct QueuedMessage {
    pub(super) envelope: InboundEnvelope,
    pub(super) reject_target: RejectTarget,
    // 仅供聚合器建立边界屏障：Dispatcher 入队 ack 只表示已接收，
    // processed_ack 要等 worker 真正处理完边界消息后才触发。
    pub(super) processed_ack: Option<oneshot::Sender<()>>,
    // shutdown flush 失败只回滚不提示；正常入站容量拒绝仍由 Dispatcher 提示“稍后再试”。
    pub(super) notify_on_reject: bool,
}

#[derive(Debug, Clone)]
pub(super) enum RejectTarget {
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
pub(super) struct RejectNotification {
    pub(super) scope_key: String,
    pub(super) target: RejectTarget,
    pub(super) message: String,
}

#[derive(Debug, Default)]
pub(super) struct RejectMetrics {
    pub(super) total: AtomicU64,
    pub(super) dropped: AtomicU64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum IdleDecision {
    StayActive,
    RetireNow,
}

#[derive(Debug)]
pub(super) enum WorkerExitReason {
    Completed,
    Cancelled,
    Panic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScopeState {
    Active,
    Retiring,
}

/// 单个 scope 的运行时状态。sender 存在表示有活跃 worker 持有有界队列；
/// worker 进入 Retiring 后 sender 被清空，新消息落入 backlog 等待继任 worker。
pub(super) struct ScopeEntry {
    pub(super) state: ScopeState,
    pub(super) generation: u64,
    pub(super) sender: Option<mpsc::Sender<QueuedMessage>>,
    pub(super) queue_len: usize,
    pub(super) backlog: VecDeque<QueuedMessage>,
    pub(super) worker_cancel: CancellationToken,
}

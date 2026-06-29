use std::collections::HashSet;

use tokio::{sync::oneshot, time::Instant};

use crate::gateway::{
    dedupe::MessageReservation,
    event::{C2cMessage, GroupMessage},
};

pub(super) enum AggregatorCommand {
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
    DeferredRetry {
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
    #[cfg(test)]
    DebugInjectBarrier {
        message: Box<C2cMessage>,
        ack: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct AggregationKey {
    pub(super) bot_instance: String,
    pub(super) platform: &'static str,
    pub(super) chat_type: &'static str,
    pub(super) conversation_id: String,
    pub(super) sender_id: String,
}

pub(super) struct PendingAggregation {
    pub(super) first_received_at: Instant,
    pub(super) last_received_at: Instant,
    pub(super) quiet_deadline: Instant,
    pub(super) hard_deadline: Instant,
    pub(super) generation: u64,
    pub(super) messages: Vec<C2cMessage>,
    pub(super) message_ids: HashSet<String>,
    pub(super) event_ids: HashSet<String>,
    pub(super) reservations: Vec<MessageReservation>,
    pub(super) total_chars: usize,
}

pub(super) struct DeferredC2cMessage {
    pub(super) message: C2cMessage,
    pub(super) reservation: MessageReservation,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum FlushReason {
    QuietTimeout,
    MaxWait,
    MaxMessages,
    MaxChars,
    Barrier,
    Shutdown,
}

impl FlushReason {
    pub(super) fn as_str(self) -> &'static str {
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

pub(super) enum AggregationDecision {
    Aggregate,
    Immediate,
}

pub(super) struct DeferredProcessError {
    pub(super) error: anyhow::Error,
    pub(super) deferred: Option<DeferredC2cMessage>,
}

impl DeferredProcessError {
    pub(super) fn plain(error: anyhow::Error) -> Self {
        Self {
            error,
            deferred: None,
        }
    }

    pub(super) fn blocked(
        message: C2cMessage,
        reservation: MessageReservation,
        error: anyhow::Error,
    ) -> Self {
        Self::from_deferred(
            DeferredC2cMessage {
                message,
                reservation,
            },
            error,
        )
    }

    pub(super) fn from_deferred(deferred: DeferredC2cMessage, error: anyhow::Error) -> Self {
        Self {
            error,
            deferred: Some(deferred),
        }
    }
}

pub(super) enum AggregateError {
    Blocked(Box<DeferredC2cMessage>, anyhow::Error),
    Plain(anyhow::Error),
}

pub(super) enum DispatchFailure {
    RolledBack(anyhow::Error),
    Retained {
        message: Box<C2cMessage>,
        reservations: Vec<MessageReservation>,
        error: anyhow::Error,
    },
}

impl DispatchFailure {
    pub(super) fn into_single_deferred(self) -> DeferredProcessError {
        match self {
            Self::RolledBack(error) => DeferredProcessError::plain(error),
            Self::Retained {
                message,
                mut reservations,
                error,
            } => {
                let Some(reservation) = reservations.pop() else {
                    return DeferredProcessError::plain(error);
                };
                DeferredProcessError::blocked(*message, reservation, error)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BarrierStatus {
    Completed,
    Closed,
    Cancelled,
}

#[derive(Debug)]
pub(super) struct BarrierEvent {
    pub(super) key: AggregationKey,
    pub(super) token: u64,
    pub(super) status: BarrierStatus,
}

#[derive(Debug)]
pub(super) struct BarrierEntry {
    pub(super) token: u64,
    pub(super) resolved: Option<BarrierStatus>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BarrierDebugState {
    pub(super) barrier_count: usize,
    pub(super) task_count: usize,
}

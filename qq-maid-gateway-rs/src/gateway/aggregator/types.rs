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
    Shutdown {
        ack: oneshot::Sender<anyhow::Result<()>>,
    },
    #[cfg(test)]
    DebugBarrierState {
        ack: oneshot::Sender<BarrierDebugState>,
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

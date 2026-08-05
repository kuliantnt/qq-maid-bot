use crate::api::{C2cStreamResponse, C2cStreamTransportState, SendMessageIds};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum C2cStreamLifecycle {
    Idle,
    Opening,
    Active,
    Completed,
    Failed,
}

/// 一个官方 StreamSession 的完整 Gateway 状态。
///
/// `transport` 对应官方协议游标；其余字段保存平台已经接受的正文和结果，
/// 从而保证失败请求不会推进本地可见状态，也不会把 stream id 当成 ref_idx。
#[derive(Debug, Clone)]
pub(crate) struct C2cStreamState {
    pub(crate) lifecycle: C2cStreamLifecycle,
    pub(crate) transport: C2cStreamTransportState,
    pub(crate) last_accepted_full: String,
    pub(crate) last_successful_update: Option<C2cStreamResponse>,
    pub(crate) has_accepted_content: bool,
    pub(crate) final_result: Option<C2cStreamResponse>,
    pub(crate) completion_attempted: bool,
}

impl C2cStreamState {
    pub(crate) fn new() -> Self {
        Self {
            lifecycle: C2cStreamLifecycle::Idle,
            transport: C2cStreamTransportState::new(),
            last_accepted_full: String::new(),
            last_successful_update: None,
            has_accepted_content: false,
            final_result: None,
            completion_attempted: false,
        }
    }

    pub(crate) fn begin_opening(&mut self) {
        self.lifecycle = C2cStreamLifecycle::Opening;
    }

    pub(crate) fn accept_update(&mut self, full_text: &str, response: C2cStreamResponse) {
        self.last_accepted_full.clear();
        self.last_accepted_full.push_str(full_text);
        self.last_successful_update = Some(response);
        self.has_accepted_content = true;
        self.lifecycle = C2cStreamLifecycle::Active;
    }

    pub(crate) fn mark_failed(&mut self) {
        self.lifecycle = C2cStreamLifecycle::Failed;
    }

    pub(crate) fn mark_completion_attempted(&mut self) -> bool {
        if self.completion_attempted {
            return false;
        }
        self.completion_attempted = true;
        true
    }

    pub(crate) fn accept_completion(&mut self, response: C2cStreamResponse) {
        self.final_result = Some(response);
        self.lifecycle = C2cStreamLifecycle::Completed;
    }

    pub(crate) fn final_ids(&self) -> Option<SendMessageIds> {
        let response = self.final_result.as_ref()?;
        Some(SendMessageIds {
            message_id: Some(response.message_id.clone()),
            ref_index_id: response.ref_index_id.clone(),
        })
    }
}

#[derive(Debug)]
pub(crate) enum C2cStreamingPhase {
    Pending(C2cStreamState),
    Active(C2cStreamState),
    BrokenActive(C2cStreamState),
    Completed,
}

impl C2cStreamingPhase {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Pending(_) => "pending",
            Self::Active(_) => "active",
            Self::BrokenActive(_) => "broken_active",
            Self::Completed => "completed",
        }
    }
}

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
/// 从而保证失败请求不会推进本地可见正文，也不会把 stream id 当成 ref_idx。
#[derive(Debug, Clone)]
pub(crate) struct C2cStreamState {
    pub(crate) lifecycle: C2cStreamLifecycle,
    pub(crate) transport: C2cStreamTransportState,
    pub(crate) last_accepted_full: String,
    pub(crate) last_successful_update: Option<C2cStreamResponse>,
    /// 最近一次成功响应返回的非空 `ext_info.ref_idx`。
    ///
    /// QQ 的 update/complete 响应不保证每次都带引用索引，因此不能只读取
    /// `final_result.ref_index_id`，也不能用消息 ID 或 stream ID 代替它。
    pub(crate) last_successful_ref_index_id: Option<String>,
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
            last_successful_ref_index_id: None,
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
        self.remember_ref_index(&response);
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
        self.remember_ref_index(&response);
        self.final_result = Some(response);
        self.lifecycle = C2cStreamLifecycle::Completed;
    }

    fn remember_ref_index(&mut self, response: &C2cStreamResponse) {
        let Some(ref_index_id) = response
            .ref_index_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        self.last_successful_ref_index_id = Some(ref_index_id.to_owned());
    }

    pub(crate) fn final_ids(&self) -> Option<SendMessageIds> {
        let response = self.final_result.as_ref()?;
        Some(SendMessageIds {
            message_id: Some(response.message_id.clone()),
            ref_index_id: self.last_successful_ref_index_id.clone(),
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

//! Respond 层的 pending 会话写入 helper。
//!
//! 具体业务 pending 的分发、owner 校验、过期文案和状态机都在对应工具域内维护。

use crate::{error::LlmError, runtime::session::SessionRecord};

use super::{
    RespondResponse, RustRespondService,
    common::{CommandBody, command_response, session_error},
};

impl RustRespondService {
    /// 追加回复到会话记录并返回响应。不改变待确认操作状态。
    pub(crate) fn append_pending_response(
        &self,
        session: &mut SessionRecord,
        user_text: &str,
        reply: impl Into<CommandBody>,
        command: impl Into<String>,
    ) -> Result<RespondResponse, LlmError> {
        let reply = reply.into();
        self.session_store
            .append_exchange_with_latest(session, user_text, &reply.text, |latest, current| {
                latest.merge_interaction_side_effects_from(current);
            })
            .map_err(session_error)?;
        Ok(command_response(
            reply,
            Some(session.session_id.clone()),
            Some(command),
        ))
    }

    /// 清除待确认操作并追加回复到会话记录。
    pub(crate) fn clear_pending_response(
        &self,
        session: &mut SessionRecord,
        user_text: &str,
        reply: impl Into<CommandBody>,
        command: impl Into<String>,
    ) -> Result<RespondResponse, LlmError> {
        session.pending_operation = None;
        self.append_pending_response(session, user_text, reply, command)
    }
}

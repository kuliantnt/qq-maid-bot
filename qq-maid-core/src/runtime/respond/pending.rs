//! 待确认操作（Pending Operation）的分发处理流程。
//! 接收会话中已有的待确认待办操作，根据操作类型分发到对应处理流程。
//! 同时提供会话记录持久化和待确认状态管理的通用方法。

use crate::{
    error::LlmError,
    runtime::{
        pending::PendingOperation,
        session::{LAST_QUERY_TTL_SECONDS, SessionMeta, SessionRecord, query_is_fresh},
        tools::todo::TodoStore,
    },
};

use super::{
    RespondRequest, RespondResponse, RustRespondService,
    common::{CommandBody, command_response, session_error},
};

impl RustRespondService {
    /// 处理会话中的待确认待办操作。
    /// 如果存在跨用户的待办待确认操作，返回等待提示。
    pub(super) async fn handle_pending_operation(
        &self,
        _req: &RespondRequest,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let Some(pending) = session.pending_operation.clone() else {
            return Ok(None);
        };
        if !query_is_fresh(pending.created_at(), LAST_QUERY_TTL_SECONDS) {
            let command = match pending {
                PendingOperation::TodoClarify { .. } => "todo_clarify_expired",
                PendingOperation::TodoAdd { .. }
                | PendingOperation::TodoDone { .. }
                | PendingOperation::TodoEdit { .. }
                | PendingOperation::TodoDelete { .. }
                | PendingOperation::TodoBulkDelete { .. }
                | PendingOperation::TodoSelectCandidate { .. } => "todo_pending_expired",
            };
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                CommandBody::plain("这条待确认操作已过期，没有执行。请重新发起。"),
                command,
            )?));
        }

        // 新 pending 会保存发起人；旧持久化 pending 没有该字段时继续按历史行为兼容。
        // 一旦记录了发起人，后续确认、取消、修订和候选选择都必须来自同一个 user_id。
        if pending
            .initiator_user_id()
            .is_some_and(|initiator| meta.user_id.as_deref() != Some(initiator))
        {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                CommandBody::plain("这个操作由其他成员发起，请由发起人继续。"),
                "pending_initiator_mismatch",
            )?));
        }

        match pending {
            PendingOperation::TodoAdd { .. }
            | PendingOperation::TodoDone { .. }
            | PendingOperation::TodoEdit { .. }
            | PendingOperation::TodoDelete { .. }
            | PendingOperation::TodoBulkDelete { .. }
            | PendingOperation::TodoSelectCandidate { .. }
            | PendingOperation::TodoClarify { .. } => {
                let owner = TodoStore::owner(meta.user_id.as_deref(), &meta.scope_key);
                if pending.owner_key().is_some_and(|key| key != owner.key) {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        CommandBody::plain(
                            "当前有一条待办操作还在等待发起人确认。请先回复“确认 / 取消”，或由发起人处理完后再继续。",
                        ),
                        "todo_pending_wait",
                    )?));
                }
                self.handle_pending_todo_operation(user_text, session, &owner)
                    .await
            }
        }
    }

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
                // 确认流和管理命令可能在追加回复前已经更新 pending、记忆列表快照，
                // 或在 Todo 写操作（新增 / 软取消 / 删除）后更新了 last_todo_action、
                // 清空了 last_todo_query。这里必须把这些字段一并合并到 latest，
                // 否则数据库旧值会反向覆盖调用方刚写入的最近 Todo 状态，导致
                // “刚才那个”无法指向新待办，或“第一条”仍按旧列表解析。
                latest.state = current.state.clone();
                latest.pending_operation = current.pending_operation.clone();
                latest.last_memory_query = current.last_memory_query.clone();
                latest.last_todo_query = current.last_todo_query.clone();
                latest.last_todo_action = current.last_todo_action.clone();
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

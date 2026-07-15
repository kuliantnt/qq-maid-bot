//! Todo PreparedAction 的通用生命周期入口。
//!
//! 本模块只处理身份、owner、interaction scope、过期时间、state 与 revision，
//! 以及执行权的事务领取和 Failed 落盘；Todo payload 的具体执行留在 `pending.rs`。

use crate::{
    error::LlmError,
    runtime::{
        freshness::query_is_fresh,
        pending::{
            PendingReplyKind, PreparedActionExecutionContext, PreparedActionState, classify_reply,
        },
        respond::{
            RespondRequest, RespondResponse, RustRespondService,
            common::{CommandBody, session_error},
        },
        session::{
            LAST_QUERY_TTL_SECONDS, PendingExecutionClaim, SessionMeta, SessionRecord, now_iso_cn,
        },
        tools::{
            TaskStore,
            todo::{TODO_PENDING_DOMAIN, TodoOwner, TodoPendingPayload, todo_lexicon},
        },
    },
};

impl RustRespondService {
    /// 统一校验 Todo PreparedAction，再交给 Todo payload 状态机。
    pub(crate) async fn handle_pending_operation(
        &self,
        _req: &RespondRequest,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let Some(pending) = session.pending_operation.clone() else {
            return Ok(None);
        };
        let pending_revision = pending.revision();
        let legacy_pending = pending.is_legacy();
        if pending.domain() != TODO_PENDING_DOMAIN {
            return Ok(None);
        }

        let expired = if legacy_pending {
            !query_is_fresh(pending.created_at(), LAST_QUERY_TTL_SECONDS)
        } else {
            pending.is_expired_at(&now_iso_cn())
        };
        if expired {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                CommandBody::plain("这条待确认操作已过期，没有执行。请重新发起。"),
                TodoPendingPayload::expired_command(&pending),
            )?));
        }

        // 新动作必须绑定可信 interaction session；旧 JSON 没有该字段，只走兼容分支。
        if !legacy_pending && pending.scope_key() != Some(session.scope_key.as_str()) {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                CommandBody::plain("这条待确认操作的会话作用域已变化，没有执行。请重新发起。"),
                "pending_scope_mismatch",
            )?));
        }
        if !legacy_pending
            && (pending.initiator_user_id().is_none() || pending.owner_key().is_none())
        {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                CommandBody::plain("这条待确认操作缺少身份或所有者信息，没有执行。请重新发起。"),
                "pending_metadata_missing",
            )?));
        }

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

        let owner = TaskStore::owner(meta.user_id.as_deref(), &meta.scope_key);
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

        match pending.state() {
            PreparedActionState::WaitingConfirmation => {}
            PreparedActionState::Executing => {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    CommandBody::plain("这条操作正在执行，请勿重复确认。"),
                    "pending_executing",
                )?));
            }
            PreparedActionState::Failed => {
                if matches!(
                    classify_reply(user_text, todo_lexicon()),
                    PendingReplyKind::Cancel
                ) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        CommandBody::plain("已清理执行失败的待确认操作，请重新发起。"),
                        "pending_failed_cancel",
                    )?));
                }
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    CommandBody::plain(
                        "这条待确认操作上次执行失败，没有重复执行。请回复“取消”后重新发起。",
                    ),
                    "pending_failed",
                )?));
            }
        }

        match self
            .handle_pending_todo_operation(user_text, session, &owner)
            .await
        {
            Ok(response) => Ok(response),
            Err(err)
                if !legacy_pending
                    && session.pending_operation.as_ref().is_some_and(|pending| {
                        pending.state() == PreparedActionState::Executing
                            && pending.revision() == pending_revision
                    }) =>
            {
                Ok(Some(self.pending_execution_failed_response(
                    session,
                    user_text,
                    pending_revision,
                    false,
                    err,
                )?))
            }
            Err(err) => Err(err),
        }
    }

    /// 原子领取当前 PreparedAction 的执行权；旧 JSON 只沿用原兼容路径。
    pub(super) fn claim_todo_pending_execution(
        &self,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        revision: u64,
        legacy_pending: bool,
    ) -> Result<bool, LlmError> {
        if legacy_pending {
            return Ok(true);
        }
        let now = now_iso_cn();
        let scope_key = session.scope_key.clone();
        let context = PreparedActionExecutionContext {
            initiator_user_id: owner.user_id.as_deref(),
            owner_key: Some(owner.key.as_str()),
            scope_key: &scope_key,
            expected_revision: revision,
            now: &now,
        };
        match self
            .session_store
            .claim_pending_execution(&session.session_id, &context)
            .map_err(session_error)?
        {
            PendingExecutionClaim::Claimed(latest) => {
                *session = latest;
                Ok(true)
            }
            PendingExecutionClaim::Rejected {
                session: latest,
                error,
            } => {
                tracing::warn!(error = %error, "prepared action execution claim rejected");
                *session = latest;
                Ok(false)
            }
        }
    }

    /// 真实执行失败时保留 Failed 状态与真实错误，禁止用成功文案覆盖。
    pub(super) fn pending_execution_failed_response(
        &self,
        session: &mut SessionRecord,
        user_text: &str,
        revision: u64,
        legacy_pending: bool,
        err: LlmError,
    ) -> Result<RespondResponse, LlmError> {
        if legacy_pending {
            return Err(err);
        }
        let message = err.message.clone();
        *session = self
            .session_store
            .mark_pending_execution_failed(&session.session_id, revision)
            .map_err(session_error)?;
        self.append_pending_response(
            session,
            user_text,
            CommandBody::plain(format!(
                "这条待确认操作执行失败，没有完成。错误：{message}\n请回复“取消”后重新发起。"
            )),
            "pending_execution_failed",
        )
    }
}

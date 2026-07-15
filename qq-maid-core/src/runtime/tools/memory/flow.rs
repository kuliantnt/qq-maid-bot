//! Memory PreparedAction 的 actor-aware 确认、澄清和真实执行状态机。

use crate::{
    error::LlmError,
    runtime::{
        group_role::group_management_allowed,
        pending::{
            PendingReplyKind, PreparedActionExecutionContext, PreparedActionState, classify_reply,
        },
        respond::{
            RespondRequest, RespondResponse, RustRespondService,
            common::{CommandBody, memory_error, session_error},
        },
        session::{PendingExecutionClaim, SessionMeta, SessionRecord, now_iso_cn},
    },
};

use super::{
    MemoryActor, MemoryKind, MemoryOperations, MemoryPendingPayload, MemoryTarget,
    contains_sensitive_text, draft_confirmation_text, format_memory_saved_reply, memory_lexicon,
    memory_write_error_reply, prepare_memory_draft,
};

impl RustRespondService {
    pub(crate) async fn handle_pending_memory_lifecycle(
        &self,
        req: &RespondRequest,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let Some(pending) = session.pending_operation.clone() else {
            return Ok(None);
        };
        let Some(payload) = MemoryPendingPayload::try_from_pending(&pending).map_err(|err| {
            LlmError::new("memory_pending_invalid", err.to_string(), "memory_pending")
        })?
        else {
            return Ok(None);
        };
        if pending.is_expired_at(&now_iso_cn()) {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                "这条记忆操作已过期，没有执行。请重新发起。",
                "memory_pending_expired",
            )?));
        }
        if pending.scope_key() != session.scope_key {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                "这条记忆操作的会话作用域已变化，没有执行。请重新发起。",
                "memory_pending_scope_mismatch",
            )?));
        }
        let Some(actor) = current_memory_actor(meta, req) else {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                "这条记忆操作缺少稳定 actor，已取消。",
                "memory_pending_actor_missing",
            )?));
        };
        if pending.initiator_user_id() != Some(actor.user_id.as_str())
            || pending.owner_key() != Some(actor.personal_scope_id.as_str())
        {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                "这个记忆草稿由其他成员发起，请由发起人继续。",
                "memory_pending_actor_mismatch",
            )?));
        }
        match pending.state() {
            PreparedActionState::WaitingConfirmation => {}
            PreparedActionState::Executing => {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "这条记忆操作正在执行，请勿重复确认。",
                    "memory_pending_executing",
                )?));
            }
            PreparedActionState::Failed => {
                if matches!(
                    classify_reply(user_text, memory_lexicon()),
                    PendingReplyKind::Cancel
                ) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        "已清理执行失败的记忆操作，请重新发起。",
                        "memory_pending_failed_cancel",
                    )?));
                }
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "这条记忆操作上次执行失败，没有重复执行。请回复“取消”后重新发起。",
                    "memory_pending_failed",
                )?));
            }
        }

        let payload = match payload {
            MemoryPendingPayload::ClarifyScope {
                normalized_content,
                source_text,
                source_ref,
                ..
            } => {
                return self.handle_memory_scope_clarification(
                    user_text,
                    meta,
                    session,
                    actor,
                    normalized_content,
                    source_text,
                    source_ref,
                );
            }
            payload => payload,
        };

        match classify_reply(user_text, memory_lexicon()) {
            PendingReplyKind::Cancel => {
                return Ok(Some(self.clear_pending_response(
                    session,
                    user_text,
                    "已取消，没有修改长期记忆。",
                    "memory_pending_cancelled",
                )?));
            }
            PendingReplyKind::Revise => {
                return self.revise_pending_memory(user_text, session, actor, payload);
            }
            PendingReplyKind::Wait => {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "当前有一条记忆操作等待确认。请回复“确认”或“取消”；也可以直接回复新内容修订草稿。",
                    "memory_pending_wait",
                )?));
            }
            PendingReplyKind::Confirm => {}
        }

        let revision = pending.revision();
        if !self.claim_memory_pending_execution(session, &actor, revision)? {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                "这条记忆操作已被处理或状态已变化，没有重复执行。",
                "memory_pending_claim_rejected",
            )?));
        }
        let result = self.execute_pending_memory(&actor, payload);
        match result {
            Ok(reply) => Ok(Some(self.clear_pending_response(
                session,
                user_text,
                reply,
                "memory_pending_completed",
            )?)),
            Err(err) => {
                let message = err.message.clone();
                *session = self
                    .session_store
                    .mark_pending_execution_failed(&session.session_id, revision)
                    .map_err(session_error)?;
                Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    CommandBody::plain(format!(
                        "这条记忆操作执行失败，没有完成。错误：{message}\n请回复“取消”后重新发起。"
                    )),
                    "memory_pending_execution_failed",
                )?))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_memory_scope_clarification(
        &self,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
        actor: MemoryActor,
        normalized_content: String,
        source_text: String,
        source_ref: Option<String>,
    ) -> Result<Option<RespondResponse>, LlmError> {
        if matches!(
            classify_reply(user_text, memory_lexicon()),
            PendingReplyKind::Cancel
        ) {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                "已取消，没有创建记忆草稿。",
                "memory_scope_cancelled",
            )?));
        }
        let choice = parse_scope_choice(user_text);
        let Some(target) = choice.and_then(|kind| target_for_kind(kind, meta)) else {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                "请只选择“个人”“画像”或“群组”；回复“取消”放弃。",
                "memory_scope_clarify",
            )?));
        };
        if target.memory_kind() == MemoryKind::Group && !actor.can_manage_group_memory {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                "群组公共记忆只能由当前群的群主或管理员保存，本次草稿未提交。",
                "group_admin_required",
            )?));
        }
        let draft = prepare_memory_draft(
            target,
            normalized_content,
            source_text,
            source_ref,
            "create",
        );
        let result =
            MemoryOperations::new(self.memory_store.clone()).save(draft.into_save_request(actor));
        let (reply, command) = match result {
            Ok(result) => (
                CommandBody::plain(format_memory_saved_reply(
                    result.memory.memory_kind,
                    &result.memory.content,
                )),
                "memory_saved",
            ),
            Err(err) => (
                CommandBody::plain(memory_write_error_reply(err.code())),
                "memory_write_failed",
            ),
        };
        Ok(Some(self.clear_pending_response(
            session, user_text, reply, command,
        )?))
    }

    fn revise_pending_memory(
        &self,
        user_text: &str,
        session: &mut SessionRecord,
        actor: MemoryActor,
        payload: MemoryPendingPayload,
    ) -> Result<Option<RespondResponse>, LlmError> {
        if contains_sensitive_text(user_text) {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                "修订内容像是包含敏感信息，草稿未改变。请换一种内容或回复“取消”。",
                "memory_revision_sensitive",
            )?));
        }
        let revised = match payload {
            MemoryPendingPayload::Save { draft, .. } => {
                let draft = prepare_memory_draft(
                    draft.target,
                    user_text.trim().to_owned(),
                    draft.source_text,
                    draft.source_ref,
                    "create",
                );
                MemoryPendingPayload::Save {
                    initiator_user_id: actor.user_id,
                    owner_key: actor.personal_scope_id,
                    draft,
                    created_at: now_iso_cn(),
                }
            }
            MemoryPendingPayload::Replace {
                record_id,
                expected_updated_at,
                expected_record,
                draft,
                ..
            } => {
                let draft = prepare_memory_draft(
                    draft.target,
                    user_text.trim().to_owned(),
                    draft.source_text,
                    draft.source_ref,
                    "replace",
                );
                MemoryPendingPayload::Replace {
                    initiator_user_id: actor.user_id,
                    owner_key: actor.personal_scope_id,
                    record_id,
                    expected_updated_at,
                    expected_record,
                    draft,
                    created_at: now_iso_cn(),
                }
            }
            _ => {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "这类记忆操作不能通过文本修订，请回复“确认”或“取消”。",
                    "memory_pending_wait",
                )?));
            }
        };
        let confirmation = match &revised {
            MemoryPendingPayload::Save { draft, .. }
            | MemoryPendingPayload::Replace { draft, .. } => draft_confirmation_text(draft),
            _ => unreachable!(),
        };
        let fresh = revised.into_prepared_action(&session.scope_key);
        let current = session.pending_operation.as_mut().ok_or_else(|| {
            LlmError::new(
                "memory_pending_missing",
                "pending disappeared",
                "memory_pending",
            )
        })?;
        current
            .revise(
                fresh.payload().clone(),
                fresh.display_snapshot().clone(),
                fresh.expires_at().to_owned(),
            )
            .map_err(|err| {
                LlmError::new(
                    "memory_pending_revision_failed",
                    err.to_string(),
                    "memory_pending",
                )
            })?;
        Ok(Some(self.append_pending_response(
            session,
            user_text,
            CommandBody::plain(confirmation),
            "memory_pending_revised",
        )?))
    }

    fn claim_memory_pending_execution(
        &self,
        session: &mut SessionRecord,
        actor: &MemoryActor,
        revision: u64,
    ) -> Result<bool, LlmError> {
        let now = now_iso_cn();
        let scope_key = session.scope_key.clone();
        let context = PreparedActionExecutionContext {
            initiator_user_id: Some(actor.user_id.as_str()),
            owner_key: Some(actor.personal_scope_id.as_str()),
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
                session: latest, ..
            } => {
                *session = latest;
                Ok(false)
            }
        }
    }

    fn execute_pending_memory(
        &self,
        actor: &MemoryActor,
        payload: MemoryPendingPayload,
    ) -> Result<String, LlmError> {
        let ops = MemoryOperations::new(self.memory_store.clone());
        match payload {
            MemoryPendingPayload::Save { draft, .. } => {
                let result = ops
                    .save(draft.into_save_request(actor.clone()))
                    .map_err(memory_error)?;
                Ok(format!(
                    "已保存{}：{}",
                    kind_label(result.memory.memory_kind),
                    result.memory.content
                ))
            }
            MemoryPendingPayload::Replace {
                record_id,
                expected_record,
                draft,
                ..
            } => {
                let expected_record = expected_record.ok_or_else(memory_snapshot_missing)?;
                let result = ops
                    .replace_if_unchanged(
                        &record_id,
                        &expected_record,
                        draft.into_save_request(actor.clone()),
                    )
                    .map_err(memory_error)?;
                Ok(format!("已纠正记忆：{}", result.memory.content))
            }
            MemoryPendingPayload::Delete {
                target,
                record_id,
                expected_record,
                ..
            } => {
                let expected_record = expected_record.ok_or_else(memory_snapshot_missing)?;
                ops.delete_if_unchanged(actor, &target, &record_id, &expected_record)
                    .map_err(memory_error)?;
                Ok("已删除这条记忆。".to_owned())
            }
            MemoryPendingPayload::Clear {
                target,
                record_ids,
                scope_label,
                ..
            } => {
                let result = ops
                    .clear_if_unchanged(actor, &target, &record_ids)
                    .map_err(memory_error)?;
                Ok(format!("已清空{scope_label}中的 {} 条记忆。", result.count))
            }
            MemoryPendingPayload::SetProfileEnabled {
                target,
                enabled,
                expected_enabled,
                record_ids,
                ..
            } => {
                let result = ops
                    .set_group_profile_enabled_if_unchanged(
                        actor,
                        &target,
                        enabled,
                        expected_enabled,
                        &record_ids,
                    )
                    .map_err(memory_error)?;
                if result.enabled {
                    Ok("已重新授权当前群保存你的群内画像。".to_owned())
                } else {
                    Ok(format!(
                        "已停止当前群保存你的画像，并归档 {} 条画像记忆。",
                        result.archived_ids.len()
                    ))
                }
            }
            MemoryPendingPayload::ClarifyScope { .. } => Err(LlmError::new(
                "memory_scope_unresolved",
                "记忆范围仍未明确",
                "memory_pending",
            )),
        }
    }
}

fn memory_snapshot_missing() -> LlmError {
    LlmError::new(
        "memory_changed",
        "记忆状态已变化，请重新查看列表后操作",
        "memory_pending",
    )
}

fn current_memory_actor(meta: &SessionMeta, req: &RespondRequest) -> Option<MemoryActor> {
    MemoryActor::from_context(
        meta.user_id.clone(),
        meta.personal_scope_id(),
        meta.group_scope_id(),
        group_management_allowed(
            meta.group_id.as_deref(),
            &meta.scope_key,
            req.group_member_role.as_deref(),
        ),
    )
}

fn parse_scope_choice(text: &str) -> Option<MemoryKind> {
    let compact = text.trim().trim_matches(['。', '！', '!', '，', ',']);
    match compact {
        "个人" | "个人记忆" | "personal" => Some(MemoryKind::Personal),
        "画像" | "群画像" | "群内画像" | "当前群画像" | "profile" => {
            Some(MemoryKind::GroupProfile)
        }
        "群" | "群组" | "群记忆" | "群组记忆" | "group" => Some(MemoryKind::Group),
        _ => None,
    }
}

fn target_for_kind(kind: MemoryKind, meta: &SessionMeta) -> Option<MemoryTarget> {
    match kind {
        MemoryKind::Personal => Some(MemoryTarget::personal(meta.personal_scope_id()?)),
        MemoryKind::GroupProfile => Some(MemoryTarget::group_profile(
            meta.group_scope_id()?,
            meta.personal_scope_id()?,
        )),
        MemoryKind::Group => Some(MemoryTarget::group(meta.group_scope_id()?)),
        MemoryKind::LegacyUnassigned => None,
    }
}

fn kind_label(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Personal => "个人记忆",
        MemoryKind::GroupProfile => "当前群画像",
        MemoryKind::Group => "当前群组记忆",
        MemoryKind::LegacyUnassigned => "未归属旧记忆",
    }
}

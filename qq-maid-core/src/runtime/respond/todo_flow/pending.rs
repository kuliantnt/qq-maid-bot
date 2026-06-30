//! Todo 待确认操作状态机。
//!
//! 这里只处理已经进入 `PendingOperation::Todo*` 的确认、取消、修订和候选选择。
//! pending 类型定义仍在 `runtime/pending`，总分发仍在 `runtime/respond/pending.rs`，
//! 以保持跨业务 pending 的入口顺序不变。

use crate::{
    error::LlmError,
    runtime::{
        pending::{
            PendingOperation, PendingReplyKind, PendingTodoAction, classify_reply,
            pending_revision_failed_reply, should_parse_pending_revision, todo_lexicon,
        },
        session::{SessionRecord, now_iso_cn},
        todo::{TodoOwner, TodoStatus},
    },
};

use super::{format::*, target::parse_candidate_selection};

use crate::runtime::respond::common::CommandBody;
use crate::runtime::respond::{RespondResponse, RustRespondService, common::todo_error};

impl RustRespondService {
    /// 处理 Todo 待确认操作。
    ///
    /// 确认/取消优先于草稿修订；候选选择必须先选编号，再进入对应二次确认。
    /// 普通删除继续调用 `TodoStore::cancel*` 保持软删除语义；
    /// 已取消待办的清理会走带状态校验的物理删除路径。
    pub(in crate::runtime::respond) async fn handle_pending_todo_operation(
        &self,
        user_text: &str,
        session: &mut SessionRecord,
        owner: &TodoOwner,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let Some(pending) = session.pending_operation.clone() else {
            return Ok(None);
        };
        if pending.owner_key().is_some_and(|key| key != owner.key) {
            return Ok(None);
        }

        match pending {
            PendingOperation::TodoAdd {
                initiator_user_id,
                owner_key,
                draft,
                allow_revision,
                ..
            } => {
                let reply_kind = classify_reply(user_text, todo_lexicon());
                if matches!(reply_kind, PendingReplyKind::Cancel) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        CommandBody::plain("已取消，不新增待办。"),
                        "todo_cancel",
                    )?));
                }
                if matches!(reply_kind, PendingReplyKind::Confirm) {
                    let created = self.todo_store.create(owner, draft).map_err(todo_error)?;
                    session.remember_last_todo_action(&owner.key, &created, "created");
                    let reply = CommandBody::dual(
                        format!("已新增待办：{}", format_todo_inline(&created)),
                        format!(
                            "# 已新增待办\n\n- {}",
                            format_todo_inline_markdown(&created)
                        ),
                    );
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        reply,
                        "todo_confirm",
                    )?));
                }
                if should_parse_pending_revision(user_text) {
                    if !allow_revision {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            format_todo_pending_add_locked_waiting_reply(),
                            "todo_train_add",
                        )?));
                    }
                    return match self
                        .revise_todo_add_draft_with_llm(&draft, user_text, session)
                        .await?
                    {
                        Ok(revised) => Ok(Some(self.replace_pending_response(
                            session,
                            user_text,
                            PendingOperation::TodoAdd {
                                initiator_user_id,
                                owner_key,
                                draft: revised.clone(),
                                allow_revision: true,
                                created_at: now_iso_cn(),
                            },
                            format_todo_add_confirm(&revised),
                            "todo_add",
                        )?)),
                        Err(_) => Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            pending_revision_failed_reply(),
                            "todo_add",
                        )?)),
                    };
                }
                Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    format_todo_pending_add_waiting_reply(),
                    "todo_add",
                )?))
            }
            PendingOperation::TodoDone { item, .. } => {
                let reply_kind = classify_reply(user_text, todo_lexicon());
                if matches!(reply_kind, PendingReplyKind::Cancel) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        CommandBody::plain("已取消，不完成待办。"),
                        "todo_cancel",
                    )?));
                }
                if matches!(reply_kind, PendingReplyKind::Confirm) {
                    // 完成单条待办 + 清空 last_todo_query / 更新 last_todo_action
                    // 统一交由 ops 门面维护，避免与指令/工具侧重写同一套时序。
                    let completed = crate::runtime::todo::ops::complete_one(
                        &self.todo_store,
                        session,
                        owner,
                        &item.id,
                    )
                    .map_err(todo_error)?;
                    let reply = CommandBody::dual(
                        format!("已完成待办：{}", format_todo_inline(&completed)),
                        format!(
                            "# 已完成待办\n\n- {}",
                            format_todo_inline_markdown(&completed)
                        ),
                    );
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        reply,
                        "todo_confirm",
                    )?));
                }
                Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    format_todo_pending_done_waiting_reply(),
                    "todo_done",
                )?))
            }
            PendingOperation::TodoEdit {
                initiator_user_id,
                owner_key,
                before,
                draft,
                ..
            } => {
                let reply_kind = classify_reply(user_text, todo_lexicon());
                if matches!(reply_kind, PendingReplyKind::Cancel) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        CommandBody::plain("已取消，不修改待办。"),
                        "todo_cancel",
                    )?));
                }
                if matches!(reply_kind, PendingReplyKind::Confirm) {
                    let updated = self
                        .todo_store
                        .edit(owner, &before.id, draft)
                        .map_err(todo_error)?;
                    session.remember_last_todo_action(&owner.key, &updated, "edited");
                    let reply = format_todo_edit_result_body(&updated);
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        reply,
                        "todo_confirm",
                    )?));
                }
                if should_parse_pending_revision(user_text) {
                    return match self
                        .revise_todo_edit_draft_with_llm(&before, &draft, user_text, session)
                        .await?
                    {
                        Ok(revised) => Ok(Some(self.replace_pending_response(
                            session,
                            user_text,
                            PendingOperation::TodoEdit {
                                initiator_user_id,
                                owner_key,
                                before: before.clone(),
                                draft: revised.clone(),
                                created_at: now_iso_cn(),
                            },
                            format_todo_edit_confirm(&before, &revised),
                            "todo_edit",
                        )?)),
                        Err(_) => Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            pending_revision_failed_reply(),
                            "todo_edit",
                        )?)),
                    };
                }
                Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    format_todo_pending_edit_waiting_reply(),
                    "todo_edit",
                )?))
            }
            PendingOperation::TodoDelete { item, .. } => {
                let reply_kind = classify_reply(user_text, todo_lexicon());
                if matches!(reply_kind, PendingReplyKind::Cancel) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        CommandBody::plain("已取消，不删除待办。"),
                        "todo_cancel",
                    )?));
                }
                if matches!(reply_kind, PendingReplyKind::Confirm) {
                    let reply = match item.status {
                        TodoStatus::Pending => {
                            // 未完成待办的软删除（状态变更为已取消）+ 清空
                            // last_todo_query / 更新 last_todo_action 统一交由 ops
                            // 门面维护；已完成/已取消分支才走物理删除，不经过 ops。
                            let deleted = crate::runtime::todo::ops::cancel_one(
                                &self.todo_store,
                                session,
                                owner,
                                &item.id,
                            )
                            .map_err(todo_error)?;
                            CommandBody::dual(
                                format!("已取消待办：{}", format_todo_inline(&deleted)),
                                format!(
                                    "# 已取消待办\n\n- {}",
                                    format_todo_inline_markdown(&deleted)
                                ),
                            )
                        }
                        TodoStatus::Completed => {
                            let outcome = self
                                .todo_store
                                .delete_completed_by_ids(owner, std::slice::from_ref(&item.id))
                                .map_err(todo_error)?;
                            if outcome.deleted_count == 0 {
                                return Ok(Some(self.clear_pending_response(
                                    session,
                                    user_text,
                                    CommandBody::plain("没有可删除的已完成待办。"),
                                    "todo_confirm",
                                )?));
                            }
                            session.clear_last_todo_action_if_matches_any(
                                &owner.key,
                                std::slice::from_ref(&item.id),
                            );
                            CommandBody::dual(
                                format!("已删除待办：{}", format_todo_inline(&item)),
                                format!("# 已删除待办\n\n- {}", format_todo_inline_markdown(&item)),
                            )
                        }
                        TodoStatus::Cancelled => {
                            let outcome = self
                                .todo_store
                                .delete_cancelled_by_ids(owner, std::slice::from_ref(&item.id))
                                .map_err(todo_error)?;
                            if outcome.deleted_count == 0 {
                                return Ok(Some(self.clear_pending_response(
                                    session,
                                    user_text,
                                    CommandBody::plain("当前没有已取消待办需要删除。"),
                                    "todo_confirm",
                                )?));
                            }
                            session.clear_last_todo_action_if_matches_any(
                                &owner.key,
                                std::slice::from_ref(&item.id),
                            );
                            CommandBody::dual(
                                format!("已删除待办：{}", format_todo_inline(&item)),
                                format!("# 已删除待办\n\n- {}", format_todo_inline_markdown(&item)),
                            )
                        }
                    };
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        reply,
                        "todo_confirm",
                    )?));
                }
                Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    format_todo_pending_delete_waiting_reply(),
                    "todo_delete",
                )?))
            }
            PendingOperation::TodoBulkDelete {
                item_ids,
                matched_count,
                status,
                source_condition,
                ..
            } => {
                let reply_kind = classify_reply(user_text, todo_lexicon());
                if matches!(reply_kind, PendingReplyKind::Cancel) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        CommandBody::plain("已取消，不删除待办。"),
                        "todo_cancel",
                    )?));
                }
                if matches!(reply_kind, PendingReplyKind::Confirm) {
                    let reply = match status {
                        TodoStatus::Completed => {
                            let outcome = self
                                .todo_store
                                .delete_completed_by_ids(owner, &item_ids)
                                .map_err(todo_error)?;
                            session.clear_last_todo_action_if_matches_any(&owner.key, &item_ids);
                            let source_count = if matched_count == 0 {
                                item_ids.len()
                            } else {
                                matched_count
                            };
                            let skipped_count = source_count.saturating_sub(outcome.deleted_count);
                            format_todo_bulk_delete_result(
                                outcome.deleted_count,
                                skipped_count,
                                &source_condition,
                            )
                        }
                        TodoStatus::Cancelled => {
                            let outcome = self
                                .todo_store
                                .delete_cancelled_by_ids(owner, &item_ids)
                                .map_err(todo_error)?;
                            session.clear_last_todo_action_if_matches_any(&owner.key, &item_ids);
                            let source_count = if matched_count == 0 {
                                item_ids.len()
                            } else {
                                matched_count
                            };
                            let skipped_count = source_count.saturating_sub(outcome.deleted_count);
                            format_todo_bulk_delete_result_for_status(
                                TodoStatus::Cancelled,
                                outcome.deleted_count,
                                skipped_count,
                                &source_condition,
                                None,
                            )
                        }
                        TodoStatus::Pending => CommandBody::plain("不支持批量删除未完成待办。"),
                    };
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        reply,
                        "todo_confirm",
                    )?));
                }
                Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    format_todo_pending_bulk_delete_waiting_reply(),
                    "todo_delete",
                )?))
            }
            PendingOperation::TodoSelectCandidate {
                initiator_user_id,
                action,
                candidates,
                edit_text,
                ..
            } => {
                let reply_kind = classify_reply(user_text, todo_lexicon());
                if matches!(reply_kind, PendingReplyKind::Cancel) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        CommandBody::plain("已取消候选选择。"),
                        "todo_cancel",
                    )?));
                }
                if matches!(reply_kind, PendingReplyKind::Confirm) {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        CommandBody::plain("请先回复候选编号选择待办；选中后还会再次请你确认。"),
                        "todo_select",
                    )?));
                }
                let Some(index) = parse_candidate_selection(user_text) else {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        format_todo_pending_select_waiting_reply(),
                        "todo_select",
                    )?));
                };
                let Some(item) = candidates
                    .get(index.saturating_sub(1))
                    .filter(|_| index > 0)
                else {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        CommandBody::plain(
                            "这个编号不在候选列表里，请重新回复候选编号，或回复“取消”。",
                        ),
                        "todo_select",
                    )?));
                };
                match action {
                    PendingTodoAction::Done => Ok(Some(self.replace_pending_response(
                        session,
                        user_text,
                        PendingOperation::TodoDone {
                            initiator_user_id: initiator_user_id.clone(),
                            owner_key: owner.key.clone(),
                            item: item.clone(),
                            created_at: now_iso_cn(),
                        },
                        format_todo_done_confirm(item),
                        "todo_done",
                    )?)),
                    PendingTodoAction::Delete => {
                        let (reply, command) = self.prepare_todo_delete_operation(
                            session,
                            owner,
                            initiator_user_id.clone(),
                            item,
                            format_todo_inline(item),
                        )?;
                        Ok(Some(self.append_pending_response(
                            session, user_text, reply, command,
                        )?))
                    }
                    PendingTodoAction::Edit => {
                        let edit_text = edit_text.unwrap_or_default();
                        match self.parse_todo_edit_draft(&edit_text, item).await? {
                            Ok(draft) => Ok(Some(self.replace_pending_response(
                                session,
                                user_text,
                                PendingOperation::TodoEdit {
                                    initiator_user_id,
                                    owner_key: owner.key.clone(),
                                    before: item.clone(),
                                    draft: draft.clone(),
                                    created_at: now_iso_cn(),
                                },
                                format_todo_edit_confirm(item, &draft),
                                "todo_edit",
                            )?)),
                            Err(message) => Ok(Some(self.clear_pending_response(
                                session,
                                user_text,
                                CommandBody::plain(message),
                                "todo_edit",
                            )?)),
                        }
                    }
                }
            }
            PendingOperation::MemoryCreate { .. }
            | PendingOperation::MemoryUpdate { .. }
            | PendingOperation::MemoryDelete { .. } => Ok(None),
        }
    }
}

//! Todo 待确认操作状态机。
//!
//! 现在 Todo 写操作统一由 Tool Loop 触发；slash 写入口已移除。这里仅保留 Tool
//! 仍会产生的待确认类型：
//! - `TodoAdd`：`create_todo` 生成草稿，用户确认后写入；
//! - `TodoDelete`：`cancel_todo` 生成软取消确认；
//! - `TodoBulkDelete`：`delete_todos` 生成永久删除确认。
//!
//! 旧 slash 写流程专用的 `TodoDone` / `TodoEdit` / `TodoSelectCandidate` 变体在
//! `PendingOperation` 中保留为空壳兼容旧 session；运行时遇到会清理并提示重新用
//! 自然语言发起操作，避免旧 pending 长期卡住会话。

use crate::{
    error::LlmError,
    runtime::{
        pending::{PendingOperation, PendingReplyKind, classify_reply, todo_lexicon},
        session::SessionRecord,
        todo::{TodoOwner, TodoStatus},
    },
};

use super::format::*;

use crate::runtime::respond::common::CommandBody;
use crate::runtime::respond::{RespondResponse, RustRespondService, common::todo_error};

impl RustRespondService {
    /// 处理 Todo 待确认操作。
    ///
    /// Tool 侧只会创建新增、软取消和永久删除三类 pending；旧 slash 专用 pending
    /// 不再执行，直接清理，避免 #103 这类“澄清后返回成功但未写入”的旧状态机问题。
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
            PendingOperation::TodoAdd { draft, .. } => {
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
                // Todo 写操作改为单入口后，不再在 pending 阶段做二次 LLM 修订。
                // 这样可以避免“澄清/修订状态没落盘但回复成功”的旧链路问题。
                Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    format_todo_pending_add_waiting_reply(),
                    "todo_add",
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
                            // last_todo_query / 更新 last_todo_action 统一交由 ops 门面维护。
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
            PendingOperation::TodoDone { .. }
            | PendingOperation::TodoEdit { .. }
            | PendingOperation::TodoSelectCandidate { .. } => {
                Ok(Some(self.clear_pending_response(
                    session,
                    user_text,
                    CommandBody::plain(
                        "这条旧版待办确认流程已清理。请直接用自然语言重新发起待办操作。",
                    ),
                    "todo_pending_deprecated",
                )?))
            }
            PendingOperation::MemoryCreate { .. }
            | PendingOperation::MemoryUpdate { .. }
            | PendingOperation::MemoryDelete { .. } => Ok(None),
        }
    }
}

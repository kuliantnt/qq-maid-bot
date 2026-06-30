//! 长期记忆（Memory）的指令处理和待确认操作流程。
//! 负责解析 `/memory` 系列子命令（list/show/edit/delete）、
//! 接收 `/memory <内容>` 草稿并调用 LLM 整理成结构化记忆、
//! 以及处理创建/更新/删除记忆的待确认交互（确认、取消、修改草稿）。
//!
//! 本模块只保留主入口与 `RustRespondService` 上的流程编排，各职责拆到子模块：
//! - `command`：`/memory` 指令解析与旧版语法兼容入口；
//! - `scope`：群/个人 scope 判定、pending scope 还原与最近列表序号解析；
//! - `format`：列表、详情、创建/更新/删除确认等面向用户的回复；
//! - `draft`：LLM 草稿 JSON 提取、清洗、分类与敏感内容判断。
//!
//! 边界：长期记忆只能由明确记忆指令生成草稿，并经用户确认后写入；
//! 普通聊天不会自动写长期记忆；不改变 `/memory`、`/记忆`、`/记` 的创建/查看语义，
//! 也不改变 memory 持久化格式。

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    error::LlmError,
    runtime::{
        memory::{CreateScopedMemoryRequest, MemoryRecord, ScopedMemoryQuery, UpdateMemoryRequest},
        pending::{
            PendingMemory, PendingMemoryDelete, PendingMemoryUpdate, PendingOperation,
            PendingReplyKind, classify_reply, memory_lexicon, pending_revision_failed_reply,
            should_parse_pending_revision,
        },
        session::{SessionMeta, SessionRecord, now_iso_cn},
    },
};

use super::{
    RespondPurpose, RespondRequest, RespondResponse, RustRespondService,
    common::{clean_string, empty_respond_request, memory_error, structured_command_body},
    llm_service::{ChatService, LlmChatService},
    session_flow::build_session_context,
};

mod command;
mod draft;
mod format;
mod scope;

pub(super) use command::parse_memory_command;
// 供测试通过 `respond::memory_flow::short_memory_id` 复用截取后的记忆 ID 展示。
pub(super) use format::short_memory_id;

use command::{
    is_legacy_memory_request, memory_draft_argument, memory_scoped_argument,
    parse_memory_draft_command, parse_memory_edit_argument, parse_memory_management_command,
};
use draft::{
    append_memory_source_text, classify_memory, contains_sensitive_text,
    parse_valid_memory_draft_content,
};
use format::*;
use scope::{
    MemoryCommandScope, MemoryTarget, memory_actor, memory_command_scope, pending_delete_scope,
    pending_memory_scope, pending_update_scope, remember_memory_query, resolve_memory_target,
};

// 列表查询最多返回条数
const MEMORY_LIST_LIMIT: usize = 10;

impl RustRespondService {
    /// 处理记忆相关的用户输入主入口。
    /// 依次尝试：记忆管理子命令（/memory list 等）、记忆草稿（/memory 内容）、旧版语法。
    pub(super) async fn handle_memory_flow(
        &self,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        if let Some(command) = parse_memory_management_command(user_text) {
            let reply = self.handle_memory_management_command(&command, meta, session)?;
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                reply,
                command.action,
            )?));
        }

        if let Some(command) = parse_memory_draft_command(user_text) {
            let Some(command_scope) = memory_command_scope(&command, meta) else {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    MEMORY_GROUP_PRIVATE_REJECT_REPLY,
                    command.action,
                )?));
            };
            let draft_argument = memory_draft_argument(&command);
            let argument = draft_argument.trim();
            if argument.is_empty() {
                let (reply, action) = if command.raw_command == "zy" {
                    (
                        super::common::CommandBody::plain(MEMORY_DRAFT_LEGACY_USAGE_REPLY),
                        "memory",
                    )
                } else {
                    let records = self
                        .memory_store
                        .list_scoped(ScopedMemoryQuery {
                            scope_type: command_scope.scope_type,
                            scope_id: command_scope.scope_id.clone(),
                            limit: Some(MEMORY_LIST_LIMIT),
                            q: None,
                            scope: None,
                            memory_type: None,
                        })
                        .map_err(memory_error)?;
                    remember_memory_query(session, "list", "", &command_scope, &records);
                    (
                        structured_command_body(format_memory_list_reply(
                            &records,
                            "",
                            &command_scope,
                        )),
                        "memory_list",
                    )
                };
                return Ok(Some(
                    self.append_pending_response(session, user_text, reply, action)?,
                ));
            }
            if contains_sensitive_text(argument) {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "这段内容像是包含密钥、token 或其他敏感信息，不创建记忆草稿。",
                    "memory",
                )?));
            }

            let Some(memory) = self
                .build_pending_memory_create(argument, user_text, session, &command_scope, meta)
                .await?
            else {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "唔，这条记忆草稿没整理成功，或者内容不适合写入长期记忆。",
                    "memory",
                )?));
            };

            let reply = format_memory_create_confirm(&memory.content);
            return Ok(Some(self.replace_pending_response(
                session,
                user_text,
                PendingOperation::MemoryCreate {
                    initiator_user_id: meta.user_id.clone(),
                    memory: memory.clone(),
                },
                structured_command_body(reply),
                "memory",
            )?));
        }

        if is_legacy_memory_request(user_text) {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                MEMORY_LEGACY_HINT_REPLY,
                "memory_legacy_hint",
            )?));
        }

        Ok(None)
    }

    /// 调用 LLM 将用户输入的草稿整理成结构化的待确认记忆。
    async fn build_pending_memory_create(
        &self,
        draft_input: &str,
        source_text: &str,
        session: &SessionRecord,
        command_scope: &MemoryCommandScope,
        meta: &SessionMeta,
    ) -> Result<Option<PendingMemory>, LlmError> {
        if contains_sensitive_text(draft_input) {
            return Ok(None);
        }
        let memory_context = self.build_memory_context(meta)?;
        let session_context = build_session_context(session);
        let service = LlmChatService::new(self.provider.clone());
        let output = service
            .respond(RespondRequest {
                session_id: session.session_id.clone(),
                model: self.memory_model.clone(),
                purpose: RespondPurpose::MemoryDraft,
                user_text: draft_input.to_owned(),
                memory_context,
                session_context,
                metadata: HashMap::from([
                    ("purpose".to_owned(), "memory_draft".to_owned()),
                    ("memory_operation".to_owned(), "create".to_owned()),
                ]),
                ..empty_respond_request()
            })
            .await?;
        self.build_pending_memory_from_output(&output.reply, source_text, command_scope)
    }

    /// 从 LLM 输出中解析验证记忆内容，构造待确认记忆结构体。
    fn build_pending_memory_from_output(
        &self,
        raw_output: &str,
        source_text: &str,
        command_scope: &MemoryCommandScope,
    ) -> Result<Option<PendingMemory>, LlmError> {
        let Some(draft) = parse_valid_memory_draft_content(raw_output) else {
            return Ok(None);
        };

        let (memory_type, scope) = classify_memory(&draft);
        Ok(Some(PendingMemory {
            content: draft,
            source_text: source_text.to_owned(),
            memory_type,
            scope,
            created_at: now_iso_cn(),
            target_scope_type: Some(command_scope.scope_type.as_str().to_owned()),
            target_scope_id: Some(command_scope.scope_id.clone()),
        }))
    }

    async fn build_pending_memory_create_revision(
        &self,
        current: &PendingMemory,
        user_text: &str,
        session: &SessionRecord,
    ) -> Result<Option<PendingMemory>, LlmError> {
        let Some(content) = self
            .revise_memory_draft_content(
                "create_revise",
                Value::Null,
                json!({ "content": current.content }),
                user_text,
                session,
            )
            .await?
        else {
            return Ok(None);
        };
        let (memory_type, scope) = classify_memory(&content);
        let source_text = if content == current.content {
            current.source_text.clone()
        } else {
            append_memory_source_text(&current.source_text, user_text)
        };
        Ok(Some(PendingMemory {
            content,
            source_text,
            memory_type,
            scope,
            created_at: now_iso_cn(),
            target_scope_type: current.target_scope_type.clone(),
            target_scope_id: current.target_scope_id.clone(),
        }))
    }

    async fn build_pending_memory_update_revision(
        &self,
        current: &PendingMemoryUpdate,
        user_text: &str,
        session: &SessionRecord,
    ) -> Result<Option<PendingMemoryUpdate>, LlmError> {
        let Some(content) = self
            .revise_memory_draft_content(
                "update_revise",
                json!({
                    "before_content": current.before_content,
                    "type": current.memory_type,
                    "scope": current.scope,
                }),
                json!({
                    "content": current.content,
                    "type": current.memory_type,
                    "scope": current.scope,
                }),
                user_text,
                session,
            )
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(PendingMemoryUpdate {
            id: current.id.clone(),
            before_content: current.before_content.clone(),
            content,
            memory_type: current.memory_type.clone(),
            scope: current.scope.clone(),
            created_at: now_iso_cn(),
            target_scope_type: current.target_scope_type.clone(),
            target_scope_id: current.target_scope_id.clone(),
        }))
    }

    async fn revise_memory_draft_content(
        &self,
        operation: &str,
        original: Value,
        current_draft: Value,
        user_text: &str,
        session: &SessionRecord,
    ) -> Result<Option<String>, LlmError> {
        if contains_sensitive_text(user_text) {
            return Ok(None);
        }
        let service = LlmChatService::new(self.provider.clone());
        let output = service
            .respond(RespondRequest {
                session_id: session.session_id.clone(),
                model: self.memory_model.clone(),
                purpose: RespondPurpose::MemoryDraft,
                user_text: user_text.to_owned(),
                session: json!({
                    "operation": operation,
                    "original": original,
                    "current_draft": current_draft,
                    "user_input": user_text.trim(),
                }),
                metadata: HashMap::from([
                    ("purpose".to_owned(), "memory_draft".to_owned()),
                    ("memory_operation".to_owned(), operation.to_owned()),
                ]),
                ..empty_respond_request()
            })
            .await?;
        Ok(parse_valid_memory_draft_content(&output.reply))
    }

    /// 处理记忆相关的待确认操作：创建 / 更新 / 删除的确认、取消、修改。
    pub(super) async fn handle_pending_memory_operation(
        &self,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let Some(pending) = session.pending_operation.clone() else {
            return Ok(None);
        };

        match pending {
            PendingOperation::MemoryCreate {
                initiator_user_id,
                memory,
            } => {
                let reply_kind = classify_reply(user_text, memory_lexicon());
                if matches!(reply_kind, PendingReplyKind::Cancel) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        "已取消，不写入记忆。",
                        "memory_cancel",
                    )?));
                }
                if matches!(reply_kind, PendingReplyKind::Confirm) {
                    let Some(command_scope) = pending_memory_scope(&memory, meta) else {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            MEMORY_SCOPE_MISMATCH_REPLY,
                            "memory",
                        )?));
                    };
                    let Some(actor) = memory_actor(meta) else {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            "当前请求缺少稳定用户标识，不能写入长期记忆。",
                            "memory",
                        )?));
                    };
                    let created = self
                        .memory_store
                        .create_scoped(CreateScopedMemoryRequest {
                            scope_type: command_scope.scope_type,
                            scope_id: command_scope.scope_id,
                            created_by_user_id: actor.user_id,
                            user_id: meta.user_id.clone(),
                            group_id: meta.group_id.clone(),
                            content: memory.content,
                            source_text: memory.source_text,
                            memory_type: memory.memory_type,
                            scope: memory.scope,
                        })
                        .map_err(memory_error)?;
                    let reply = format!("已记下：{}", created.content);
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        reply,
                        "memory_confirm",
                    )?));
                }
                if should_parse_pending_revision(user_text) {
                    let Some(revised) = self
                        .build_pending_memory_create_revision(&memory, user_text, session)
                        .await?
                    else {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            pending_revision_failed_reply(),
                            "memory",
                        )?));
                    };
                    let reply = format_memory_create_confirm(&revised.content);
                    return Ok(Some(self.replace_pending_response(
                        session,
                        user_text,
                        PendingOperation::MemoryCreate {
                            initiator_user_id,
                            memory: revised.clone(),
                        },
                        structured_command_body(reply),
                        "memory",
                    )?));
                }
                let reply = format_memory_pending_create_waiting_reply();
                Ok(Some(self.append_pending_response(
                    session, user_text, reply, "memory",
                )?))
            }
            PendingOperation::MemoryUpdate {
                initiator_user_id,
                update,
            } => {
                let reply_kind = classify_reply(user_text, memory_lexicon());
                if matches!(reply_kind, PendingReplyKind::Cancel) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        "已取消，不修改记忆。",
                        "memory_cancel",
                    )?));
                }
                if matches!(reply_kind, PendingReplyKind::Confirm) {
                    let Some(command_scope) = pending_update_scope(&update, meta) else {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            MEMORY_SCOPE_MISMATCH_REPLY,
                            "memory_update",
                        )?));
                    };
                    let Some(actor) = memory_actor(meta) else {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            "当前请求缺少稳定用户标识，不能修改长期记忆。",
                            "memory_update",
                        )?));
                    };
                    let updated = self
                        .memory_store
                        .update_scoped(
                            command_scope.scope_type,
                            &command_scope.scope_id,
                            &update.id,
                            &actor,
                            UpdateMemoryRequest {
                                content: Some(update.content),
                                source_text: None,
                                memory_type: Some(update.memory_type),
                                scope: Some(update.scope),
                            },
                        )
                        .map_err(memory_error)?;
                    let reply = format!(
                        "已更新记忆 {}：{}",
                        short_memory_id(&updated.id),
                        updated.content
                    );
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        reply,
                        "memory_confirm",
                    )?));
                }
                if should_parse_pending_revision(user_text) {
                    let Some(revised) = self
                        .build_pending_memory_update_revision(&update, user_text, session)
                        .await?
                    else {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            pending_revision_failed_reply(),
                            "memory_update",
                        )?));
                    };
                    let reply = format_pending_memory_update_confirm(&revised);
                    return Ok(Some(self.replace_pending_response(
                        session,
                        user_text,
                        PendingOperation::MemoryUpdate {
                            initiator_user_id,
                            update: revised.clone(),
                        },
                        structured_command_body(reply),
                        "memory_update",
                    )?));
                }
                let reply = format_memory_pending_update_waiting_reply();
                Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    reply,
                    "memory_update",
                )?))
            }
            PendingOperation::MemoryDelete { delete, .. } => {
                let reply_kind = classify_reply(user_text, memory_lexicon());
                if matches!(reply_kind, PendingReplyKind::Cancel) {
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        "已取消，不删除记忆。",
                        "memory_cancel",
                    )?));
                }
                if matches!(reply_kind, PendingReplyKind::Confirm) {
                    let Some(command_scope) = pending_delete_scope(&delete, meta) else {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            MEMORY_SCOPE_MISMATCH_REPLY,
                            "memory_delete",
                        )?));
                    };
                    let Some(actor) = memory_actor(meta) else {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            "当前请求缺少稳定用户标识，不能删除长期记忆。",
                            "memory_delete",
                        )?));
                    };
                    let deleted = self
                        .memory_store
                        .delete_scoped(
                            command_scope.scope_type,
                            &command_scope.scope_id,
                            &delete.id,
                            &actor,
                        )
                        .map_err(memory_error)?;
                    let reply = format!("已删除记忆：{}", short_memory_id(&deleted));
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        reply,
                        "memory_confirm",
                    )?));
                }
                let reply = format_memory_pending_delete_waiting_reply();
                Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    reply,
                    "memory_delete",
                )?))
            }
            _ => Ok(None),
        }
    }

    /// 处理记忆管理子命令：list / show / edit / delete。
    fn handle_memory_management_command(
        &self,
        command: &crate::runtime::command::ParsedCommand,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<super::common::CommandBody, LlmError> {
        let Some(command_scope) = memory_command_scope(command, meta) else {
            return Ok(MEMORY_GROUP_PRIVATE_REJECT_REPLY.into());
        };
        let scoped_argument = memory_scoped_argument(command);
        let argument = scoped_argument.trim();
        match command.action.as_str() {
            "memory_list" => {
                let records = self
                    .memory_store
                    .list_scoped(ScopedMemoryQuery {
                        scope_type: command_scope.scope_type,
                        scope_id: command_scope.scope_id.clone(),
                        limit: Some(MEMORY_LIST_LIMIT),
                        q: clean_string(argument.to_owned()),
                        scope: None,
                        memory_type: None,
                    })
                    .map_err(memory_error)?;
                remember_memory_query(session, "list", argument, &command_scope, &records);
                Ok(structured_command_body(format_memory_list_reply(
                    &records,
                    argument,
                    &command_scope,
                )))
            }
            "memory_show" => {
                if argument.is_empty() {
                    return Ok("用法：/memory show 列表序号".into());
                }
                let Some(record) = self.resolve_memory_record(session, &command_scope, argument)?
                else {
                    return Ok(format_memory_no_list_index_reply(argument, &command_scope).into());
                };
                Ok(structured_command_body(format_memory_detail_reply(&record)))
            }
            "memory_edit" => {
                let Some((target, content)) = parse_memory_edit_argument(argument) else {
                    return Ok("用法：/memory edit 列表序号 新内容".into());
                };
                if contains_sensitive_text(&content) {
                    return Ok("这段内容像是包含密钥、token 或其他敏感信息，不更新记忆。".into());
                }
                let (memory_type, scope) = classify_memory(&content);
                let Some(record) = self.resolve_memory_record(session, &command_scope, &target)?
                else {
                    return Ok(format_memory_no_list_index_reply(&target, &command_scope).into());
                };
                let update = PendingMemoryUpdate {
                    id: record.id.clone(),
                    before_content: record.content.clone(),
                    content,
                    memory_type,
                    scope,
                    created_at: now_iso_cn(),
                    target_scope_type: Some(command_scope.scope_type.as_str().to_owned()),
                    target_scope_id: Some(command_scope.scope_id.clone()),
                };
                let reply = format_memory_update_confirm(&record, &update);
                session.pending_operation = Some(PendingOperation::MemoryUpdate {
                    initiator_user_id: meta.user_id.clone(),
                    update,
                });
                Ok(structured_command_body(reply))
            }
            "memory_delete" => {
                if argument.is_empty() {
                    return Ok("用法：/memory delete 列表序号".into());
                }
                let Some(record) = self.resolve_memory_record(session, &command_scope, argument)?
                else {
                    return Ok(format_memory_no_list_index_reply(argument, &command_scope).into());
                };
                session.pending_operation = Some(PendingOperation::MemoryDelete {
                    initiator_user_id: meta.user_id.clone(),
                    delete: PendingMemoryDelete {
                        id: record.id.clone(),
                        content: record.content.clone(),
                        memory_type: record.memory_type.clone(),
                        scope: record.scope.clone(),
                        created_at: now_iso_cn(),
                        target_scope_type: Some(command_scope.scope_type.as_str().to_owned()),
                        target_scope_id: Some(command_scope.scope_id.clone()),
                    },
                });
                Ok(structured_command_body(format_memory_delete_confirm(
                    &record,
                )))
            }
            "memory_update_hint" => Ok("记忆修改请使用：/memory edit 列表序号 新内容".into()),
            _ => Ok("用法：/memory list [关键词]".into()),
        }
    }

    /// 根据用户输入的字符串（ID 或列表序号）解析并获取记忆记录。
    fn resolve_memory_record(
        &self,
        session: &mut SessionRecord,
        command_scope: &MemoryCommandScope,
        target: &str,
    ) -> Result<Option<MemoryRecord>, LlmError> {
        let target = resolve_memory_target(session, command_scope, target);
        let id = match target {
            MemoryTarget::ResolvedId(id) => id,
            MemoryTarget::MissingListIndex(_) => return Ok(None),
        };
        self.memory_store
            .get_scoped(command_scope.scope_type, &command_scope.scope_id, &id)
            .map(Some)
            .map_err(memory_error)
    }
}

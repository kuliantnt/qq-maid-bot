//! 长期记忆（Memory）的指令处理和待确认操作流程。
//! 负责解析 `/memory` 系列子命令（list/show/edit/delete）、
//! 接收 `/memory <内容>` 草稿并调用 LLM 整理成结构化记忆、
//! 以及处理创建/更新/删除记忆的待确认交互（确认、取消、修改草稿）。

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    error::LlmError,
    runtime::{
        command::{ParsedCommand, parse_slash_command},
        memory::{
            CreateScopedMemoryRequest, MemoryActor, MemoryRecord, MemoryScopeType,
            ScopedMemoryQuery, UpdateMemoryRequest,
        },
        pending::{
            PendingMemory, PendingMemoryDelete, PendingMemoryUpdate, PendingOperation,
            PendingReplyKind, classify_reply, memory_lexicon, pending_revision_failed_reply,
            should_parse_pending_revision,
        },
        session::{LastMemoryQuery, SessionMeta, SessionRecord, now_iso_cn, redact_sensitive_text},
    },
};

use super::{
    RespondPurpose, RespondRequest, RespondResponse, RustRespondService,
    common::{
        LAST_QUERY_TTL_SECONDS, clean_string, empty_respond_request, extract_json_object,
        memory_error, query_is_fresh, structured_command_body, truncate_chars,
    },
    llm_service::{ChatService, LlmChatService, clean_memory_draft_output},
    session_flow::{build_session_context, datetime_for_display},
};

// 列表查询最多返回条数
const MEMORY_LIST_LIMIT: usize = 10;
// 旧版 /zy 指令的迁移提示
const MEMORY_DRAFT_LEGACY_USAGE_REPLY: &str = "/zy 仍可使用，但推荐改用：/memory 要保存的记忆内容
也可以使用：/记忆、/记";
// 非斜杠开头的"记一下"等旧版语法的提示
const MEMORY_LEGACY_HINT_REPLY: &str = "长期记忆请使用：/memory 要保存的内容
也可以使用：/记忆 要保存的内容";
const MEMORY_GROUP_PRIVATE_REJECT_REPLY: &str = "群记忆只能在群聊中查看或管理。";
const MEMORY_SCOPE_MISMATCH_REPLY: &str = "这条记忆不在当前可管理范围内。";

/// 记忆操作目标：只允许通过最近列表序号解析出的真实 ID 或无效序号。
#[derive(Debug, Clone, PartialEq, Eq)]
enum MemoryTarget {
    /// 已解析为真实记忆 ID
    ResolvedId(String),
    /// 列表序号缺失或超出范围，记录原始输入用于错误提示
    MissingListIndex(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryCommandScope {
    scope_type: MemoryScopeType,
    scope_id: String,
    label: &'static str,
    group_command: bool,
}

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
        command: &ParsedCommand,
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

/// 解析 `/memory` 草稿指令（无子命令的情况）。
fn parse_memory_draft_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_slash_command(text)?;
    (command.action == "memory").then_some(command)
}

/// 解析 `/memory` 管理子命令（list / show / edit / delete 等）。
fn parse_memory_management_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_memory_draft_command(text)?;
    let mut parts = command.argument.splitn(2, char::is_whitespace);
    let subcommand = parts.next()?.trim().to_ascii_lowercase();
    let (action, argument) = match subcommand.as_str() {
        // group/群 是显式群记忆命名空间；其后的空参数按群列表处理。
        "group" | "群" => {
            let rest = parts.next().unwrap_or("").trim();
            let mut group_parts = rest.splitn(2, char::is_whitespace);
            let group_subcommand = group_parts.next().unwrap_or("").trim().to_ascii_lowercase();
            match group_subcommand.as_str() {
                "" | "list" | "ls" | "列表" | "search" | "find" | "搜索" => (
                    "memory_list",
                    group_argument(group_parts.next().unwrap_or("").trim()),
                ),
                "add" | "新增" | "添加" => {
                    return None;
                }
                "show" | "get" | "查看" | "详情" => (
                    "memory_show",
                    group_argument(group_parts.next().unwrap_or("").trim()),
                ),
                "edit" | "set" | "修改" | "改" => (
                    "memory_edit",
                    group_argument(group_parts.next().unwrap_or("").trim()),
                ),
                "update" | "更新" => ("memory_update_hint", group_argument("")),
                "delete" | "del" | "rm" | "删除" => (
                    "memory_delete",
                    group_argument(group_parts.next().unwrap_or("").trim()),
                ),
                _ => ("memory_list", group_argument(rest)),
            }
        }
        "list" | "ls" | "列表" | "search" | "find" | "搜索" => {
            ("memory_list", parts.next().unwrap_or("").trim().to_owned())
        }
        "show" | "get" | "查看" | "详情" => {
            ("memory_show", parts.next().unwrap_or("").trim().to_owned())
        }
        "edit" | "set" | "修改" | "改" => {
            ("memory_edit", parts.next().unwrap_or("").trim().to_owned())
        }
        "update" | "更新" => (
            "memory_update_hint",
            parts.next().unwrap_or("").trim().to_owned(),
        ),
        "delete" | "del" | "rm" | "删除" => (
            "memory_delete",
            parts.next().unwrap_or("").trim().to_owned(),
        ),
        _ => return None,
    };
    Some(ParsedCommand {
        action: action.to_owned(),
        argument,
        raw_command: command.raw_command,
    })
}

fn memory_command_scope(command: &ParsedCommand, meta: &SessionMeta) -> Option<MemoryCommandScope> {
    let group_command = memory_command_targets_group(command);
    if group_command {
        let group_id = clean_string(meta.group_id.clone()?)?;
        return Some(MemoryCommandScope {
            scope_type: MemoryScopeType::Group,
            scope_id: group_id,
            label: "群",
            group_command: true,
        });
    }
    let user_id = clean_string(meta.user_id.clone()?)?;
    Some(MemoryCommandScope {
        scope_type: MemoryScopeType::Personal,
        scope_id: user_id,
        label: "个人",
        group_command: false,
    })
}

fn memory_command_targets_group(command: &ParsedCommand) -> bool {
    let argument = command.argument.trim_start();
    argument == "group"
        || argument == "群"
        || argument.starts_with("group ")
        || argument.starts_with("群 ")
}

fn memory_scoped_argument(command: &ParsedCommand) -> String {
    let argument = command.argument.trim();
    for prefix in ["group", "群"] {
        if argument == prefix {
            return String::new();
        }
        if let Some(rest) = argument.strip_prefix(&format!("{prefix} ")) {
            return rest.trim().to_owned();
        }
    }
    argument.to_owned()
}

fn memory_draft_argument(command: &ParsedCommand) -> String {
    let argument = command.argument.trim();
    for prefix in [
        "group add",
        "group 新增",
        "group 添加",
        "群 add",
        "群 新增",
        "群 添加",
    ] {
        if let Some(rest) = argument.strip_prefix(prefix) {
            return rest.trim().to_owned();
        }
    }
    argument.to_owned()
}

fn group_argument(argument: &str) -> String {
    if argument.trim().is_empty() {
        "group".to_owned()
    } else {
        format!("group {}", argument.trim())
    }
}

fn pending_memory_scope(memory: &PendingMemory, meta: &SessionMeta) -> Option<MemoryCommandScope> {
    pending_scope(
        memory.target_scope_type.as_deref(),
        memory.target_scope_id.as_deref(),
        meta,
    )
}

fn pending_update_scope(
    update: &PendingMemoryUpdate,
    meta: &SessionMeta,
) -> Option<MemoryCommandScope> {
    pending_scope(
        update.target_scope_type.as_deref(),
        update.target_scope_id.as_deref(),
        meta,
    )
}

fn pending_delete_scope(
    delete: &PendingMemoryDelete,
    meta: &SessionMeta,
) -> Option<MemoryCommandScope> {
    pending_scope(
        delete.target_scope_type.as_deref(),
        delete.target_scope_id.as_deref(),
        meta,
    )
}

fn pending_scope(
    scope_type: Option<&str>,
    scope_id: Option<&str>,
    meta: &SessionMeta,
) -> Option<MemoryCommandScope> {
    let scope_type = match scope_type? {
        "personal" => MemoryScopeType::Personal,
        "group" => MemoryScopeType::Group,
        _ => return None,
    };
    let scope_id = scope_id?.trim();
    if scope_id.is_empty() {
        return None;
    }
    match scope_type {
        MemoryScopeType::Personal if meta.user_id.as_deref() == Some(scope_id) => {
            Some(MemoryCommandScope {
                scope_type,
                scope_id: scope_id.to_owned(),
                label: "个人",
                group_command: false,
            })
        }
        MemoryScopeType::Group if meta.group_id.as_deref() == Some(scope_id) => {
            Some(MemoryCommandScope {
                scope_type,
                scope_id: scope_id.to_owned(),
                label: "群",
                group_command: true,
            })
        }
        _ => None,
    }
}

fn memory_actor(meta: &SessionMeta) -> Option<MemoryActor> {
    clean_string(meta.user_id.clone()?).map(|user_id| MemoryActor { user_id })
}

fn format_memory_list_reply(
    records: &[MemoryRecord],
    query: &str,
    command_scope: &MemoryCommandScope,
) -> String {
    if records.is_empty() {
        if query.trim().is_empty() {
            return format!("当前没有{}长期记忆。", command_scope.label);
        }
        return format!("没有找到匹配的{}长期记忆。", command_scope.label);
    }
    let mut rows = vec![format!("{}长期记忆：", command_scope.label)];
    for (index, record) in records.iter().enumerate() {
        rows.push(format!(
            "{}. {} [{}/{}] {}",
            index + 1,
            short_memory_id(&record.id),
            record.memory_type,
            record.scope,
            truncate_chars(&record.content, 80)
        ));
    }
    let prefix = if command_scope.group_command {
        "/memory group"
    } else {
        "/memory"
    };
    rows.push(format!(
        "操作：{prefix} show 1；{prefix} edit 1 新内容；{prefix} delete 1"
    ));
    rows.join("\n")
}

fn format_memory_detail_reply(record: &MemoryRecord) -> String {
    let created_at = if record.created_at.trim().is_empty() {
        &record.ts
    } else {
        &record.created_at
    };
    let mut rows = vec![
        format!("记忆 {}：", short_memory_id(&record.id)),
        format!("- 类型：{}", record.memory_type),
        format!("- 范围：{}", record.scope),
        format!("- 时间：{}", datetime_for_display(created_at)),
    ];
    if let Some(updated_at) = &record.updated_at {
        rows.push(format!("- 更新：{}", datetime_for_display(updated_at)));
    }
    rows.push(format!("- 内容：{}", record.content));
    rows.join("\n")
}

fn format_memory_create_confirm(content: &str) -> String {
    format!(
        "整理成这条记忆草稿：{}\n\n{}",
        content.trim(),
        build_memory_confirm_hint()
    )
}

fn format_memory_pending_create_waiting_reply() -> String {
    "这条记忆草稿还在等待确认。要写入请回复“确认 / 可以 / 记吧”；要调整请直接继续补充修改意见；要放弃请回复“取消 / 不记 / 算了”。"
        .to_owned()
}

fn format_memory_pending_update_waiting_reply() -> String {
    "这次记忆修改还在等待确认。要执行请回复“确认 / 可以 / 好”；要调整请直接继续补充修改意见；要放弃请回复“取消 / 不记 / 算了”。"
        .to_owned()
}

fn format_memory_pending_delete_waiting_reply() -> String {
    "这次记忆删除还在等待确认。要删除请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不记 / 算了”。"
        .to_owned()
}

fn format_memory_update_confirm(record: &MemoryRecord, update: &PendingMemoryUpdate) -> String {
    format_pending_memory_update_confirm_with_id(&short_memory_id(&record.id), update)
}

fn format_pending_memory_update_confirm(update: &PendingMemoryUpdate) -> String {
    format_pending_memory_update_confirm_with_id(&short_memory_id(&update.id), update)
}

fn format_pending_memory_update_confirm_with_id(
    memory_id: &str,
    update: &PendingMemoryUpdate,
) -> String {
    [
        format!("待确认修改记忆 {}：", memory_id),
        format!("- 原内容：{}", truncate_chars(&update.before_content, 120)),
        format!("- 新内容：{}", update.content),
        format!("- 新类型：{}", update.memory_type),
        format!("- 新范围：{}", update.scope),
        build_memory_operation_confirm_hint(),
    ]
    .join("\n")
}

fn format_memory_delete_confirm(record: &MemoryRecord) -> String {
    [
        format!("确认删除这条记忆 {}？", short_memory_id(&record.id)),
        format!("- 类型：{}", record.memory_type),
        format!("- 范围：{}", record.scope),
        format!("- 内容：{}", truncate_chars(&record.content, 120)),
        build_memory_operation_confirm_hint(),
    ]
    .join("\n")
}

fn parse_memory_edit_argument(argument: &str) -> Option<(String, String)> {
    let mut parts = argument.splitn(2, char::is_whitespace);
    let memory_id = parts.next()?.trim().to_owned();
    let content = parts.next()?.trim().to_owned();
    if memory_id.is_empty() || content.is_empty() {
        None
    } else {
        Some((memory_id, content))
    }
}

fn remember_memory_query(
    session: &mut SessionRecord,
    query_type: impl Into<String>,
    condition: impl Into<String>,
    command_scope: &MemoryCommandScope,
    records: &[MemoryRecord],
) {
    session.last_memory_query = Some(LastMemoryQuery {
        query_type: query_type.into(),
        condition: condition.into(),
        scope_type: Some(command_scope.scope_type.as_str().to_owned()),
        scope_id: Some(command_scope.scope_id.clone()),
        result_ids: records.iter().map(|record| record.id.clone()).collect(),
        created_at: now_iso_cn(),
    });
}

fn resolve_memory_target(
    session: &mut SessionRecord,
    command_scope: &MemoryCommandScope,
    target: &str,
) -> MemoryTarget {
    let target = target.split_whitespace().next().unwrap_or("").trim();
    if target.chars().all(|ch| ch.is_ascii_digit())
        && let Ok(index) = target.parse::<usize>()
        && let Some(query) = valid_last_memory_query(session, command_scope)
        && let Some(id) = query
            .result_ids
            .get(index.saturating_sub(1))
            .filter(|_| index > 0)
    {
        return MemoryTarget::ResolvedId(id.clone());
    }
    // 与 Todo 保持一致：管理命令只接受最近列表中的序号。
    // 不再把短 ID 当目标，避免 UUID 前缀全数字时和列表序号产生歧义。
    MemoryTarget::MissingListIndex(target.to_owned())
}

fn valid_last_memory_query(
    session: &mut SessionRecord,
    command_scope: &MemoryCommandScope,
) -> Option<LastMemoryQuery> {
    let query = session.last_memory_query.clone()?;
    if !matches!(query.query_type.as_str(), "list" | "search") {
        return None;
    }
    // 旧会话快照没有 scope_type/scope_id。缺字段时强制重新列表，避免跨作用域复用序号。
    if query.scope_type.as_deref() != Some(command_scope.scope_type.as_str())
        || query.scope_id.as_deref() != Some(command_scope.scope_id.as_str())
    {
        session.last_memory_query = None;
        return None;
    }
    if !query_is_fresh(&query.created_at, LAST_QUERY_TTL_SECONDS) {
        session.last_memory_query = None;
        return None;
    }
    Some(query)
}

fn format_memory_no_list_index_reply(target: &str, command_scope: &MemoryCommandScope) -> String {
    let list_command = if command_scope.group_command {
        "/memory group"
    } else {
        "/memory"
    };
    format!(
        "最近的{}记忆列表里没有第 {} 条。请先发送 {list_command} 查看列表，再使用列表序号。",
        command_scope.label,
        target.trim()
    )
}

/// 从 LLM 返回的 JSON 中提取记忆草稿的 content 字段。
fn parse_memory_draft_json_content(raw: &str) -> Option<String> {
    let value = extract_json_object(raw)?;
    let object = value.as_object()?;
    let content = object.get("content")?;
    match content {
        Value::String(value) => sanitize_memory_content(value),
        Value::Null => None,
        _ => None,
    }
}

fn parse_valid_memory_draft_content(raw: &str) -> Option<String> {
    let draft = parse_memory_draft_json_content(raw)?;
    if is_invalid_memory_draft(&draft) || contains_sensitive_text(&draft) {
        None
    } else {
        Some(draft)
    }
}

fn sanitize_memory_content(value: &str) -> Option<String> {
    if looks_like_markdown_fence(value) {
        return None;
    }
    let content = clean_memory_draft_output(value);
    if looks_like_embedded_memory_json(&content) {
        return None;
    }
    clean_string(content)
}

fn looks_like_markdown_fence(text: &str) -> bool {
    text.trim_start().starts_with("```")
}

fn looks_like_embedded_memory_json(text: &str) -> bool {
    let text = text.trim();
    text.starts_with('{') && text.contains("\"content\"")
}

/// 根据记忆草稿内容自动分类记忆类型和范围。
/// 返回 (memory_type, scope)，默认 type=note, scope=general。
fn classify_memory(text: &str) -> (String, String) {
    if text.contains("编号映射") || text.contains("已知编号列表") {
        return ("rule".to_owned(), "innerworld.member_id_mapping".to_owned());
    }
    if text.contains("前台") && (text.contains("不确定") || text.contains("询问")) {
        return ("preference".to_owned(), "front_detection".to_owned());
    }
    ("note".to_owned(), "general".to_owned())
}

fn build_memory_confirm_hint() -> String {
    "回复“确认 / 可以 / 记吧”写入长期记忆。\n回复“取消 / 不记 / 算了”放弃。".to_owned()
}

fn build_memory_operation_confirm_hint() -> String {
    "回复“确认 / 可以 / 好”执行。\n回复“取消 / 不记 / 算了”放弃。".to_owned()
}

fn is_invalid_memory_draft(text: &str) -> bool {
    matches!(text.trim(), "" | "无" | "不适合写入长期记忆" | "无法整理")
}

fn is_legacy_memory_request(text: &str) -> bool {
    let text = text.trim();
    !text.starts_with('/') && (text.starts_with("记一下") || text.contains("写入记忆"))
}

fn contains_sensitive_text(text: &str) -> bool {
    redact_sensitive_text(text) != text
}

fn append_memory_source_text(existing: &str, user_text: &str) -> String {
    let existing = existing.trim();
    let user_text = user_text.trim();
    if existing.is_empty() {
        user_text.to_owned()
    } else if user_text.is_empty() {
        existing.to_owned()
    } else {
        format!("{existing}\n{user_text}")
    }
}

pub(super) fn short_memory_id(memory_id: &str) -> String {
    memory_id.chars().take(8).collect()
}

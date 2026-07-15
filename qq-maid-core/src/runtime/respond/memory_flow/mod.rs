//! 长期记忆（Memory）的指令处理流程。
//! 负责解析 `/memory` 系列子命令（list/show/edit/delete），
//! 并接收 `/memory <内容>` 草稿调用 LLM 整理成结构化记忆后直接写入。
//!
//! 本模块只保留主入口与 `RustRespondService` 上的流程编排，各职责拆到子模块：
//! - `command`：`/memory` 指令解析与旧版语法兼容入口；
//! - `scope`：群/个人 scope 判定与最近列表序号解析；
//! - `format`：列表、详情和写入/删除等面向用户的回复；
//! - `runtime/tools/memory`：草稿规则、权限、CRUD、冲突与生命周期领域操作。
//!
//! 边界：长期记忆只能由明确记忆指令生成草稿并直接写入；普通聊天不会自动写长期记忆；
//! 不改变 `/memory`、`/记忆`、`/记` 的创建/查看语义，也不改变 memory 持久化格式。

use std::collections::HashMap;

use crate::{
    config::ChatScene,
    error::LlmError,
    runtime::{
        session::{SessionMeta, SessionRecord},
        tools::memory::{
            CreateScopedMemoryRequest, MemoryActor, MemoryError, MemoryOperations, MemoryRecord,
            ReplaceScopedMemoryRequest, ScopedMemoryQuery,
        },
    },
};

use super::{
    RespondPurpose, RespondRequest, RespondResponse, RustRespondService,
    common::{
        GROUP_ADMIN_REQUIRED_REPLY, clean_string, empty_respond_request, memory_error,
        structured_command_body,
    },
    llm_service::{ChatService, LlmChatService},
    session_flow::build_session_context,
};

mod command;
mod format;
mod scope;

pub(super) use command::parse_memory_command;
// 供测试通过 `respond::memory_flow::short_memory_id` 复用截取后的记忆 ID 展示。
pub(super) use format::short_memory_id;

use crate::runtime::tools::memory::{
    classify_memory, contains_sensitive_text, parse_valid_memory_draft_content,
};
use command::{
    is_legacy_memory_request, memory_draft_argument, memory_scoped_argument,
    parse_memory_draft_command, parse_memory_edit_argument, parse_memory_management_command,
};
use format::*;
use scope::{
    MemoryCommandScope, MemoryTarget, memory_actor, memory_command_scope, remember_memory_query,
    resolve_memory_target,
};

// 列表查询最多返回条数
const MEMORY_LIST_LIMIT: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryDraft {
    content: String,
    source_text: String,
    memory_type: String,
    scope: String,
}

impl RustRespondService {
    /// 处理记忆相关的用户输入主入口。
    /// 依次尝试：记忆管理子命令（/memory list 等）、记忆草稿（/memory 内容）、旧版语法。
    pub(super) async fn handle_memory_flow(
        &self,
        req: &RespondRequest,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        if let Some(command) = parse_memory_management_command(user_text) {
            let reply = match self.handle_memory_management_command(&command, req, meta, session) {
                Ok(reply) => reply,
                Err(err) if err.code == "forbidden" => {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        GROUP_ADMIN_REQUIRED_REPLY,
                        "group_admin_required",
                    )?));
                }
                Err(err) => return Err(err),
            };
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
                    let Some(actor) = memory_actor(meta, req) else {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            "当前请求缺少稳定用户标识，不能读取长期记忆。",
                            "memory",
                        )?));
                    };
                    let records = MemoryOperations::new(self.memory_store.clone())
                        .list_scoped(
                            &actor,
                            ScopedMemoryQuery {
                                scope_type: command_scope.scope_type,
                                scope_id: command_scope.scope_id.clone(),
                                limit: Some(MEMORY_LIST_LIMIT),
                                q: None,
                                scope: None,
                                memory_type: None,
                            },
                        )
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
            let Some(actor) = memory_actor(meta, req) else {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "当前请求缺少稳定用户标识，不能写入长期记忆。",
                    "memory",
                )?));
            };
            match MemoryOperations::new(self.memory_store.clone()).ensure_scoped_write_allowed(
                &actor,
                command_scope.scope_type,
                &command_scope.scope_id,
            ) {
                Ok(()) => {}
                Err(err) if err.code() == "forbidden" => {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        GROUP_ADMIN_REQUIRED_REPLY,
                        "group_admin_required",
                    )?));
                }
                Err(err) => return Err(memory_error(err)),
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
                .build_memory_draft(argument, user_text, session, &command_scope, meta)
                .await?
            else {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "唔，这条记忆草稿没整理成功，或者内容不适合写入长期记忆。",
                    "memory",
                )?));
            };

            let created = match self.create_memory_from_draft(memory, meta, &command_scope, actor) {
                Ok(created) => created,
                Err(err) if err.code() == "forbidden" => {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        GROUP_ADMIN_REQUIRED_REPLY,
                        "group_admin_required",
                    )?));
                }
                Err(err) => return Err(memory_error(err)),
            };
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                format!("已记下：{}", created.content),
                "memory_create",
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

    /// 调用 LLM 将用户输入整理成可直接写入的结构化记忆草稿。
    async fn build_memory_draft(
        &self,
        draft_input: &str,
        source_text: &str,
        session: &SessionRecord,
        command_scope: &MemoryCommandScope,
        meta: &SessionMeta,
    ) -> Result<Option<MemoryDraft>, LlmError> {
        if contains_sensitive_text(draft_input) {
            return Ok(None);
        }
        let memory_context = self.build_memory_context(meta)?;
        let session_context = build_session_context(session);
        let service = LlmChatService::new(self.provider.clone());
        let scene = if meta
            .group_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            ChatScene::Group
        } else {
            ChatScene::Private
        };
        let policy = self.agent_config.resolve(scene)?;
        let output = service
            .respond(RespondRequest {
                session_id: session.session_id.clone(),
                model: policy.resolve_auxiliary_model(self.memory_model.as_deref()),
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
        self.build_memory_draft_from_output(&output.reply, source_text, command_scope)
    }

    fn create_memory_from_draft(
        &self,
        memory: MemoryDraft,
        meta: &SessionMeta,
        command_scope: &MemoryCommandScope,
        actor: MemoryActor,
    ) -> Result<MemoryRecord, MemoryError> {
        MemoryOperations::new(self.memory_store.clone()).create_scoped(
            &actor,
            CreateScopedMemoryRequest {
                scope_type: command_scope.scope_type,
                scope_id: command_scope.scope_id.clone(),
                created_by_user_id: actor.personal_scope_id.clone(),
                user_id: meta.user_id.clone(),
                group_id: meta.group_id.clone(),
                content: memory.content,
                source_text: memory.source_text,
                memory_type: memory.memory_type,
                scope: memory.scope,
            },
        )
    }

    /// 从 LLM 输出中解析验证记忆内容，构造可直接写入的记忆结构体。
    fn build_memory_draft_from_output(
        &self,
        raw_output: &str,
        source_text: &str,
        _command_scope: &MemoryCommandScope,
    ) -> Result<Option<MemoryDraft>, LlmError> {
        let Some(draft) = parse_valid_memory_draft_content(raw_output) else {
            return Ok(None);
        };

        let (memory_type, scope) = classify_memory(&draft);
        Ok(Some(MemoryDraft {
            content: draft,
            source_text: source_text.to_owned(),
            memory_type,
            scope,
        }))
    }

    /// 处理记忆管理子命令：list / show / edit / delete。
    fn handle_memory_management_command(
        &self,
        command: &crate::runtime::command::ParsedCommand,
        req: &RespondRequest,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<super::common::CommandBody, LlmError> {
        let Some(command_scope) = memory_command_scope(command, meta) else {
            return Ok(MEMORY_GROUP_PRIVATE_REJECT_REPLY.into());
        };
        let Some(actor) = memory_actor(meta, req) else {
            return Ok("当前请求缺少稳定用户标识，不能管理长期记忆。".into());
        };
        let memory_ops = MemoryOperations::new(self.memory_store.clone());
        let scoped_argument = memory_scoped_argument(command);
        let argument = scoped_argument.trim();
        match command.action.as_str() {
            "memory_list" => {
                let records = memory_ops
                    .list_scoped(
                        &actor,
                        ScopedMemoryQuery {
                            scope_type: command_scope.scope_type,
                            scope_id: command_scope.scope_id.clone(),
                            limit: Some(MEMORY_LIST_LIMIT),
                            q: clean_string(argument.to_owned()),
                            scope: None,
                            memory_type: None,
                        },
                    )
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
                let Some(record) =
                    self.resolve_memory_record(session, &command_scope, argument, &actor)?
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
                let Some(record) =
                    self.resolve_memory_record(session, &command_scope, &target, &actor)?
                else {
                    return Ok(format_memory_no_list_index_reply(&target, &command_scope).into());
                };
                let old_id = record.id.clone();
                let (memory_type, scope) = classify_memory(&content);
                let created = match memory_ops.replace_scoped(ReplaceScopedMemoryRequest {
                    scope_type: command_scope.scope_type,
                    scope_id: command_scope.scope_id.clone(),
                    id_or_prefix: old_id.clone(),
                    actor: actor.clone(),
                    user_id: meta.user_id.clone(),
                    group_id: meta.group_id.clone(),
                    content,
                    source_text: format!("/{} {}", command.raw_command, command.argument)
                        .trim()
                        .to_owned(),
                    memory_type,
                    scope,
                }) {
                    Ok(created) => created,
                    Err(err) if err.code() == "forbidden" => {
                        return Err(memory_error(err));
                    }
                    Err(err) if err.code() == "not_found" => {
                        return Ok("这条记忆不在当前可管理范围内。".into());
                    }
                    Err(err) => return Err(memory_error(err)),
                };
                Ok(structured_command_body(format!(
                    "已替换记忆 {}：{}",
                    short_memory_id(&old_id),
                    created.content
                )))
            }
            "memory_delete" => {
                if argument.is_empty() {
                    return Ok("用法：/memory delete 列表序号".into());
                }
                let Some(record) =
                    self.resolve_memory_record(session, &command_scope, argument, &actor)?
                else {
                    return Ok(format_memory_no_list_index_reply(argument, &command_scope).into());
                };
                let deleted = match memory_ops.delete_scoped(
                    &actor,
                    command_scope.scope_type,
                    &command_scope.scope_id,
                    &record.id,
                ) {
                    Ok(deleted) => deleted,
                    Err(err) if err.code() == "forbidden" => {
                        return Err(memory_error(err));
                    }
                    Err(err) if err.code() == "not_found" => {
                        return Ok("这条记忆不在当前可管理范围内。".into());
                    }
                    Err(err) => return Err(memory_error(err)),
                };
                Ok(structured_command_body(format!(
                    "已删除记忆：{}",
                    short_memory_id(&deleted)
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
        actor: &MemoryActor,
    ) -> Result<Option<MemoryRecord>, LlmError> {
        let target = resolve_memory_target(session, command_scope, target);
        let id = match target {
            MemoryTarget::ResolvedId(id) => id,
            MemoryTarget::MissingListIndex(_) => return Ok(None),
        };
        MemoryOperations::new(self.memory_store.clone())
            .get_scoped(
                actor,
                command_scope.scope_type,
                &command_scope.scope_id,
                &id,
            )
            .map(Some)
            .map_err(memory_error)
    }
}

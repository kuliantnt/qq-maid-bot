//! 长期记忆的分域规范化、直接新增与命令管理编排。
//!
//! Respond 只负责识别命令、调用规范化模型、保存破坏性操作 PreparedAction 和渲染结果；
//! target 授权、真实写入、清空、opt-out 与冲突规则统一由 `runtime/tools/memory` 执行。

use std::collections::HashMap;

use qq_maid_common::identity_context::ConversationKind;

use crate::{
    config::ChatScene,
    error::LlmError,
    runtime::{
        session::{SessionMeta, SessionRecord, now_iso_cn},
        tools::memory::{
            MemoryActor, MemoryKind, MemoryOperations, MemoryPendingPayload, MemoryQuery,
            MemoryRecord, draft_confirmation_text, format_memory_saved_reply,
            memory_write_error_reply, prepare_memory_draft,
        },
    },
};

use super::{
    RespondPurpose, RespondRequest, RespondResponse, RustRespondService,
    common::{
        CommandBody, GROUP_ADMIN_REQUIRED_REPLY, clean_string, empty_respond_request, memory_error,
        structured_command_body, truncate_chars,
    },
    llm_service::{ChatService, LlmChatService},
    session_flow::build_session_context,
};

mod command;
mod format;
mod scope;

pub(super) use command::parse_memory_command;
#[cfg(test)]
pub(super) use format::short_memory_id;

use crate::runtime::tools::memory::{contains_sensitive_text, parse_valid_memory_draft_content};
use command::{
    MemoryNamespace, is_legacy_memory_request, memory_draft_argument, memory_namespace,
    memory_scoped_argument, parse_memory_draft_command, parse_memory_edit_argument,
    parse_memory_management_command,
};
use format::*;
use scope::{
    MemoryCommandScope, MemoryTargetResolution, infer_group_memory_namespace, memory_actor,
    memory_command_scope, memory_scope_for_namespace, remember_memory_query, resolve_memory_target,
};

const MEMORY_LIST_LIMIT: usize = 10;
const MEMORY_CHANNEL_EXPLICIT_PERSONAL_REPLY: &str = "当前频道暂不支持画像或群组记忆。请显式使用 `/memory personal 内容` 保存个人记忆，本次未创建草稿。";
const MEMORY_UNKNOWN_SCOPE_REPLY: &str = "当前会话类型无法确认，不能自动选择记忆范围。请显式使用 `/memory personal 内容` 保存个人记忆，本次未创建草稿。";

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedMemoryDraft {
    content: String,
    source_text: String,
}

impl RustRespondService {
    pub(super) async fn handle_memory_flow(
        &self,
        req: &RespondRequest,
        user_text: &str,
        meta: &SessionMeta,
        session: &mut SessionRecord,
    ) -> Result<Option<RespondResponse>, LlmError> {
        if let Some(command) = parse_memory_management_command(user_text) {
            let reply = match self
                .handle_memory_management_command(&command, req, meta, session, user_text)
            {
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
            let argument = memory_draft_argument(&command);
            if argument.trim().is_empty() {
                let reply = if command.raw_command == "zy" {
                    CommandBody::plain(MEMORY_DRAFT_LEGACY_USAGE_REPLY)
                } else {
                    CommandBody::plain(
                        "🧠 记忆用法\n\n- `/memory 内容`：保存个人记忆\n- `/memory personal 内容`：保存个人记忆\n- `/memory profile 内容`：保存当前群画像\n- `/memory group 内容`：保存当前群公共记忆\n- `/memory list`：查看列表",
                    )
                };
                return Ok(Some(
                    self.append_pending_response(session, user_text, reply, "memory")?,
                ));
            }
            let namespace = match resolve_memory_draft_namespace(
                memory_namespace(&command),
                req,
                meta,
                &argument,
            ) {
                Ok(namespace) => namespace,
                Err(reply) => {
                    return Ok(Some(self.append_pending_response(
                        session,
                        user_text,
                        reply,
                        "memory_scope_explicit_required",
                    )?));
                }
            };
            let Some(actor) = memory_actor(meta, req) else {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "当前请求缺少稳定用户标识，不能创建长期记忆草稿。",
                    "memory",
                )?));
            };
            if contains_sensitive_text(&argument) {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "这段内容像是包含身份证件、账号凭据、密钥、token 或其他敏感信息，不创建可提交草稿。",
                    "memory_sensitive_rejected",
                )?));
            }

            // 显式群组命名空间先做管理员门禁，避免无权限请求仍触发草稿模型。
            if memory_namespace(&command) == Some(MemoryNamespace::Group)
                && !actor.can_manage_group_memory
            {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    GROUP_ADMIN_REQUIRED_REPLY,
                    "group_admin_required",
                )?));
            }

            let Some(normalized) = self
                .build_memory_draft(&argument, user_text, session, meta)
                .await?
            else {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "唔，这条记忆草稿没整理成功，或者内容不适合写入长期记忆。",
                    "memory",
                )?));
            };

            let Some(namespace) = namespace else {
                session.pending_operation = Some(
                    MemoryPendingPayload::ClarifyScope {
                        initiator_user_id: actor.user_id.clone(),
                        owner_key: actor.personal_scope_id.clone(),
                        normalized_content: normalized.content,
                        source_text: normalized.source_text,
                        source_ref: safe_source_ref(req),
                        created_at: now_iso_cn(),
                    }
                    .into_prepared_action(&session.scope_key),
                );
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    "这条内容在群聊中的保存范围不够明确。请回复“个人”“画像”或“群组”；回复“取消”放弃。",
                    "memory_scope_clarify",
                )?));
            };
            let Some(command_scope) = memory_scope_for_namespace(namespace, meta) else {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    MEMORY_GROUP_PRIVATE_REJECT_REPLY,
                    "memory_scope_invalid",
                )?));
            };
            if command_scope.kind() == MemoryKind::Group && !actor.can_manage_group_memory {
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    GROUP_ADMIN_REQUIRED_REPLY,
                    "group_admin_required",
                )?));
            }
            let draft = prepare_memory_draft(
                command_scope.target,
                normalized.content,
                normalized.source_text,
                safe_source_ref(req),
                "create",
            );
            let result = MemoryOperations::new(self.memory_store.clone())
                .save(draft.into_save_request(actor));
            let (reply, command) = match result {
                Ok(result) => (
                    structured_command_body(format_memory_saved_reply(
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
            return Ok(Some(
                self.append_pending_response(session, user_text, reply, command)?,
            ));
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

    async fn build_memory_draft(
        &self,
        draft_input: &str,
        source_text: &str,
        session: &SessionRecord,
        meta: &SessionMeta,
    ) -> Result<Option<NormalizedMemoryDraft>, LlmError> {
        if contains_sensitive_text(draft_input) {
            return Ok(None);
        }
        let memory_context = self.build_memory_context(meta)?;
        let session_context = build_session_context(session);
        let service = LlmChatService::new(self.provider.clone());
        let scene = if meta.group_scope_id().is_some() {
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
        let Some(content) = parse_valid_memory_draft_content(&output.reply) else {
            return Ok(None);
        };
        Ok(Some(NormalizedMemoryDraft {
            content,
            source_text: source_text.to_owned(),
        }))
    }

    fn handle_memory_management_command(
        &self,
        command: &crate::runtime::command::ParsedCommand,
        req: &RespondRequest,
        meta: &SessionMeta,
        session: &mut SessionRecord,
        user_text: &str,
    ) -> Result<CommandBody, LlmError> {
        if channel_like_conversation(req, meta)
            && matches!(
                memory_namespace(command),
                Some(MemoryNamespace::GroupProfile | MemoryNamespace::Group)
            )
        {
            return Ok(CommandBody::plain(MEMORY_CHANNEL_EXPLICIT_PERSONAL_REPLY));
        }
        if personal_management_is_blocked_in_shared_conversation(command, req, meta) {
            return Ok(CommandBody::plain(
                "共享会话不会展示或管理历史个人记忆。请前往私聊使用 /memory；当前会话可使用 /memory profile 或 /memory group。",
            ));
        }
        let Some(command_scope) = memory_command_scope(command, meta) else {
            return Ok(MEMORY_GROUP_PRIVATE_REJECT_REPLY.into());
        };
        let Some(actor) = memory_actor(meta, req) else {
            return Ok("当前请求缺少稳定用户标识，不能管理长期记忆。".into());
        };
        let memory_ops = MemoryOperations::new(self.memory_store.clone());
        let argument = memory_scoped_argument(command);
        let argument = argument.trim();
        match command.action.as_str() {
            "memory_list" => {
                let mut query = MemoryQuery::active(command_scope.target.clone());
                if !argument.is_empty() {
                    query.q = Some(argument.to_owned());
                }
                query.limit = Some(MEMORY_LIST_LIMIT);
                let records = memory_ops.list(&actor, query).map_err(memory_error)?;
                remember_memory_query(
                    session,
                    &actor,
                    if argument.is_empty() {
                        "list"
                    } else {
                        "search"
                    },
                    argument,
                    &command_scope,
                    &records,
                );
                Ok(structured_command_body(format_memory_list_reply(
                    &records,
                    argument,
                    &command_scope,
                )))
            }
            "memory_show" => {
                if argument.is_empty() {
                    return Ok(
                        format!("用法：{} show 列表序号", command_scope.command_prefix).into(),
                    );
                }
                let Some(record) =
                    self.resolve_memory_record(session, &command_scope, argument, &actor)?
                else {
                    return Ok(format_memory_no_list_index_reply(argument, &command_scope).into());
                };
                Ok(structured_command_body(format_memory_detail_reply(&record)))
            }
            "memory_edit" => {
                ensure_management_write_allowed(&command_scope, &actor)?;
                let Some((target, content)) = parse_memory_edit_argument(argument) else {
                    return Ok(format!(
                        "用法：{} edit 列表序号 新内容",
                        command_scope.command_prefix
                    )
                    .into());
                };
                if contains_sensitive_text(&content) {
                    return Ok("这段内容像是包含敏感信息，不创建修改草稿。".into());
                }
                let Some(record) =
                    self.resolve_memory_record(session, &command_scope, &target, &actor)?
                else {
                    return Ok(format_memory_no_list_index_reply(&target, &command_scope).into());
                };
                let draft = prepare_memory_draft(
                    command_scope.target.clone(),
                    content.trim().to_owned(),
                    user_text.to_owned(),
                    safe_source_ref(req),
                    "replace",
                );
                session.pending_operation = Some(
                    MemoryPendingPayload::Replace {
                        initiator_user_id: actor.user_id,
                        owner_key: actor.personal_scope_id,
                        record_id: record.id.clone(),
                        expected_updated_at: record.updated_at.clone(),
                        expected_record: Some(Box::new(record)),
                        draft: draft.clone(),
                        created_at: now_iso_cn(),
                    }
                    .into_prepared_action(&session.scope_key),
                );
                Ok(structured_command_body(draft_confirmation_text(&draft)))
            }
            "memory_delete" => {
                ensure_management_write_allowed(&command_scope, &actor)?;
                if argument.is_empty() {
                    return Ok(
                        format!("用法：{} delete 列表序号", command_scope.command_prefix).into(),
                    );
                }
                let Some(record) =
                    self.resolve_memory_record(session, &command_scope, argument, &actor)?
                else {
                    return Ok(format_memory_no_list_index_reply(argument, &command_scope).into());
                };
                let content_snapshot = truncate_chars(&record.content, 80);
                session.pending_operation = Some(
                    MemoryPendingPayload::Delete {
                        initiator_user_id: actor.user_id,
                        owner_key: actor.personal_scope_id,
                        target: command_scope.target,
                        record_id: record.id.clone(),
                        expected_updated_at: record.updated_at.clone(),
                        expected_record: Some(Box::new(record)),
                        content_snapshot: content_snapshot.clone(),
                        created_at: now_iso_cn(),
                    }
                    .into_prepared_action(&session.scope_key),
                );
                Ok(CommandBody::plain(format!(
                    "待删除：{content_snapshot}\n回复“确认”执行删除；回复“取消”放弃。"
                )))
            }
            "memory_clear" => {
                ensure_management_write_allowed(&command_scope, &actor)?;
                let record_ids = memory_ops
                    .list_active_ids(&actor, &command_scope.target)
                    .map_err(memory_error)?;
                if record_ids.is_empty() {
                    return Ok(format!("当前没有{}可清空。", command_scope.label).into());
                }
                session.pending_operation = Some(
                    MemoryPendingPayload::Clear {
                        initiator_user_id: actor.user_id,
                        owner_key: actor.personal_scope_id,
                        target: command_scope.target,
                        record_ids: record_ids.clone(),
                        scope_label: command_scope.label.to_owned(),
                        created_at: now_iso_cn(),
                    }
                    .into_prepared_action(&session.scope_key),
                );
                Ok(CommandBody::plain(format!(
                    "将清空{}中的 {} 条 active 记忆。回复“确认”执行；回复“取消”放弃。",
                    command_scope.label,
                    record_ids.len()
                )))
            }
            "memory_profile_disable" | "memory_profile_enable" => {
                let enabled = command.action == "memory_profile_enable";
                let (expected_enabled, record_ids) = memory_ops
                    .group_profile_snapshot(&actor, &command_scope.target)
                    .map_err(memory_error)?;
                session.pending_operation = Some(
                    MemoryPendingPayload::SetProfileEnabled {
                        initiator_user_id: actor.user_id,
                        owner_key: actor.personal_scope_id,
                        target: command_scope.target,
                        enabled,
                        expected_enabled,
                        record_ids: record_ids.clone(),
                        created_at: now_iso_cn(),
                    }
                    .into_prepared_action(&session.scope_key),
                );
                let text = if enabled {
                    "将重新授权当前群保存你的群内画像。回复“确认”授权；回复“取消”放弃。".to_owned()
                } else {
                    format!(
                        "将停止当前群继续保存你的画像，并归档当前 {} 条画像记忆。回复“确认”执行；回复“取消”放弃。",
                        record_ids.len()
                    )
                };
                Ok(CommandBody::plain(text))
            }
            "memory_update_hint" => Ok(format!(
                "记忆修改请使用：{} edit 列表序号 新内容",
                command_scope.command_prefix
            )
            .into()),
            _ => Ok("用法：/memory [personal|profile|group] list [关键词]".into()),
        }
    }

    fn resolve_memory_record(
        &self,
        session: &mut SessionRecord,
        command_scope: &MemoryCommandScope,
        target: &str,
        actor: &MemoryActor,
    ) -> Result<Option<MemoryRecord>, LlmError> {
        let target = resolve_memory_target(session, command_scope, actor, target);
        let id = match target {
            MemoryTargetResolution::ResolvedId(id) => id,
            MemoryTargetResolution::MissingListIndex(_) => return Ok(None),
        };
        match MemoryOperations::new(self.memory_store.clone()).get(
            actor,
            &command_scope.target,
            &id,
        ) {
            Ok(record) => Ok(Some(record)),
            Err(err) if err.code() == "not_found" => Ok(None),
            Err(err) => Err(memory_error(err)),
        }
    }
}

fn personal_management_is_blocked_in_shared_conversation(
    command: &crate::runtime::command::ParsedCommand,
    req: &RespondRequest,
    meta: &SessionMeta,
) -> bool {
    let namespace = memory_namespace(command).unwrap_or(MemoryNamespace::Personal);
    namespace == MemoryNamespace::Personal && shared_conversation(req, meta)
}

fn shared_conversation(req: &RespondRequest, meta: &SessionMeta) -> bool {
    match req.conversation_kind {
        ConversationKind::Group | ConversationKind::Channel => true,
        ConversationKind::Private | ConversationKind::ServiceAccount => false,
        ConversationKind::Unknown => {
            meta.group_id.is_some() || meta.guild_id.is_some() || meta.channel_id.is_some()
        }
    }
}

/// 裸写入只能由权威会话类型决定默认范围；Unknown 仅在存在明确群元信息时复用群聊推断。
fn resolve_memory_draft_namespace(
    explicit_namespace: Option<MemoryNamespace>,
    req: &RespondRequest,
    meta: &SessionMeta,
    argument: &str,
) -> Result<Option<MemoryNamespace>, &'static str> {
    if let Some(namespace) = explicit_namespace {
        if channel_like_conversation(req, meta) && namespace != MemoryNamespace::Personal {
            return Err(MEMORY_CHANNEL_EXPLICIT_PERSONAL_REPLY);
        }
        return Ok(Some(namespace));
    }

    match req.conversation_kind {
        ConversationKind::Private | ConversationKind::ServiceAccount => {
            Ok(Some(MemoryNamespace::Personal))
        }
        ConversationKind::Group => Ok(infer_group_memory_namespace(argument)),
        ConversationKind::Channel => Err(MEMORY_CHANNEL_EXPLICIT_PERSONAL_REPLY),
        ConversationKind::Unknown if has_group_metadata(meta) => {
            Ok(infer_group_memory_namespace(argument))
        }
        ConversationKind::Unknown if has_channel_metadata(meta) => {
            Err(MEMORY_CHANNEL_EXPLICIT_PERSONAL_REPLY)
        }
        ConversationKind::Unknown => Err(MEMORY_UNKNOWN_SCOPE_REPLY),
    }
}

fn channel_like_conversation(req: &RespondRequest, meta: &SessionMeta) -> bool {
    req.conversation_kind == ConversationKind::Channel
        || (req.conversation_kind == ConversationKind::Unknown
            && !has_group_metadata(meta)
            && has_channel_metadata(meta))
}

fn has_group_metadata(meta: &SessionMeta) -> bool {
    meta.group_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

fn has_channel_metadata(meta: &SessionMeta) -> bool {
    [meta.guild_id.as_deref(), meta.channel_id.as_deref()]
        .into_iter()
        .flatten()
        .any(|value| !value.trim().is_empty())
}

fn ensure_management_write_allowed(
    scope: &MemoryCommandScope,
    actor: &MemoryActor,
) -> Result<(), LlmError> {
    if scope.kind() == MemoryKind::Group && !actor.can_manage_group_memory {
        return Err(LlmError::new(
            "forbidden",
            "group memory management requires admin role",
            "memory",
        ));
    }
    Ok(())
}

fn safe_source_ref(req: &RespondRequest) -> Option<String> {
    req.message_id
        .clone()
        .and_then(clean_string)
        .filter(|value| {
            value.len() <= 200
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.')
                })
        })
}

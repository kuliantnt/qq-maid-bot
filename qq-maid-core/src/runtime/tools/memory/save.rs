//! `save_memory` 自然语言长期记忆写入 Tool。

use async_trait::async_trait;
use qq_maid_common::identity_context::ConversationKind;
use qq_maid_llm::tool::{Tool, ToolContext, ToolEffect, ToolMetadata, ToolOutput};
use serde_json::{Value, json};

use crate::{
    error::LlmError,
    runtime::{
        group_role::group_management_allowed,
        session::{SessionMeta, SessionStore, now_iso_cn},
    },
};

use super::{
    MemoryActor, MemoryKind, MemoryOperations, MemoryPendingPayload, MemoryStore, MemoryTarget,
    SAVE_MEMORY_TOOL_NAME, contains_sensitive_text, infer_group_memory_kind, memory_kind_label,
    memory_write_error_reply, normalize_explicit_memory_content, prepare_memory_draft,
    route::is_memory_write_explicitly_negated,
};

#[derive(Clone)]
pub struct SaveMemoryTool {
    memory_store: MemoryStore,
    session_store: SessionStore,
    source_text: Option<String>,
}

impl SaveMemoryTool {
    pub fn new(memory_store: MemoryStore, session_store: SessionStore) -> Self {
        Self {
            memory_store,
            session_store,
            source_text: None,
        }
    }

    pub(crate) fn scoped_for_request(&self, source_text: impl Into<String>) -> Self {
        Self {
            memory_store: self.memory_store.clone(),
            session_store: self.session_store.clone(),
            source_text: Some(source_text.into()),
        }
    }
}

#[async_trait]
impl Tool for SaveMemoryTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: SAVE_MEMORY_TOOL_NAME.to_owned(),
            description: "把用户语义上明确要求长期保存的个人偏好、称呼或群内画像直接写入 Memory。群公共约定和群规则不得调用本工具，必须提示用户使用 `/memory` 管理；普通陈述、聊天事实、临时行程、模型自行推断的信息也不得调用。content 只放用户要求保存的内容。scope 用于表达建议范围，并与服务端根据原始消息得到的范围证据交叉校验；用户身份、群和最终范围由服务端上下文决定。新增成功后立即写入，不需要二次确认。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "用户明确要求长期记住的规范化内容，不得补充用户未表达的事实。"
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["auto", "personal", "profile"],
                        "description": "建议范围：personal=个人记忆，profile=当前用户群内画像，无法确定时用 auto。群公共记忆不属于本工具能力。群聊中建议范围必须与服务端从原始消息得到的范围证据一致，否则会要求澄清。"
                    }
                },
                "required": ["content", "scope"],
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::SideEffecting
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let suggested = arguments
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("auto");
        if suggested == "group" {
            return Ok(failure_output(
                "group_memory_command_only",
                memory_write_error_reply("group_memory_command_only"),
            ));
        }
        let source_text = self.source_text.as_deref().unwrap_or_default().trim();
        if source_text.is_empty() || is_memory_write_explicitly_negated(source_text) {
            return Ok(failure_output(
                "memory_intent_required",
                "当前消息没有明确要求写入长期记忆，本次未保存。",
            ));
        }
        if context.conversation.kind == ConversationKind::Group
            && infer_group_memory_kind(source_text) == Some(MemoryKind::Group)
        {
            return Ok(failure_output(
                "group_memory_command_only",
                memory_write_error_reply("group_memory_command_only"),
            ));
        }
        let raw_content = arguments
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if contains_sensitive_text(source_text) || contains_sensitive_text(raw_content) {
            return Ok(failure_output(
                "memory_sensitive_rejected",
                memory_write_error_reply("memory_sensitive_rejected"),
            ));
        }
        let content = normalize_explicit_memory_content(raw_content);
        let Some(content) = content else {
            return Ok(failure_output(
                "bad_tool_arguments",
                "记忆内容为空或格式不受支持，本次未保存。",
            ));
        };
        let Some((actor, meta)) = actor_and_meta(&context) else {
            return Ok(failure_output(
                "memory_actor_missing",
                memory_write_error_reply("memory_actor_missing"),
            ));
        };
        let target = match resolve_target(&context, &actor, source_text, suggested) {
            Ok(Some(target)) => target,
            Ok(None) => {
                return self.prepare_scope_clarification(
                    &context,
                    &actor,
                    &meta,
                    content,
                    source_text,
                );
            }
            Err(code) => return Ok(failure_output(code, memory_write_error_reply(code))),
        };
        let draft = prepare_memory_draft(
            target,
            content,
            source_text.to_owned(),
            safe_source_ref(&context),
            "create",
        );
        match MemoryOperations::new(self.memory_store.clone()).save(draft.into_save_request(actor))
        {
            Ok(result) => Ok(ToolOutput::json(json!({
                "ok": true,
                "scope": result.memory.memory_kind.as_str(),
                "scope_label": memory_kind_label(result.memory.memory_kind),
                "content": result.memory.content,
                "archived_conflicts": result.archived_ids.len(),
            }))),
            Err(err) => Ok(failure_output(
                err.code(),
                memory_write_error_reply(err.code()),
            )),
        }
    }
}

impl SaveMemoryTool {
    fn prepare_scope_clarification(
        &self,
        context: &ToolContext,
        actor: &MemoryActor,
        meta: &SessionMeta,
        content: String,
        source_text: &str,
    ) -> Result<ToolOutput, LlmError> {
        if context.conversation.kind != ConversationKind::Group {
            return Ok(failure_output(
                "memory_scope_unsupported",
                memory_write_error_reply("memory_scope_unsupported"),
            ));
        }
        let mut session = self
            .session_store
            .get_or_create_active(meta)
            .map_err(memory_session_error)?;
        if session.pending_operation.is_some() {
            return Ok(failure_output(
                "memory_pending_conflict",
                memory_write_error_reply("memory_pending_conflict"),
            ));
        }
        session.pending_operation = Some(
            MemoryPendingPayload::ClarifyScope {
                initiator_user_id: actor.user_id.clone(),
                owner_key: actor.personal_scope_id.clone(),
                normalized_content: content,
                source_text: source_text.to_owned(),
                source_ref: safe_source_ref(context),
                created_at: now_iso_cn(),
            }
            .into_prepared_action(&session.scope_key),
        );
        self.session_store
            .save(&mut session)
            .map_err(memory_session_error)?;
        Ok(ToolOutput::json(json!({
            "ok": false,
            "requires_clarification": true,
            "error_code": "memory_scope_ambiguous",
            "question": "这条记忆是个人记忆，还是当前群画像？请回复“个人”或“画像”。"
        })))
    }
}

fn actor_and_meta(context: &ToolContext) -> Option<(MemoryActor, SessionMeta)> {
    let user_id = context
        .actor
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    let group_id = (context.conversation.kind == ConversationKind::Group)
        .then(|| context.conversation.target_id.clone())
        .flatten();
    let interaction_scope_id = if context.conversation.kind == ConversationKind::Group
        && context.conversation.interaction_scope_id == context.conversation.scope_id
        && crate::identity::parse_stable_scope_key(&context.conversation.scope_id).is_some()
    {
        crate::identity::interaction_scope_key(Some(&user_id), &context.conversation.scope_id)
    } else {
        context.conversation.interaction_scope_id.clone()
    };
    let scope_meta = SessionMeta::new_with_account(
        context.conversation.scope_id.clone(),
        Some(user_id.clone()),
        group_id.clone(),
        None,
        None,
        context.conversation.platform.clone(),
        context.conversation.account_id.clone(),
    );
    let actor = MemoryActor::from_context(
        Some(user_id.clone()),
        scope_meta.personal_scope_id(),
        scope_meta.group_scope_id(),
        group_management_allowed(
            group_id.as_deref(),
            &context.conversation.scope_id,
            context.actor.group_member_role.as_deref(),
        ),
    )?;
    let interaction_meta = SessionMeta::new_with_account(
        interaction_scope_id,
        Some(user_id),
        group_id,
        None,
        None,
        context.conversation.platform.clone(),
        context.conversation.account_id.clone(),
    );
    Some((actor, interaction_meta))
}

fn resolve_target(
    context: &ToolContext,
    actor: &MemoryActor,
    source_text: &str,
    suggested: &str,
) -> Result<Option<MemoryTarget>, &'static str> {
    match context.conversation.kind {
        ConversationKind::Private | ConversationKind::ServiceAccount => {
            if !matches!(suggested, "auto" | "personal") {
                return Err("memory_scope_unsupported");
            }
            Ok(Some(MemoryTarget::personal(
                actor.personal_scope_id.clone(),
            )))
        }
        ConversationKind::Group => {
            let server_kind = infer_group_memory_kind(source_text);
            let suggested_kind = match suggested {
                "personal" => Some(MemoryKind::Personal),
                "profile" => Some(MemoryKind::GroupProfile),
                "group" => return Err("group_memory_command_only"),
                "auto" => None,
                _ => return Err("bad_tool_arguments"),
            };
            // scope 是模型建议，不可单独决定群聊写入目标；它必须与服务端原文证据一致。
            let Some(kind) = server_kind else {
                return Ok(None);
            };
            if suggested_kind.is_some_and(|suggested| suggested != kind) {
                return Ok(None);
            }
            match kind {
                MemoryKind::Personal => Ok(Some(MemoryTarget::personal(
                    actor.personal_scope_id.clone(),
                ))),
                MemoryKind::GroupProfile => Ok(Some(MemoryTarget::group_profile(
                    actor
                        .group_scope_id
                        .clone()
                        .ok_or("memory_scope_unsupported")?,
                    actor.personal_scope_id.clone(),
                ))),
                MemoryKind::Group => Err("group_memory_command_only"),
                MemoryKind::LegacyUnassigned => Err("memory_scope_unsupported"),
            }
        }
        ConversationKind::Channel | ConversationKind::Unknown => Err("memory_scope_unsupported"),
    }
}

fn safe_source_ref(context: &ToolContext) -> Option<String> {
    context
        .tool_call_id
        .as_deref()
        .or(Some(context.task_id.as_str()))
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 200
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':' | b'.')
                })
        })
        .map(|value| format!("tool:{value}"))
}

fn failure_output(code: &str, message: &str) -> ToolOutput {
    ToolOutput::json(json!({
        "ok": false,
        "error_code": code,
        "message": message,
    }))
}

fn memory_session_error(err: crate::runtime::session::SessionError) -> LlmError {
    LlmError::new(err.code(), "记忆范围澄清状态暂时不可用", "memory_tool")
}

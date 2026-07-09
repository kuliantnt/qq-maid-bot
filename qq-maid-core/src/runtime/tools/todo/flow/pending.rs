//! Todo 待确认与澄清恢复状态机。
//!
//! Todo 写操作统一由 Tool Loop 触发；slash 写入口已移除。这里处理 Tool 仍会产生的
//! 两类跨轮状态：
//! - 确认类 pending：旧版 `TodoAdd`、永久删除 `TodoBulkDelete`；
//!   新建/修改/完成/取消/恢复不再进入确认；
//! - 澄清类 pending：`TodoClarify`，保存原工具、原始参数和精简候选边界，用户补充后
//!   通过受限 Tool Loop 重入原 Todo Tool，由 LLM 只负责选择/继续澄清，真正校验与副作用
//!   仍由原 Tool 重新读取 `TodoStore` 后执行。
//!
//! `TodoClarify` 不在 Pending 层解析自然语言、不直接调用 `ops::*`、不构造确认 pending；
//! 当前候选编号通过请求级 TodoTool selection scope 临时生效，不污染 `last_todo_query`。
//!
//! 旧 slash 写流程专用的 `TodoDone` / `TodoEdit` / `TodoSelectCandidate` 变体在
//! `PendingOperation` 中保留为空壳兼容旧 session；运行时遇到会清理并提示重新用
//! 自然语言发起操作，避免旧 pending 长期卡住会话。

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use qq_maid_llm::{
    provider::{
        ChatOutcome, ToolChatRequest,
        types::{ChatMessage, ChatRequest},
    },
    tool::{DynTool, Tool, ToolContext, ToolMetadata, ToolOutput, ToolRegistry},
};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::runtime::visible_entity::VisibleEntitySelectionScope as SelectionScope;
use crate::{
    config::ChatScene,
    error::LlmError,
    runtime::{
        pending::{PendingReplyKind, classify_reply},
        session::{LAST_QUERY_TTL_SECONDS, SessionMeta, SessionRecord, query_is_fresh},
        tools::TaskStore,
        tools::todo::{
            PendingTodoClarification, TODO_PENDING_DOMAIN, TodoBulkDeleteOutcome, TodoOwner,
            TodoPendingOperation, TodoStatus, todo_lexicon,
        },
        tools::{
            CompleteTodoTool, DeleteTodoTool, EditTodoTool, ManageRecurringReminderTool,
            RestoreTodoTool,
        },
        tools::{cancel_reminder_task, cancel_reminder_task_by_id},
    },
};

use super::format::*;
use super::receipt::{receipt_after_created, receipt_after_deleted};

use crate::runtime::respond::common::CommandBody;
use crate::runtime::respond::{
    RespondRequest, RespondResponse, RustRespondService, common::todo_error,
};

impl RustRespondService {
    /// 处理会话中的 Todo pending。
    ///
    /// Respond 层只负责在发现 session 有 pending 时调用本入口；Todo 的过期文案、
    /// 发起人/owner 隔离和具体状态机都留在 Todo 域内。
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
        if pending.domain() != TODO_PENDING_DOMAIN {
            return Ok(None);
        }

        if !query_is_fresh(pending.created_at(), LAST_QUERY_TTL_SECONDS) {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                CommandBody::plain("这条待确认操作已过期，没有执行。请重新发起。"),
                TodoPendingOperation::expired_command(&pending),
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
        self.handle_pending_todo_operation(user_text, session, &owner)
            .await
    }

    /// 处理 Todo 待确认与澄清恢复操作。
    ///
    /// 确认类 pending 只接受确认/取消；`TodoClarify` 则在取消、过期和候选边界检查后，
    /// 构造仅包含原 Todo Tool 与无副作用控制工具的受限 Tool Loop。恢复执行必须走原
    /// Todo Tool 的 prepare/execute 路径，Pending 层只维护恢复上下文和候选边界。
    pub(crate) async fn handle_pending_todo_operation(
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
        let pending = TodoPendingOperation::try_from_pending(&pending)
            .map_err(|err| {
                LlmError::new(
                    "pending_decode_error",
                    format!("failed to decode todo pending operation: {err}"),
                    "todo_pending",
                )
            })?
            .ok_or_else(|| {
                LlmError::new(
                    "pending_domain_mismatch",
                    "pending operation is not a todo pending",
                    "todo_pending",
                )
            })?;

        match pending {
            TodoPendingOperation::TodoAdd { draft, .. } => {
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
                    let created = crate::runtime::tools::todo::ops::create_one(
                        &self.task_store,
                        session,
                        owner,
                        draft,
                    )
                    .map_err(todo_error)?;
                    let receipt =
                        receipt_after_created(&self.task_store, session, owner, &created)?;
                    return Ok(Some(self.clear_pending_response(
                        session,
                        user_text,
                        receipt.body,
                        receipt.command,
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
            TodoPendingOperation::TodoDelete { item, .. } => {
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
                    if item.status == TodoStatus::Pending {
                        // legacy only：旧版 `TodoDelete + Pending` 曾表示软取消。
                        // 新版删除/取消已严格分离，不能再把确认删除解释成取消。
                        return Ok(Some(self.clear_pending_response(
                            session,
                            user_text,
                            CommandBody::plain(
                                "这条旧版待确认操作已失效。请重新发起删除或取消操作。",
                            ),
                            "todo_legacy_delete",
                        )?));
                    }
                    let outcome = delete_by_ids_with_pending_status(
                        &self.task_store,
                        owner,
                        std::slice::from_ref(&item.id),
                        &item.status,
                    )
                    .map_err(todo_error)?;
                    if outcome.deleted_count == 0 {
                        return Ok(Some(self.clear_pending_response(
                            session,
                            user_text,
                            CommandBody::plain("这条待办已不存在或不属于当前会话，没有执行删除。"),
                            "todo_confirm",
                        )?));
                    }
                    cancel_reminder_task(&self.notification_store, &item).map_err(|message| {
                        LlmError::new("todo_reminder_cancel_failed", message, "todo_pending")
                    })?;
                    session.clear_last_todo_action_if_matches_any(
                        &owner.key,
                        std::slice::from_ref(&item.id),
                    );
                    let reply = receipt_after_deleted(
                        &self.task_store,
                        session,
                        owner,
                        item.status,
                        outcome.deleted_count,
                        0,
                    )?
                    .body;
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
                    format_todo_pending_delete_waiting_reply(&item.status),
                    "todo_delete",
                )?))
            }
            TodoPendingOperation::TodoBulkDelete {
                item_ids,
                matched_count,
                status,
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
                    let outcome = delete_by_ids_with_pending_status(
                        &self.task_store,
                        owner,
                        &item_ids,
                        &status,
                    )
                    .map_err(todo_error)?;
                    if outcome.deleted_count > 0 {
                        for item_id in &item_ids {
                            if self
                                .task_store
                                .get_by_id(owner, item_id)
                                .map_err(todo_error)?
                                .is_none()
                            {
                                cancel_reminder_task_by_id(&self.notification_store, item_id)
                                    .map_err(|message| {
                                        LlmError::new(
                                            "todo_reminder_cancel_failed",
                                            message,
                                            "todo_pending",
                                        )
                                    })?;
                            }
                        }
                    }
                    session.clear_last_todo_action_if_matches_any(&owner.key, &item_ids);
                    let source_count = if matched_count == 0 {
                        item_ids.len()
                    } else {
                        matched_count
                    };
                    let skipped_count = source_count.saturating_sub(outcome.deleted_count);
                    let reply = receipt_after_deleted(
                        &self.task_store,
                        session,
                        owner,
                        status,
                        outcome.deleted_count,
                        skipped_count,
                    )?
                    .body;
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
            TodoPendingOperation::TodoClarify { request, .. } => {
                self.handle_pending_todo_clarification(user_text, session, owner, request)
                    .await
            }
            TodoPendingOperation::TodoDone { .. }
            | TodoPendingOperation::TodoEdit { .. }
            | TodoPendingOperation::TodoSelectCandidate { .. } => {
                Ok(Some(self.clear_pending_response(
                    session,
                    user_text,
                    CommandBody::plain(
                        "这条旧版待办确认流程已清理。请直接用自然语言重新发起待办操作。",
                    ),
                    "todo_pending_deprecated",
                )?))
            }
        }
    }

    async fn handle_pending_todo_clarification(
        &self,
        user_text: &str,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        request: PendingTodoClarification,
    ) -> Result<Option<RespondResponse>, LlmError> {
        if is_clarification_abandon_text(user_text) {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                CommandBody::plain("已取消，不执行这次待办操作。"),
                "todo_clarify_cancel",
            )?));
        }
        if !query_is_fresh(&request.created_at, LAST_QUERY_TTL_SECONDS) {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                CommandBody::plain("这次澄清已经过期，没有执行待办操作。请重新发起。"),
                "todo_clarify_expired",
            )?));
        }
        if request.candidates.is_empty() {
            return Ok(Some(self.clear_pending_response(
                session,
                user_text,
                CommandBody::plain("这条待办澄清状态缺少候选边界，没有执行待办操作。请重新发起。"),
                "todo_clarify_invalid_scope",
            )?));
        }

        if let Some(number) = parse_explicit_candidate_number(user_text) {
            return self
                .run_pending_todo_clarification_fast_path(
                    user_text, session, owner, request, number,
                )
                .await;
        }

        self.run_pending_todo_clarification_loop(user_text, session, owner, request)
            .await
    }

    async fn run_pending_todo_clarification_fast_path(
        &self,
        user_text: &str,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        request: PendingTodoClarification,
        number: usize,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let Some(arguments) = clarification_tool_arguments_for_number(&request, number)? else {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                CommandBody::plain("这次澄清对应的工具不支持编号恢复，请重新发起操作。"),
                "todo_clarify_unknown_tool",
            )?));
        };
        let registry = self.restricted_todo_clarification_registry(&request)?;
        let context = clarification_tool_context(session, owner);
        let arguments_text = serde_json::to_string(&arguments).map_err(|err| {
            LlmError::new(
                "bad_tool_arguments",
                format!("failed to serialize clarification tool arguments: {err}"),
                "todo_pending",
            )
        })?;
        let output = match registry
            .execute_json(&context, &request.tool_name, &arguments_text)
            .await
        {
            Ok(output) => output,
            Err(err) => {
                self.refresh_pending_session(session)?;
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    CommandBody::plain(format!(
                        "这次待办恢复执行失败，没有清除原澄清状态。错误：{}",
                        err.message
                    )),
                    "todo_clarify_tool_error",
                )?));
            }
        };
        let output_value = serde_json::from_str::<Value>(&output).unwrap_or_else(|_| {
            json!({
                "ok": false,
                "message": output,
            })
        });
        self.refresh_pending_session(session)?;
        if same_todo_clarification(session, &request) {
            let question = output_value
                .get("question")
                .and_then(Value::as_str)
                .or_else(|| output_value.get("message").and_then(Value::as_str))
                .unwrap_or("目标待办状态已变化或无法唯一定位，没有执行待办操作。请重新选择候选。")
                .to_owned();
            keep_todo_clarification(session, owner, request, question.clone());
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                CommandBody::plain(question),
                "todo_clarify_wait",
            )?));
        }
        let reply = tool_output_reply(&output_value);
        Ok(Some(self.append_pending_response(
            session,
            user_text,
            CommandBody::plain(reply),
            clarification_command_for_output(&output_value),
        )?))
    }

    async fn run_pending_todo_clarification_loop(
        &self,
        user_text: &str,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        request: PendingTodoClarification,
    ) -> Result<Option<RespondResponse>, LlmError> {
        let registry = self.restricted_todo_clarification_registry(&request)?;
        let context = clarification_tool_context(session, owner);
        let scene = if session
            .group_id
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            ChatScene::Group
        } else {
            ChatScene::Private
        };
        let policy = self.agent_config.resolve(scene)?;
        let chat = ChatRequest {
            session_id: session.session_id.clone(),
            model: Some(policy.main_model.clone()),
            messages: build_todo_clarification_messages(user_text, &request),
            context_budget: None,
            max_output_tokens: policy.max_output_tokens,
            reasoning_effort: policy.reasoning_effort,
            metadata: HashMap::from([
                ("purpose".to_owned(), "todo_clarification_resume".to_owned()),
                ("tool_name".to_owned(), request.tool_name.clone()),
                ("agent_scene".to_owned(), policy.scene.as_str().to_owned()),
                ("agent_profile".to_owned(), policy.profile.clone()),
            ]),
        };
        let outcome = match self
            .provider
            .chat_with_tools(ToolChatRequest {
                chat,
                tools: registry,
                tool_context: context,
                max_rounds: policy.max_tool_rounds.max(1),
                progress_sink: None,
                final_delta_sink: None,
            })
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                self.refresh_pending_session(session)?;
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    CommandBody::plain(format!(
                        "这次待办恢复没有完成，原澄清状态已保留。错误：{}",
                        err.message
                    )),
                    "todo_clarify_loop_error",
                )?));
            }
        };

        self.refresh_pending_session(session)?;
        if outcome
            .executed_tools
            .iter()
            .any(|name| name == &request.tool_name)
            && !same_todo_clarification(session, &request)
        {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                CommandBody::plain(non_empty_reply(
                    &outcome.reply,
                    "已按你的补充继续执行待办操作。",
                )),
                "todo_clarify_resumed",
            )?));
        }

        match clarification_control_action(&outcome) {
            Some(ClarificationControlAction::Abandon) => {
                return Ok(Some(self.clear_pending_response(
                    session,
                    user_text,
                    CommandBody::plain(non_empty_reply(
                        &outcome.reply,
                        "已放弃这次待办澄清。若要处理新的请求，请重新发送。",
                    )),
                    "todo_clarify_abandon",
                )?));
            }
            Some(ClarificationControlAction::AskAgain(question)) => {
                if same_todo_clarification(session, &request) {
                    keep_todo_clarification(session, owner, request, question.clone());
                }
                return Ok(Some(self.append_pending_response(
                    session,
                    user_text,
                    CommandBody::plain(non_empty_reply(&outcome.reply, &question)),
                    "todo_clarify_wait",
                )?));
            }
            None => {}
        }

        // 没有执行原 Todo Tool，或工具返回仍需澄清且原 TodoClarify 仍在：把模型最终
        // 回复视为新的最小澄清问题，保留候选边界，不产生副作用。
        let question = non_empty_reply(&outcome.reply, &request.question);
        if same_todo_clarification(session, &request) {
            keep_todo_clarification(session, owner, request, question.clone());
        }
        Ok(Some(self.append_pending_response(
            session,
            user_text,
            CommandBody::plain(question),
            "todo_clarify_wait",
        )?))
    }

    fn restricted_todo_clarification_registry(
        &self,
        request: &PendingTodoClarification,
    ) -> Result<ToolRegistry, LlmError> {
        let mut registry = self
            .tool_runtime
            .registry_for_tool_name(&request.tool_name)?;
        registry.replace(self.scoped_todo_tool(&request.tool_name, candidate_scope(request)?)?)?;
        registry.insert(Arc::new(ClarificationControlTool) as DynTool)?;
        Ok(registry)
    }

    fn scoped_todo_tool(&self, tool_name: &str, scope: Arc<[String]>) -> Result<DynTool, LlmError> {
        let scope = SelectionScope::Scoped(scope);
        match tool_name {
            "complete_todos" => Ok(Arc::new(
                CompleteTodoTool::new(
                    self.task_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope),
            ) as DynTool),
            "edit_todo" => Ok(Arc::new(
                EditTodoTool::new(
                    self.task_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope),
            ) as DynTool),
            "restore_todos" => Ok(Arc::new(
                RestoreTodoTool::new(
                    self.task_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope),
            ) as DynTool),
            "delete_todos" => Ok(Arc::new(
                DeleteTodoTool::new(
                    self.task_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope),
            ) as DynTool),
            "manage_recurring_reminder" => Ok(Arc::new(
                ManageRecurringReminderTool::new(
                    self.task_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope),
            ) as DynTool),
            _ => Err(LlmError::new(
                "unsupported_todo_clarification_tool",
                format!("unsupported todo clarification tool `{tool_name}`"),
                "todo_pending",
            )),
        }
    }

    fn refresh_pending_session(&self, session: &mut SessionRecord) -> Result<(), LlmError> {
        let latest = self
            .session_store
            .get(&session.session_id)
            .map_err(crate::runtime::respond::common::session_error)?
            .ok_or_else(|| {
                LlmError::new(
                    "session_missing",
                    format!(
                        "session `{}` disappeared after todo clarification",
                        session.session_id
                    ),
                    "session",
                )
            })?;
        *session = latest;
        Ok(())
    }
}

fn delete_by_ids_with_pending_status(
    todo_store: &crate::runtime::tools::todo::TodoStore,
    owner: &TodoOwner,
    item_ids: &[String],
    status: &TodoStatus,
) -> Result<TodoBulkDeleteOutcome, crate::runtime::tools::todo::TodoError> {
    // 删除确认是按“发起确认时的状态”授权的；执行确认时仍必须在 SQL 条件里校验
    // 当前状态，避免过期确认把已经恢复或重新变为进行中的待办永久删除。
    match status {
        TodoStatus::Completed => todo_store.delete_completed_by_ids(owner, item_ids),
        TodoStatus::Pending => todo_store.delete_pending_by_ids(owner, item_ids),
    }
}

const CLARIFICATION_CONTROL_TOOL_NAME: &str = "clarification_control";

struct ClarificationControlTool;

#[async_trait]
impl Tool for ClarificationControlTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: CLARIFICATION_CONTROL_TOOL_NAME.to_owned(),
            description: "澄清恢复控制工具。仅用于表示仍需追问或放弃当前澄清，不操作 Todo 数据。"
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["ask_again", "abandon"],
                        "description": "ask_again=信息仍不足，需要继续追问；abandon=用户放弃或明显不是在回答当前澄清。"
                    },
                    "question": {
                        "type": ["string", "null"],
                        "description": "action=ask_again 时给用户的最小澄清问题；其他情况传 null。"
                    }
                },
                "required": ["action", "question"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        _context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| LlmError::new("bad_tool_arguments", "action is required", "tool"))?;
        let question = arguments
            .get("question")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        match action {
            "ask_again" => Ok(ToolOutput::json(json!({
                "ok": true,
                "action": "ask_again",
                "question": question.unwrap_or_else(|| "请再具体说明要操作哪条待办。".to_owned()),
            }))),
            "abandon" => Ok(ToolOutput::json(json!({
                "ok": true,
                "action": "abandon",
                "question": Value::Null,
            }))),
            _ => Err(LlmError::new(
                "bad_tool_arguments",
                "action must be ask_again or abandon",
                "tool",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClarificationControlAction {
    AskAgain(String),
    Abandon,
}

fn clarification_control_action(outcome: &ChatOutcome) -> Option<ClarificationControlAction> {
    outcome
        .tool_results
        .iter()
        .rev()
        .find(|result| result.name == CLARIFICATION_CONTROL_TOOL_NAME)
        .and_then(
            |result| match result.output.get("action").and_then(Value::as_str) {
                Some("ask_again") => Some(ClarificationControlAction::AskAgain(
                    result
                        .output
                        .get("question")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("请再具体说明要操作哪条待办。")
                        .to_owned(),
                )),
                Some("abandon") => Some(ClarificationControlAction::Abandon),
                _ => None,
            },
        )
}

fn candidate_scope(request: &PendingTodoClarification) -> Result<Arc<[String]>, LlmError> {
    if request.candidates.is_empty() {
        return Err(LlmError::new(
            "todo_clarification_scope_empty",
            "todo clarification candidates are empty",
            "todo_pending",
        ));
    }
    let ids = request
        .candidates
        .iter()
        .map(|candidate| candidate.id.clone())
        .collect::<Vec<_>>();
    Ok(Arc::from(ids.into_boxed_slice()))
}

fn build_todo_clarification_messages(
    user_text: &str,
    request: &PendingTodoClarification,
) -> Vec<ChatMessage> {
    let candidates = request
        .candidates
        .iter()
        .map(|candidate| format!("{}. {}", candidate.display_number, candidate.title))
        .collect::<Vec<_>>()
        .join("\n");
    let original_arguments =
        serde_json::to_string_pretty(&request.arguments).unwrap_or_else(|_| "{}".to_owned());
    let system = format!(
        "你正在恢复一个待办工具澄清任务。\n\n\
职责边界：\n\
- 只能恢复原工具 `{tool_name}`，不得改成其他 Todo 操作。\n\
- 当前候选编号只在本次澄清中有效，必须从候选 1..N 里选择；不要使用数据库内部 ID。\n\
- 候选标题里的数字（例如“6 号”“买 2 个”）不是候选编号。\n\
- 如果能唯一确定目标，请调用原工具 `{tool_name}`，用候选展示编号作为 number/numbers，并保留或补全原始参数里的其他业务字段。\n\
- 如果仍无法唯一确定，请调用 `{control_tool}`，action=ask_again，并给出最小澄清问题。\n\
- 如果用户明确放弃或明显不是在回答当前澄清，请调用 `{control_tool}`，action=abandon。\n\
- 不要编造成功结果；工具结果才是真实执行状态。\n\n\
原工具：{tool_name}\n原始参数 JSON：\n{original_arguments}\n\n上一次澄清问题：\n{question}\n\n当前候选：\n{candidates}",
        tool_name = request.tool_name,
        control_tool = CLARIFICATION_CONTROL_TOOL_NAME,
        question = request.question,
    );
    vec![
        ChatMessage::system(system),
        ChatMessage::user(user_text.trim().to_owned()),
    ]
}

fn clarification_tool_context(session: &SessionRecord, owner: &TodoOwner) -> ToolContext {
    ToolContext {
        task_id: format!("todo-clarify:{}", Uuid::new_v4()),
        user_id: owner.user_id.clone(),
        scope_id: owner.scope_key.clone(),
        group_member_role: None,
        tool_call_id: Some(format!("clarify-{}", session.session_id)),
    }
}

fn is_clarification_abandon_text(text: &str) -> bool {
    let compact = text
        .trim()
        .chars()
        .filter(|ch| {
            !ch.is_whitespace()
                && !matches!(
                    ch,
                    '，' | ','
                        | '。'
                        | '.'
                        | '！'
                        | '!'
                        | '？'
                        | '?'
                        | '、'
                        | ';'
                        | '；'
                        | ':'
                        | '：'
                )
        })
        .collect::<String>()
        .trim_end_matches(['了', '吧', '啊', '呀', '呢'])
        .to_owned();
    matches!(
        compact.as_str(),
        "取消" | "放弃" | "算了" | "不用" | "不要" | "撤销"
    )
}

fn parse_explicit_candidate_number(text: &str) -> Option<usize> {
    let compact = text
        .trim()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    let compact = compact
        .trim_matches(&['。', '.', '，', ',', '！', '!', '？', '?'][..])
        .to_owned();
    if compact.is_empty() {
        return None;
    }
    if compact.chars().all(|ch| ch.is_ascii_digit()) {
        return compact.parse::<usize>().ok().filter(|value| *value > 0);
    }
    let mut core = compact.strip_prefix('第')?;
    for suffix in ["条", "个", "项"] {
        if let Some(stripped) = core.strip_suffix(suffix) {
            core = stripped;
            break;
        }
    }
    if core.chars().all(|ch| ch.is_ascii_digit()) {
        return core.parse::<usize>().ok().filter(|value| *value > 0);
    }
    parse_simple_chinese_number(core)
}

fn parse_simple_chinese_number(text: &str) -> Option<usize> {
    match text {
        "一" => Some(1),
        "二" | "两" => Some(2),
        "三" => Some(3),
        "四" => Some(4),
        "五" => Some(5),
        "六" => Some(6),
        "七" => Some(7),
        "八" => Some(8),
        "九" => Some(9),
        "十" => Some(10),
        _ => None,
    }
}

fn clarification_tool_arguments_for_number(
    request: &PendingTodoClarification,
    number: usize,
) -> Result<Option<Value>, LlmError> {
    let mut arguments = request.arguments.clone();
    let object = arguments.as_object_mut().ok_or_else(|| {
        LlmError::new(
            "bad_tool_arguments",
            "pending clarification arguments must be a JSON object",
            "todo_pending",
        )
    })?;
    match request.tool_name.as_str() {
        "complete_todos" | "restore_todos" | "delete_todos" | "manage_recurring_reminder" => {
            object.insert("numbers".to_owned(), json!([number]));
            object.insert("reference".to_owned(), Value::Null);
            if request.tool_name == "delete_todos" {
                object.insert("query".to_owned(), Value::Null);
                object.insert("all_status".to_owned(), Value::Null);
            }
            Ok(Some(arguments))
        }
        "edit_todo" => {
            object.insert("number".to_owned(), json!(number));
            object.insert("reference".to_owned(), Value::Null);
            Ok(Some(arguments))
        }
        _ => Ok(None),
    }
}

fn same_todo_clarification(session: &SessionRecord, request: &PendingTodoClarification) -> bool {
    matches!(
        session
            .pending_operation
            .as_ref()
            .and_then(|pending| TodoPendingOperation::try_from_pending(pending).ok().flatten()),
        Some(TodoPendingOperation::TodoClarify { request: current, .. })
            if current.tool_name == request.tool_name && current.created_at == request.created_at
    )
}

fn keep_todo_clarification(
    session: &mut SessionRecord,
    owner: &TodoOwner,
    mut request: PendingTodoClarification,
    question: String,
) {
    request.question = question;
    session.pending_operation = Some(
        TodoPendingOperation::TodoClarify {
            initiator_user_id: owner.user_id.clone(),
            owner_key: owner.key.clone(),
            created_at: request.created_at.clone(),
            request,
        }
        .into(),
    );
}

fn clarification_command_for_output(output: &Value) -> &'static str {
    if output.get("requires_confirmation").and_then(Value::as_bool) == Some(true) {
        "todo_clarify_confirm_ready"
    } else if output.get("ok").and_then(Value::as_bool) == Some(false) {
        "todo_clarify_wait"
    } else {
        "todo_clarify_resumed"
    }
}

fn tool_output_reply(output: &Value) -> String {
    output
        .get("question")
        .and_then(Value::as_str)
        .or_else(|| output.get("message").and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| "已按澄清选择继续待办操作。".to_owned())
}

fn non_empty_reply(reply: &str, fallback: &str) -> String {
    let reply = reply.trim();
    if reply.is_empty() {
        fallback.to_owned()
    } else {
        reply.to_owned()
    }
}

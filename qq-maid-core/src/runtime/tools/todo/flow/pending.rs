//! Todo 待确认与澄清恢复状态机。
//!
//! Todo 写操作统一由 Tool Loop 触发；slash 写入口已移除。这里处理 Tool 仍会产生的
//! 两类跨轮状态：
//! - 确认类 pending：单条删除和批量删除；
//!   新建/修改/完成/取消/恢复不再进入确认；
//! - 澄清类 pending：`TodoClarify`，保存原工具、原始参数和精简候选边界，用户补充后
//!   通过受限 Tool Loop 重入原 Todo Tool，由 LLM 只负责选择/继续澄清，真正校验与副作用
//!   仍由原 Tool 重新读取 `TodoStore` 后执行。
//!
//! `TodoClarify` 不在 Pending 层解析自然语言、不直接调用 `ops::*`、不构造确认 pending；
//! 当前候选编号通过请求级 TodoTool selection scope 临时生效，不污染 `last_todo_query`。
//!
use std::{collections::HashMap, sync::Arc};

use qq_maid_llm::{
    provider::{ToolChatRequest, types::ChatRequest},
    tool::{DynTool, ToolRegistry},
};
use serde_json::{Value, json};

use crate::runtime::visible_entity::VisibleEntitySelectionScope as SelectionScope;
use crate::{
    config::ChatScene,
    error::LlmError,
    runtime::{
        freshness::query_is_fresh,
        pending::{PendingReplyKind, classify_reply},
        session::{LAST_QUERY_TTL_SECONDS, SessionRecord},
        tools::todo::{
            PendingTodoClarification, TodoBulkDeleteOutcome, TodoOwner, TodoPendingPayload,
            TodoStatus, todo_lexicon,
        },
        tools::{
            CompleteTodoTool, DeleteTodoTool, EditTodoTool, ManageRecurringReminderTool,
            RestoreTodoTool,
        },
        tools::{cancel_reminder_task, cancel_reminder_task_by_id},
    },
};

use super::format::*;
use super::pending_clarification::*;
use super::receipt::receipt_after_deleted;

use crate::runtime::respond::common::CommandBody;
use crate::runtime::respond::{RespondResponse, RustRespondService, common::todo_error};

impl RustRespondService {
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
        let pending_revision = pending.revision();
        if pending.owner_key().is_some_and(|key| key != owner.key) {
            return Ok(None);
        }
        let pending = match TodoPendingPayload::try_from_pending(&pending) {
            Ok(Some(pending)) => pending,
            Ok(None) | Err(_) => {
                return Ok(Some(self.clear_pending_response(
                    session,
                    user_text,
                    CommandBody::plain("这条待确认操作数据无效，已清理。请重新发起。"),
                    "todo_pending_invalid",
                )?));
            }
        };

        match pending {
            TodoPendingPayload::TodoDelete { item, .. } => {
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
                        // 单条 TodoDelete 只允许用于已完成待办；进行中范围必须使用
                        // 带明确 status 的 TodoBulkDelete，避免把永久删除误解为软取消。
                        return Ok(Some(self.clear_pending_response(
                            session,
                            user_text,
                            CommandBody::plain(
                                "这条待确认删除范围无效。请重新发起删除或取消操作。",
                            ),
                            "todo_delete_invalid_pending",
                        )?));
                    }
                    if !self.claim_todo_pending_execution(session, owner, pending_revision)? {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            CommandBody::plain(
                                "这条待确认操作已变化或已被处理，没有重复执行。请重新发起。",
                            ),
                            "pending_claim_rejected",
                        )?));
                    }
                    let outcome = match delete_by_ids_with_pending_status(
                        &self.task_store,
                        owner,
                        std::slice::from_ref(&item.id),
                        &item.status,
                    ) {
                        Ok(outcome) => outcome,
                        Err(err) => {
                            return Ok(Some(self.pending_execution_failed_response(
                                session,
                                user_text,
                                pending_revision,
                                todo_error(err),
                            )?));
                        }
                    };
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
            TodoPendingPayload::TodoBulkDelete {
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
                    if !self.claim_todo_pending_execution(session, owner, pending_revision)? {
                        return Ok(Some(self.append_pending_response(
                            session,
                            user_text,
                            CommandBody::plain(
                                "这条待确认操作已变化或已被处理，没有重复执行。请重新发起。",
                            ),
                            "pending_claim_rejected",
                        )?));
                    }
                    let outcome = match delete_by_ids_with_pending_status(
                        &self.task_store,
                        owner,
                        &item_ids,
                        &status,
                    ) {
                        Ok(outcome) => outcome,
                        Err(err) => {
                            return Ok(Some(self.pending_execution_failed_response(
                                session,
                                user_text,
                                pending_revision,
                                todo_error(err),
                            )?));
                        }
                    };
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
            TodoPendingPayload::TodoClarify { request, .. } => {
                self.handle_pending_todo_clarification(
                    user_text,
                    session,
                    owner,
                    request,
                    pending_revision,
                )
                .await
            }
        }
    }

    async fn handle_pending_todo_clarification(
        &self,
        user_text: &str,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        request: PendingTodoClarification,
        revision: u64,
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
                    user_text, session, owner, request, number, revision,
                )
                .await;
        }

        self.run_pending_todo_clarification_loop(user_text, session, owner, request, revision)
            .await
    }

    async fn run_pending_todo_clarification_fast_path(
        &self,
        user_text: &str,
        session: &mut SessionRecord,
        owner: &TodoOwner,
        request: PendingTodoClarification,
        number: usize,
        revision: u64,
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
        if !self.claim_todo_pending_execution(session, owner, revision)? {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                CommandBody::plain("这次待办澄清已变化或已被处理，没有重复执行。请重新发起。"),
                "pending_claim_rejected",
            )?));
        }
        let output = match registry
            .execute_json(&context, &request.tool_name, &arguments_text)
            .await
        {
            Ok(output) => output,
            Err(err) => {
                return Ok(Some(self.pending_execution_failed_response(
                    session, user_text, revision, err,
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
            keep_todo_clarification(session, owner, request, question.clone())?;
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
        revision: u64,
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
        if !self.claim_todo_pending_execution(session, owner, revision)? {
            return Ok(Some(self.append_pending_response(
                session,
                user_text,
                CommandBody::plain("这次待办澄清已变化或已被处理，没有重复执行。请重新发起。"),
                "pending_claim_rejected",
            )?));
        }
        let outcome = match self
            .provider
            .chat_with_tools(ToolChatRequest {
                chat,
                tools: registry,
                tool_context: context,
                max_rounds: policy.max_tool_rounds.max(1),
                progress_sink: None,
                final_delta_sink: None,
                run_handle: None,
            })
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                return Ok(Some(self.pending_execution_failed_response(
                    session, user_text, revision, err,
                )?));
            }
        };

        self.refresh_pending_session(session)?;
        if outcome
            .agent
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
                    keep_todo_clarification(session, owner, request, question.clone())?;
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
            keep_todo_clarification(session, owner, request, question.clone())?;
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
            .and_then(|pending| TodoPendingPayload::try_from_pending(pending).ok().flatten()),
        Some(TodoPendingPayload::TodoClarify { request: current, .. })
            if current.tool_name == request.tool_name && current.created_at == request.created_at
    )
}

fn keep_todo_clarification(
    session: &mut SessionRecord,
    owner: &TodoOwner,
    mut request: PendingTodoClarification,
    question: String,
) -> Result<(), LlmError> {
    request.question = question;
    let operation = TodoPendingPayload::TodoClarify {
        initiator_user_id: owner.user_id.clone(),
        owner_key: owner.key.clone(),
        created_at: request.created_at.clone(),
        request,
    };
    let replacement = operation.into_prepared_action(&session.scope_key);
    let current = session.pending_operation.as_mut().ok_or_else(|| {
        LlmError::new(
            "pending_missing",
            "todo clarification disappeared before returning to waiting state",
            "todo_pending",
        )
    })?;
    current
        .continue_waiting_after_execution(
            replacement.payload().clone(),
            replacement.display_snapshot().clone(),
            replacement.expires_at(),
        )
        .map_err(|err| {
            LlmError::new(
                "pending_transition_failed",
                format!("failed to return todo clarification to waiting state: {err}"),
                "todo_pending",
            )
        })?;
    Ok(())
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

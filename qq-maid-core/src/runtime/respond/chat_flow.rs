//! 普通聊天流程。
//!
//! 承担 `RustRespondService` 中"兜底聊天"路径的实现：
//! 组装 LLM 请求、发起调用、保存对话记录、自动生成会话标题等。

use std::{future::Future, pin::Pin};

use serde_json::{Value, json};

use crate::{
    config::agent::AgentConfigSource,
    error::LlmError,
    provider::types::{ChatMessage, ChatRole},
    runtime::session::{DEFAULT_SESSION_TITLE, SessionMeta, SessionRecord},
};

use super::{
    ChatToolPlan, RespondPurpose, RespondRequest, RespondResponse, RustRespondService,
    agent_outcome::{AgentTurnOutcome, AgentTurnStatus, ToolEffect, ToolOutcomeStatus},
    common::{
        SESSION_HISTORY_MESSAGE_LIMIT, command_response, empty_respond_request, memory_error,
        merge_metadata, session_error,
    },
    llm_service::{ChatService, LlmChatService, response_from_output},
    session_flow::build_session_context,
    title::generate_session_title,
    todo_flow::aggregate_todo_tool_results,
    tool_presenters::tool_outcome_from_weather_result,
};

pub(super) mod todo_guard;

const TOOL_LOOP_AMBIGUITY_PROMPT: &str = "\
工具调用边界：如果用户要修改待办、记忆或其他持久化状态，但目标、字段或修改内容存在歧义，\
不要猜测，也不要调用写工具；直接用自然语言追问缺少的信息并结束本轮回复。";

impl RustRespondService {
    /// 处理普通聊天请求。
    ///
    /// 1. 空消息直接返回提示。
    /// 2. 构建会话上下文、记忆上下文和知识检索上下文。
    /// 3. 调用 LLM 或 Tool Loop 获取回复。
    /// 4. 保存对话记录。
    /// 5. 尝试自动生成会话标题。
    pub(super) async fn handle_chat(
        &self,
        req: RespondRequest,
        user_text: String,
        meta: SessionMeta,
        mut session: SessionRecord,
        chat_tool_plan: ChatToolPlan,
    ) -> Result<RespondResponse, LlmError> {
        if user_text.trim().is_empty() {
            let reply = "唔，我在。可以直接说要我看哪一块。";
            self.session_store
                .append_exchange(&mut session, &user_text, reply)
                .map_err(session_error)?;
            return Ok(command_response(
                reply,
                Some(session.session_id),
                Some("empty_chat"),
            ));
        }

        let session_context = build_session_context(&session);

        let knowledge_context = self.knowledge_index.search_context(&user_text)?;
        let used_knowledge = !knowledge_context.text.trim().is_empty();
        let memory_context = self.build_memory_context(&meta)?;
        let used_memory = !memory_context.trim().is_empty();
        let system_prompts = self.prompt_config.load_system_prompts()?;
        let system_prompts = if matches!(chat_tool_plan, ChatToolPlan::ForceCompleteToolLoop) {
            let mut prompts = system_prompts;
            prompts.push(TOOL_LOOP_AMBIGUITY_PROMPT.to_owned());
            prompts
        } else {
            system_prompts
        };
        let policy = self.resolve_agent_policy(&req)?;
        let chat_req = RespondRequest {
            session_id: session.session_id.clone(),
            purpose: RespondPurpose::Chat,
            user_text: user_text.clone(),
            system_prompts,
            memory_context,
            knowledge_context: knowledge_context.text.clone(),
            session_context,
            history_messages: recent_session_messages(&session, SESSION_HISTORY_MESSAGE_LIMIT),
            scope_key: meta.scope_key.clone(),
            user_id: meta.user_id.clone(),
            group_id: meta.group_id.clone(),
            guild_id: meta.guild_id.clone(),
            channel_id: meta.channel_id.clone(),
            message_id: req.message_id.clone(),
            timestamp: req.timestamp.clone(),
            platform: meta.platform.clone(),
            event_type: req.event_type.clone(),
            metadata: merge_metadata(
                req.metadata,
                &[
                    ("purpose", "chat"),
                    ("platform", meta.platform.as_str()),
                    ("scope_key", meta.scope_key.as_str()),
                    ("agent_scene", policy.scene.as_str()),
                    ("agent_profile", policy.profile.as_str()),
                    ("agent_config_source", policy_source_label(&policy)),
                ],
            ),
            model: Some(policy.main_model.clone()),
            max_output_tokens: policy.max_output_tokens,
            reasoning_effort: policy.reasoning_effort,
            ..empty_respond_request()
        };
        if !policy.enabled {
            let reply = "当前场景普通 AI 聊天未启用。";
            self.session_store
                .append_exchange(&mut session, &user_text, reply)
                .map_err(session_error)?;
            return Ok(command_response(
                reply,
                Some(session.session_id),
                Some("chat_scene_disabled"),
            ));
        }
        let service =
            LlmChatService::with_context_budget(self.provider.clone(), self.context_budget);
        let use_tool_loop = matches!(chat_tool_plan, ChatToolPlan::ForceCompleteToolLoop);
        let (output, todo_success_validation, agent_turn_outcome) = if use_tool_loop {
            let mut output = service
                .respond_with_tools(chat_req, self.tool_registry.clone(), policy.max_tool_rounds)
                .await?;

            let mut latest_session = self
                .session_store
                .get(&session.session_id)
                .map_err(session_error)?
                .ok_or_else(|| {
                    LlmError::new(
                        "session_missing",
                        format!(
                            "session `{}` disappeared after tool loop",
                            session.session_id
                        ),
                        "session",
                    )
                })?;
            // Tool 执行会基于同一 active session 保存 pending/最近 Todo 查询等字段；
            // 这里只把本轮聊天在调用模型前更新的状态合并回最新记录，避免旧 SessionRecord 覆盖工具结果。
            latest_session.state = session.state.clone();
            session = latest_session;

            let owner =
                crate::runtime::todo::TodoStore::owner(meta.user_id.as_deref(), &meta.scope_key);
            let turn_outcome =
                build_agent_turn_outcome(&self.todo_store, &mut session, &owner, &output)?;
            let validation = if turn_outcome.can_replace_model_reply() {
                apply_agent_turn_outcome(&mut output, &turn_outcome);
                todo_success_validation_from_agent_outcome(&turn_outcome)
            } else if turn_outcome.has_unhandled_outcome() && !turn_outcome.outcomes.is_empty() {
                apply_agent_turn_compat_output(&mut output, &turn_outcome);
                todo_success_validation_from_agent_outcome(&turn_outcome)
            } else {
                let validation = todo_guard::validate_todo_success_reply(&output);
                if !validation.passed() {
                    output = todo_success_not_verified_output(
                        output,
                        todo_guard::todo_success_not_verified_reply_for_output,
                    );
                }
                validation
            };
            (output, validation, Some(turn_outcome))
        } else {
            (
                service.respond(chat_req).await?,
                todo_guard::TodoSuccessValidation::Passed {
                    claimed_success: false,
                },
                None,
            )
        };

        let reply = output.reply.clone();
        let executed_tools = output.executed_tools.clone();
        let todo_tool_summaries = todo_guard::todo_tool_result_summaries(&output);
        if use_tool_loop {
            if todo_tool_summaries.is_empty() {
                if todo_success_validation.claimed_success() {
                    tracing::warn!(
                        entered_tool_loop = true,
                        executed_tools = ?executed_tools,
                        todo_success_claimed = true,
                        todo_success_verified = todo_success_validation.passed(),
                        "todo success claim blocked without todo write tool result"
                    );
                } else {
                    tracing::debug!(
                        entered_tool_loop = true,
                        executed_tools = ?executed_tools,
                        "tool loop completed without todo write tool result"
                    );
                }
            } else {
                for summary in &todo_tool_summaries {
                    tracing::info!(
                        entered_tool_loop = true,
                        tool = %summary.tool,
                        succeeded = summary.succeeded,
                        error_code = summary.error_code.as_deref().unwrap_or(""),
                        requires_confirmation = summary.requires_confirmation,
                        requires_clarification = summary.requires_clarification,
                        skipped = summary.skipped,
                        skip_reason = summary.skip_reason.as_deref().unwrap_or(""),
                        pending_action = summary.pending_action.as_deref().unwrap_or(""),
                        todo_success_claimed = todo_success_validation.claimed_success(),
                        todo_success_verified = todo_success_validation.passed(),
                        "todo tool result"
                    );
                }
            }
        }
        self.session_store
            .append_exchange(&mut session, &user_text, &reply)
            .map_err(session_error)?;
        self.schedule_auto_title(session.clone());

        let mut response = response_from_output(output);
        response.session_id = Some(session.session_id);
        response.command = agent_turn_outcome
            .as_ref()
            .and_then(AgentTurnOutcome::primary_command);
        response.handled = Some(true);
        let agent_diagnostics = agent_turn_outcome
            .as_ref()
            .map(AgentTurnOutcome::diagnostics)
            .unwrap_or_else(|| {
                json!({
                    "agent_turn_status": Value::Null,
                    "tool_outcomes": [],
                })
            });
        response.diagnostics = Some(json!({
            "backend": "rust",
            "session_backend": "rust",
            "used_memory": used_memory,
            "used_knowledge": used_knowledge,
            "knowledge_hit_count": knowledge_context.hit_count,
            "used_search": false,
            "tool_calling_enabled": use_tool_loop,
            "agent_policy": policy.diagnostic_summary(),
            "tool_loop_executed_tools": executed_tools,
            "todo_tool_results": todo_tool_summaries.iter().map(|summary| json!({
                "tool": &summary.tool,
                "succeeded": summary.succeeded,
                "error_code": &summary.error_code,
                "requires_confirmation": summary.requires_confirmation,
                "requires_clarification": summary.requires_clarification,
                "skipped": summary.skipped,
                "skip_reason": &summary.skip_reason,
                "pending_action": &summary.pending_action,
            })).collect::<Vec<_>>(),
            "todo_success_claimed": todo_success_validation.claimed_success(),
            "todo_success_verified": todo_success_validation.passed(),
            "agent_turn_status": agent_diagnostics["agent_turn_status"].clone(),
            "tool_outcomes": agent_diagnostics["tool_outcomes"].clone(),
            "tool_retry_count": 0,
            "error_code": if let Some(error_code) = agent_turn_outcome
                .as_ref()
                .and_then(AgentTurnOutcome::primary_error_code)
            {
                json!(error_code)
            } else if let Some(error_code) = generic_agent_error_code(
                agent_turn_outcome.as_ref(),
                use_tool_loop,
                &todo_success_validation,
            ) {
                json!(error_code)
            } else {
                Value::Null
            },
        }));
        Ok(response)
    }

    /// 普通聊天真流式路径：复用非流式聊天的上下文构造和后处理，只替换 LLM 调用方式。
    pub async fn handle_chat_stream<F>(
        &self,
        req: RespondRequest,
        on_delta: F,
    ) -> Result<RespondResponse, LlmError>
    where
        F: FnMut(String) -> Pin<Box<dyn Future<Output = Result<(), LlmError>> + Send>> + Send,
    {
        let user_text = req.effective_user_text();
        let meta = SessionMeta::new(
            req.scope_key.clone(),
            req.user_id.clone(),
            req.group_id.clone(),
            req.guild_id.clone(),
            req.channel_id.clone(),
            req.platform.clone(),
        );
        let mut session = self
            .session_store
            .get_or_create_active(&meta)
            .map_err(session_error)?;
        if user_text.trim().is_empty() {
            return self
                .handle_chat(req, user_text, meta, session, ChatToolPlan::Plain)
                .await;
        }

        let session_context = build_session_context(&session);

        let knowledge_context = self.knowledge_index.search_context(&user_text)?;
        let used_knowledge = !knowledge_context.text.trim().is_empty();
        let memory_context = self.build_memory_context(&meta)?;
        let used_memory = !memory_context.trim().is_empty();
        let system_prompts = self.prompt_config.load_system_prompts()?;
        let policy = self.resolve_agent_policy(&req)?;
        if !policy.enabled {
            let reply = "当前场景普通 AI 聊天未启用。";
            self.session_store
                .append_exchange(&mut session, &user_text, reply)
                .map_err(session_error)?;
            return Ok(command_response(
                reply,
                Some(session.session_id),
                Some("chat_scene_disabled"),
            ));
        }
        let service =
            LlmChatService::with_context_budget(self.provider.clone(), self.context_budget);
        let output = service
            .stream_respond(
                RespondRequest {
                    session_id: session.session_id.clone(),
                    purpose: RespondPurpose::Chat,
                    user_text: user_text.clone(),
                    system_prompts,
                    memory_context,
                    knowledge_context: knowledge_context.text.clone(),
                    session_context,
                    history_messages: recent_session_messages(
                        &session,
                        SESSION_HISTORY_MESSAGE_LIMIT,
                    ),
                    scope_key: meta.scope_key.clone(),
                    user_id: meta.user_id.clone(),
                    group_id: meta.group_id.clone(),
                    guild_id: meta.guild_id.clone(),
                    channel_id: meta.channel_id.clone(),
                    message_id: req.message_id.clone(),
                    timestamp: req.timestamp.clone(),
                    platform: meta.platform.clone(),
                    event_type: req.event_type.clone(),
                    metadata: merge_metadata(
                        req.metadata,
                        &[
                            ("purpose", "chat"),
                            ("platform", meta.platform.as_str()),
                            ("scope_key", meta.scope_key.as_str()),
                            ("agent_scene", policy.scene.as_str()),
                            ("agent_profile", policy.profile.as_str()),
                            ("agent_config_source", policy_source_label(&policy)),
                        ],
                    ),
                    model: Some(policy.main_model.clone()),
                    max_output_tokens: policy.max_output_tokens,
                    reasoning_effort: policy.reasoning_effort,
                    ..empty_respond_request()
                },
                on_delta,
            )
            .await?;

        let reply = output.reply.clone();
        self.session_store
            .append_exchange(&mut session, &user_text, &reply)
            .map_err(session_error)?;
        self.schedule_auto_title(session.clone());

        let mut response = response_from_output(output);
        response.session_id = Some(session.session_id);
        response.command = None;
        response.handled = Some(true);
        response.diagnostics = Some(json!({
            "backend": "rust",
            "session_backend": "rust",
            "used_memory": used_memory,
            "used_knowledge": used_knowledge,
            "knowledge_hit_count": knowledge_context.hit_count,
            "used_search": false,
            "agent_policy": policy.diagnostic_summary(),
        }));
        Ok(response)
    }

    /// 从长期记忆存储中读取当前请求可访问的最近记录，组装为系统提示上下文。
    ///
    /// 个人和群记忆先在 SQL 中限定各自合法作用域，再沿用原有 `row_id DESC LIMIT 12`
    /// 合并排序；这里不做固定配额，避免低排序记忆挤掉原本更靠前的合法记忆。
    pub(super) fn build_memory_context(&self, meta: &SessionMeta) -> Result<String, LlmError> {
        let records = self
            .memory_store
            .list_accessible_for_context(meta.user_id.as_deref(), meta.group_id.as_deref(), 12)
            .map_err(memory_error)?;
        let rows = records
            .iter()
            .filter(|record| !record.content.trim().is_empty())
            .map(|record| format!("- [{}] {}", record.ts, record.content))
            .collect::<Vec<_>>();
        if rows.is_empty() {
            Ok(String::new())
        } else {
            let mut context = format!(
                "以下是用户明确要求记录的本地记忆，只作为参考，不要机械复述：\n{}",
                rows.join("\n")
            );
            if meta
                .group_id
                .as_deref()
                .is_some_and(|value| !value.is_empty())
            {
                context.push_str(
                    "\n群聊隐私约束：个人记忆只用于理解当前发言者，不要主动披露、列举或转述个人记忆。",
                );
            }
            Ok(context)
        }
    }

    /// 如果会话标题还是默认值，且用户消息轮数在 2~4 之间，则后台尝试生成标题。
    ///
    /// 主聊天回复已经完成落库，标题只是展示增强；不能让标题模型的慢响应、
    /// 失败或取消影响本轮 `Completed`。后台任务只允许条件更新标题，不能保存
    /// 旧的完整会话快照，否则会覆盖期间继续写入的历史、pending 或手工重命名。
    fn schedule_auto_title(&self, session: SessionRecord) {
        let Some(title_model) = self.title_model.clone() else {
            return;
        };
        if session.title != DEFAULT_SESSION_TITLE {
            return;
        }
        let user_message_count = session
            .history
            .iter()
            .filter(|message| message.role == "user" && !message.content.trim().is_empty())
            .count();
        if !(2..=4).contains(&user_message_count) {
            return;
        }

        let provider = self.provider.clone();
        let session_store = self.session_store.clone();
        let session_id = session.session_id.clone();
        let history = session.history.clone();
        tokio::spawn(async move {
            match generate_session_title(provider.as_ref(), &title_model, &history, false).await {
                Ok(title) => {
                    match session_store.update_title_if_current(
                        &session_id,
                        DEFAULT_SESSION_TITLE,
                        &title,
                    ) {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::debug!(
                                session_id = %session_id,
                                "generated session title ignored because current title changed"
                            );
                        }
                        Err(err) => {
                            tracing::warn!(
                                error = %err.message(),
                                session_id = %session_id,
                                "failed to save generated session title"
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::debug!(
                        error = %err,
                        session_id = %session_id,
                        "session auto title generation failed"
                    );
                }
            }
        });
    }
}

fn todo_success_not_verified_output(
    output: super::llm_service::RespondOutput,
    reply_builder: impl FnOnce(&super::llm_service::RespondOutput) -> String,
) -> super::llm_service::RespondOutput {
    let reply = reply_builder(&output);
    super::llm_service::RespondOutput {
        reply: reply.clone(),
        text: reply.clone(),
        markdown: None,
        chat: super::types::ChatResponse::ok(
            reply,
            crate::util::metrics::LlmMetrics {
                provider: "rust".to_owned(),
                model: "tool-loop-guard".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 0,
            },
            None,
        ),
        executed_tools: output.executed_tools,
        tool_results: output.tool_results,
    }
}

fn build_agent_turn_outcome(
    todo_store: &crate::runtime::todo::TodoStore,
    session: &mut SessionRecord,
    owner: &crate::runtime::todo::TodoOwner,
    output: &super::llm_service::RespondOutput,
) -> Result<AgentTurnOutcome, LlmError> {
    let todo_aggregation =
        aggregate_todo_tool_results(todo_store, session, owner, &output.tool_results)?;
    let mut outcomes = Vec::new();
    let mut todo_outcomes = todo_aggregation.outcomes.into_iter().peekable();
    for (index, result) in output.tool_results.iter().enumerate() {
        if todo_aggregation.consumed_result_indexes.contains(&index) {
            while todo_outcomes
                .peek()
                .is_some_and(|(outcome_index, _)| *outcome_index == index)
            {
                if let Some((_, outcome)) = todo_outcomes.next() {
                    outcomes.push(outcome);
                }
            }
        } else if let Some(outcome) = tool_outcome_from_weather_result(result) {
            outcomes.push(outcome);
        } else {
            outcomes.push(super::agent_outcome::ToolExecutionOutcome::generic(result));
        }
    }
    outcomes.extend(todo_outcomes.map(|(_, outcome)| outcome));
    Ok(AgentTurnOutcome::from_outcomes(outcomes))
}

fn apply_agent_turn_outcome(
    output: &mut super::llm_service::RespondOutput,
    outcome: &AgentTurnOutcome,
) {
    let body = outcome.render_body();
    output.reply = body.markdown.clone().unwrap_or_else(|| body.text.clone());
    output.text = body.text;
    output.markdown = body.markdown;
    output.chat.reply = Some(output.reply.clone());
}

fn apply_agent_turn_compat_output(
    output: &mut super::llm_service::RespondOutput,
    outcome: &AgentTurnOutcome,
) {
    let body = outcome.render_compat_body();
    output.reply = body.markdown.clone().unwrap_or_else(|| body.text.clone());
    output.text = body.text;
    output.markdown = body.markdown;
    output.chat.reply = Some(output.reply.clone());
}

fn todo_success_validation_from_agent_outcome(
    outcome: &AgentTurnOutcome,
) -> todo_guard::TodoSuccessValidation {
    let todo_write_outcomes = outcome
        .outcomes
        .iter()
        .filter(|item| is_todo_write_outcome(item))
        .collect::<Vec<_>>();
    if todo_write_outcomes.is_empty() {
        return todo_guard::TodoSuccessValidation::Passed {
            claimed_success: false,
        };
    }
    if todo_write_outcomes.iter().all(|item| {
        matches!(
            item.status,
            ToolOutcomeStatus::Succeeded | ToolOutcomeStatus::PendingConfirmation
        )
    }) {
        return todo_guard::TodoSuccessValidation::Passed {
            claimed_success: true,
        };
    }
    todo_guard::TodoSuccessValidation::Blocked
}

fn is_todo_write_outcome(outcome: &super::agent_outcome::ToolExecutionOutcome) -> bool {
    outcome.domain == "todo" && outcome.effect != ToolEffect::ReadOnly
}

fn policy_source_label(policy: &crate::config::ResolvedAgentPolicy) -> &str {
    match &policy.source {
        AgentConfigSource::BuiltInLegacy => "built_in_legacy",
        AgentConfigSource::File(path) => path.as_str(),
    }
}

fn generic_agent_error_code(
    outcome: Option<&AgentTurnOutcome>,
    use_tool_loop: bool,
    todo_success_validation: &todo_guard::TodoSuccessValidation,
) -> Option<&'static str> {
    if use_tool_loop
        && !todo_success_validation.passed()
        && outcome.is_none_or(|outcome| outcome.outcomes.is_empty())
    {
        return Some("todo_success_not_verified");
    }
    if let Some(outcome) = outcome {
        if outcome.has_unhandled_outcome() {
            return Some("tool_outcome_unhandled");
        }
        return match outcome.status {
            AgentTurnStatus::Succeeded | AgentTurnStatus::PendingConfirmation => None,
            AgentTurnStatus::PartialSuccess => Some("agent_turn_partial_success"),
            AgentTurnStatus::RequiresClarification | AgentTurnStatus::Failed => {
                Some("agent_turn_failed")
            }
        };
    }
    (use_tool_loop && !todo_success_validation.passed()).then_some("todo_success_not_verified")
}

/// 从会话历史中截取最近的 N 条消息，转换为 LLM `ChatMessage` 格式。
///
/// 仅保留 user 和 assistant 角色，按时间正序返回。
pub(super) fn recent_session_messages(session: &SessionRecord, limit: usize) -> Vec<ChatMessage> {
    session
        .history
        .iter()
        .rev()
        .filter_map(|message| match message.role.as_str() {
            "user" => Some(ChatMessage {
                role: ChatRole::User,
                content: message.content.clone(),
            }),
            "assistant" => Some(ChatMessage {
                role: ChatRole::Assistant,
                content: message.content.clone(),
            }),
            _ => None,
        })
        .filter(|message| !message.content.trim().is_empty())
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

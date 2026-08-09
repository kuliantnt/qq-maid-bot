//! Todo 接入通用 Tool Turn 后处理的 domain adapter。

use qq_maid_llm::provider::ToolExecutionResult;
use serde_json::{Map, Value, json};

use crate::{
    error::LlmError,
    runtime::{
        respond::{
            RespondRequest,
            agent_composition::AgentReplySource,
            agent_outcome::{AgentTurnOutcome, ToolEffect, ToolOutcomeStatus},
            llm_service::RespondOutput,
        },
        session::{SessionMeta, SessionRecord},
        tools::{
            TaskStore,
            agent_turn::{DomainResultProjection, DomainTurnDiagnostics, DomainTurnPostprocessor},
            todo,
        },
    },
    util::metrics::LlmMetrics,
};

/// 捕获投影前的 Todo 会话上下文和模型原始回复，避免通用 Tool Turn 调度层
/// 感知验真候选细节，也避免事实卡或工具回执干扰成功声明判定。
pub(crate) struct TodoTurnPostprocessor {
    candidate_scope: todo::success_guard::TodoSuccessVerificationScope,
    original_model_reply: String,
}

impl TodoTurnPostprocessor {
    pub(crate) fn for_request(
        req: &RespondRequest,
        session: &SessionRecord,
        original_model_reply: &str,
    ) -> Self {
        let interaction = todo::interaction_state::snapshot_for_request(req, Some(session));
        let user_text = req.effective_user_text();
        Self {
            candidate_scope: todo::success_guard::todo_success_verification_scope(
                &user_text,
                interaction.has_visible_snapshot || interaction.has_recent_operation,
            ),
            original_model_reply: original_model_reply.to_owned(),
        }
    }
}

impl DomainTurnPostprocessor for TodoTurnPostprocessor {
    fn postprocess_output(
        self: Box<Self>,
        projected_outcome: Option<&mut AgentTurnOutcome>,
        output: &mut RespondOutput,
        state_session: &mut SessionRecord,
        reply_source: AgentReplySource,
    ) -> Box<dyn DomainTurnDiagnostics> {
        // 自然语言 Agent 正常成功时模型正文是唯一主体，确定性 RelatedList 不再
        // 附加。既然列表正文没有结构化发布，就同步撤销本轮 list_todos 写入的快照，
        // 避免下一轮“第一条”命中用户未见过的编号。
        let model_primary_hides_related_list =
            matches!(reply_source, AgentReplySource::NaturalLanguageAgent)
                && !output.model_reply_empty
                && projected_outcome.as_deref().is_some_and(|outcome| {
                    outcome.has_related_list() && outcome.can_use_model_reply_as_primary()
                });

        let (validation, guard_applied) = if let Some(validation) = projected_outcome
            .as_deref()
            .and_then(success_validation_from_agent_outcome)
        {
            // 已有 Todo 写结果时让通用 composer 渲染真实成功/失败回执；这里仅记录
            // 验真状态，不能用通用 guard 文案覆盖更具体的领域错误。
            (validation, false)
        } else {
            let scope = success_verification_scope(self.candidate_scope, output);
            if matches!(
                scope,
                todo::success_guard::TodoSuccessVerificationScope::None
            ) {
                (
                    todo::success_guard::TodoSuccessValidation::Passed {
                        claimed_success: false,
                    },
                    false,
                )
            } else {
                let validation =
                    validate_model_reply_success(&self.original_model_reply, output, scope);
                let guard_applied = !validation.passed();
                if !validation.passed() {
                    apply_success_not_verified_output(output);
                }
                (validation, guard_applied)
            }
        };

        if (guard_applied || model_primary_hides_related_list)
            && let Some(outcome) = projected_outcome
        {
            let clears_list_snapshot = outcome.has_related_list();
            outcome.visible_entity_snapshot = None;
            if clears_list_snapshot {
                state_session.last_todo_query = None;
            }
        }

        Box::new(diagnostics_from_tool_results(
            &output.agent.tool_results,
            validation,
            guard_applied,
        ))
    }
}

pub(crate) fn project_results(
    task_store: &TaskStore,
    session: &mut SessionRecord,
    meta: &SessionMeta,
    results: &[ToolExecutionResult],
    attempts: &[qq_maid_llm::provider::ToolExecutionAttempt],
) -> Result<DomainResultProjection, LlmError> {
    let owner = TaskStore::owner(meta.user_id.as_deref(), &meta.scope_key);
    let aggregation =
        todo::flow::aggregate_todo_tool_results(task_store, session, &owner, results, attempts)?;
    let visible_entity_snapshot = aggregation.visible_entity_snapshot(session, meta);
    Ok(DomainResultProjection {
        consumed_result_indexes: aggregation.consumed_result_indexes,
        outcomes: aggregation.outcomes,
        visible_entity_snapshot,
    })
}

pub(crate) fn diagnostics_from_plain_output(output: &RespondOutput) -> TodoAgentDiagnostics {
    diagnostics_from_tool_results(
        &output.agent.tool_results,
        todo::success_guard::TodoSuccessValidation::Passed {
            claimed_success: false,
        },
        false,
    )
}

pub(crate) fn diagnostics_from_tool_results(
    tool_results: &[ToolExecutionResult],
    validation: todo::success_guard::TodoSuccessValidation,
    blocks_reply_composition: bool,
) -> TodoAgentDiagnostics {
    TodoAgentDiagnostics {
        validation,
        summaries: todo::success_guard::todo_tool_result_summaries(tool_results),
        blocks_reply_composition,
    }
}

fn success_validation_from_agent_outcome(
    outcome: &AgentTurnOutcome,
) -> Option<todo::success_guard::TodoSuccessValidation> {
    let todo_write_outcomes = outcome
        .outcomes
        .iter()
        .filter(|item| item.domain == "todo" && item.effect != ToolEffect::ReadOnly)
        .collect::<Vec<_>>();
    if todo_write_outcomes.is_empty() {
        // 其他领域 outcome 不能替 Todo 完成验真；调用方仍需检查模型原始回复。
        return None;
    }
    if todo_write_outcomes.iter().all(|item| {
        matches!(
            item.status,
            ToolOutcomeStatus::Succeeded | ToolOutcomeStatus::PendingConfirmation
        )
    }) {
        return Some(todo::success_guard::TodoSuccessValidation::Passed {
            claimed_success: true,
        });
    }
    Some(todo::success_guard::TodoSuccessValidation::Blocked)
}

fn validate_model_reply_success(
    original_model_reply: &str,
    output: &RespondOutput,
    scope: todo::success_guard::TodoSuccessVerificationScope,
) -> todo::success_guard::TodoSuccessValidation {
    todo::success_guard::validate_todo_success_reply(
        original_model_reply,
        &output.agent.tool_results,
        scope,
    )
}

fn success_verification_scope(
    candidate_scope: todo::success_guard::TodoSuccessVerificationScope,
    output: &RespondOutput,
) -> todo::success_guard::TodoSuccessVerificationScope {
    if !matches!(
        candidate_scope,
        todo::success_guard::TodoSuccessVerificationScope::None
    ) {
        // 输入已确定范围时保持该范围；省略式创建不能因工具痕迹扩大到完整写声明。
        return candidate_scope;
    }
    if output
        .agent
        .emitted_tools
        .iter()
        .any(|name| todo::success_guard::is_todo_write_tool(name))
        || todo::success_guard::has_todo_write_tool_result(&output.agent.tool_results)
    {
        // 一旦本轮实际涉及 Todo 写工具，完整核验所有写操作成功声明。
        todo::success_guard::TodoSuccessVerificationScope::ExplicitMutation
    } else {
        candidate_scope
    }
}

fn apply_success_not_verified_output(output: &mut RespondOutput) {
    let reply = todo::success_guard::todo_success_not_verified_reply_for_tool_results(
        &output.agent.tool_results,
    );
    output.reply = reply.clone();
    output.text = reply;
    output.markdown = None;
    output.parts.clear();
    output.metrics = LlmMetrics {
        provider: "rust".to_owned(),
        model: "tool-loop-guard".to_owned(),
        stream: false,
        ttfe_ms: None,
        ttft_ms: None,
        total_latency_ms: 0,
    };
    output.usage = None;
}

pub(crate) struct TodoAgentDiagnostics {
    validation: todo::success_guard::TodoSuccessValidation,
    summaries: Vec<todo::success_guard::TodoToolResultSummary>,
    blocks_reply_composition: bool,
}

impl DomainTurnDiagnostics for TodoAgentDiagnostics {
    fn blocks_reply_composition(&self) -> bool {
        self.blocks_reply_composition
    }

    fn log_tool_loop_results(&self, executed_tools: &[String]) {
        if self.summaries.is_empty() {
            if self.validation.claimed_success() {
                tracing::warn!(
                    entered_tool_loop = true,
                    executed_tools = ?executed_tools,
                    todo_success_claimed = true,
                    todo_success_verified = self.validation.passed(),
                    "缺少 Todo 写入 Tool 结果，已拦截成功声明"
                );
            } else {
                tracing::debug!(
                    entered_tool_loop = true,
                    executed_tools = ?executed_tools,
                    "Tool Loop 已完成，但没有 Todo 写入 Tool 结果"
                );
            }
            return;
        }

        for summary in &self.summaries {
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
                todo_success_claimed = self.validation.claimed_success(),
                todo_success_verified = self.validation.passed(),
                "Todo Tool 返回结果"
            );
        }
    }

    fn extend_response_diagnostics(&self, target: &mut Map<String, Value>) {
        target.insert(
            "todo_tool_results".to_owned(),
            json!(
                self.summaries
                    .iter()
                    .map(|summary| json!({
                        "tool": &summary.tool,
                        "succeeded": summary.succeeded,
                        "error_code": &summary.error_code,
                        "requires_confirmation": summary.requires_confirmation,
                        "requires_clarification": summary.requires_clarification,
                        "skipped": summary.skipped,
                        "skip_reason": &summary.skip_reason,
                        "pending_action": &summary.pending_action,
                    }))
                    .collect::<Vec<_>>()
            ),
        );
        target.insert(
            "todo_success_claimed".to_owned(),
            json!(self.validation.claimed_success()),
        );
        target.insert(
            "todo_success_verified".to_owned(),
            json!(self.validation.passed()),
        );
    }

    fn guard_error_code(
        &self,
        outcome: Option<&AgentTurnOutcome>,
        use_agent_runtime: bool,
    ) -> Option<&'static str> {
        if use_agent_runtime
            && !self.validation.passed()
            && outcome.is_none_or(|outcome| outcome.outcomes.is_empty())
        {
            return Some("todo_success_not_verified");
        }
        (use_agent_runtime && !self.validation.passed()).then_some("todo_success_not_verified")
    }
}

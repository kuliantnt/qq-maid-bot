//! Todo 接入通用 Tool Turn 后处理的 domain adapter。

use qq_maid_llm::provider::ToolExecutionResult;
use serde_json::{Map, Value, json};

use crate::{
    error::LlmError,
    runtime::{
        respond::{
            ChatResponse,
            agent_outcome::{AgentTurnOutcome, ToolEffect, ToolOutcomeStatus},
            llm_service::RespondOutput,
        },
        session::{SessionMeta, SessionRecord},
        tools::{
            TaskStore,
            agent_turn::{DomainTurnDiagnostics, IndexedToolOutcomes},
            todo,
        },
    },
    service::VisibleEntitySnapshot,
    util::metrics::LlmMetrics,
};

pub(crate) struct TodoAgentProjection {
    pub(crate) consumed_result_indexes: std::collections::HashSet<usize>,
    pub(crate) outcomes: IndexedToolOutcomes,
    pub(crate) visible_entity_snapshot: Option<VisibleEntitySnapshot>,
}

pub(crate) fn project_results(
    task_store: &TaskStore,
    session: &mut SessionRecord,
    meta: &SessionMeta,
    results: &[ToolExecutionResult],
) -> Result<TodoAgentProjection, LlmError> {
    let owner = TaskStore::owner(meta.user_id.as_deref(), &meta.scope_key);
    let aggregation =
        todo::flow::aggregate_todo_tool_results(task_store, session, &owner, results)?;
    let visible_entity_snapshot = aggregation.visible_entity_snapshot(session, meta);
    Ok(TodoAgentProjection {
        consumed_result_indexes: aggregation.consumed_result_indexes,
        outcomes: aggregation.outcomes,
        visible_entity_snapshot,
    })
}

pub(crate) fn diagnostics_from_plain_output(output: &RespondOutput) -> TodoAgentDiagnostics {
    diagnostics_from_tool_results(
        &output.tool_results,
        todo::success_guard::TodoSuccessValidation::Passed {
            claimed_success: false,
        },
    )
}

pub(crate) fn diagnostics_from_tool_results(
    tool_results: &[ToolExecutionResult],
    validation: todo::success_guard::TodoSuccessValidation,
) -> TodoAgentDiagnostics {
    TodoAgentDiagnostics {
        validation,
        summaries: todo::success_guard::todo_tool_result_summaries(tool_results),
    }
}

pub(crate) fn success_validation_from_agent_outcome(
    outcome: &AgentTurnOutcome,
) -> todo::success_guard::TodoSuccessValidation {
    let todo_write_outcomes = outcome
        .outcomes
        .iter()
        .filter(|item| item.domain == "todo" && item.effect != ToolEffect::ReadOnly)
        .collect::<Vec<_>>();
    if todo_write_outcomes.is_empty() {
        return todo::success_guard::TodoSuccessValidation::Passed {
            claimed_success: false,
        };
    }
    if todo_write_outcomes.iter().all(|item| {
        matches!(
            item.status,
            ToolOutcomeStatus::Succeeded | ToolOutcomeStatus::PendingConfirmation
        )
    }) {
        return todo::success_guard::TodoSuccessValidation::Passed {
            claimed_success: true,
        };
    }
    todo::success_guard::TodoSuccessValidation::Blocked
}

pub(crate) fn validate_model_reply_success(
    output: &RespondOutput,
) -> todo::success_guard::TodoSuccessValidation {
    todo::success_guard::validate_todo_success_reply(&output.reply, &output.tool_results)
}

pub(crate) fn success_not_verified_output(output: RespondOutput) -> RespondOutput {
    let reply =
        todo::success_guard::todo_success_not_verified_reply_for_tool_results(&output.tool_results);
    RespondOutput {
        reply: reply.clone(),
        text: reply.clone(),
        markdown: None,
        chat: ChatResponse::ok(
            reply,
            LlmMetrics {
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

pub(crate) struct TodoAgentDiagnostics {
    validation: todo::success_guard::TodoSuccessValidation,
    summaries: Vec<todo::success_guard::TodoToolResultSummary>,
}

impl DomainTurnDiagnostics for TodoAgentDiagnostics {
    fn log_tool_loop_results(&self, executed_tools: &[String]) {
        if self.summaries.is_empty() {
            if self.validation.claimed_success() {
                tracing::warn!(
                    entered_tool_loop = true,
                    executed_tools = ?executed_tools,
                    todo_success_claimed = true,
                    todo_success_verified = self.validation.passed(),
                    "todo success claim blocked without todo write tool result"
                );
            } else {
                tracing::debug!(
                    entered_tool_loop = true,
                    executed_tools = ?executed_tools,
                    "tool loop completed without todo write tool result"
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
                "todo tool result"
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
        use_tool_loop: bool,
    ) -> Option<&'static str> {
        if use_tool_loop
            && !self.validation.passed()
            && outcome.is_none_or(|outcome| outcome.outcomes.is_empty())
        {
            return Some("todo_success_not_verified");
        }
        (use_tool_loop && !self.validation.passed()).then_some("todo_success_not_verified")
    }
}

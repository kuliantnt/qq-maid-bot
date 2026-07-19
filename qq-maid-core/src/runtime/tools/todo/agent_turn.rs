//! Todo 接入通用 Tool Turn 后处理的 domain adapter。

use qq_maid_llm::provider::{AgentStopReason, ToolExecutionResult};
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
            agent_turn::{DomainTurnDiagnostics, IndexedToolOutcomes, ToolTurnContext},
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
        &output.agent.tool_results,
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
    todo::success_guard::validate_todo_success_reply(&output.reply, &output.agent.tool_results)
}

pub(crate) fn should_validate_success(context: &ToolTurnContext, output: &RespondOutput) -> bool {
    output
        .agent
        .emitted_tools
        .iter()
        .any(|name| todo::success_guard::is_todo_write_tool(name))
        || todo::success_guard::has_todo_write_tool_result(&output.agent.tool_results)
        || (context.semantic_domain == Some("todo")
            && context.status_subject == Some("todo")
            && matches!(context.status_action, Some("write" | "confirm" | "process")))
}

/// 最终模型轮次失败后，只在 Todo 写工具已有可信结果时构造确定性回执输入。
///
/// 这里不重跑工具，也不根据模型文案猜测成功；后续仍由通用 Tool Turn 投影读取
/// `tool_results`，按真实数据库结果生成用户可见回执。
pub(crate) fn fallback_output_after_agent_failure(
    err: &LlmError,
    model: &str,
) -> Option<RespondOutput> {
    let agent = err.agent.as_deref()?;
    if matches!(
        agent.stop_reason,
        Some(AgentStopReason::Cancelled | AgentStopReason::Timeout)
    ) || !agent.tools_with_unknown_result.is_empty()
    {
        return None;
    }
    let write_tool_started = agent
        .executed_tools
        .iter()
        .any(|name| todo::success_guard::is_todo_write_tool(name));
    if !write_tool_started || !todo::success_guard::has_todo_write_tool_result(&agent.tool_results)
    {
        return None;
    }

    let reply = "待办工具已经执行，以下回执来自真实工具结果。".to_owned();
    Some(RespondOutput {
        reply: reply.clone(),
        text: reply.clone(),
        markdown: None,
        parts: Vec::new(),
        chat: ChatResponse::ok(
            reply,
            LlmMetrics {
                provider: "rust".to_owned(),
                model: format!("{model}:todo-tool-result-fallback"),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 0,
            },
            None,
        ),
        agent: agent.clone(),
    })
}

pub(crate) fn success_not_verified_output(output: RespondOutput) -> RespondOutput {
    let reply = todo::success_guard::todo_success_not_verified_reply_for_tool_results(
        &output.agent.tool_results,
    );
    RespondOutput {
        reply: reply.clone(),
        text: reply.clone(),
        markdown: None,
        parts: Vec::new(),
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
        agent: output.agent,
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

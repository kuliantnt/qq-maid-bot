//! Tool Loop 整轮后处理。
//!
//! Respond/chat_flow 只负责发起 Tool Loop 和保存最终回复；一轮工具结果如何投影、
//! 哪些 domain 要写 session 快照、怎样补充诊断字段，都由本模块统一调度。

use serde_json::{Map, Value, json};

use crate::{
    error::LlmError,
    runtime::{
        respond::{
            agent_outcome::{AgentTurnOutcome, AgentTurnStatus, ToolExecutionOutcome},
            llm_service::RespondOutput,
        },
        session::{SessionMeta, SessionRecord, SessionStore},
        tools::{TaskStore, memory, todo},
    },
};

use super::agent_presenters::{
    tool_outcome_from_knowledge_result, tool_outcome_from_rss_result,
    tool_outcome_from_train_result, tool_outcome_from_weather_result,
    tool_outcome_from_web_search_result,
};

pub(crate) type IndexedToolOutcomes = Vec<(usize, ToolExecutionOutcome)>;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ToolTurnContext {
    pub(crate) semantic_domain: Option<&'static str>,
    pub(crate) status_subject: Option<&'static str>,
    pub(crate) status_action: Option<&'static str>,
}

pub(crate) trait DomainTurnDiagnostics {
    fn log_tool_loop_results(&self, executed_tools: &[String]);
    fn extend_response_diagnostics(&self, target: &mut Map<String, Value>);
    fn guard_error_code(
        &self,
        _outcome: Option<&AgentTurnOutcome>,
        _use_agent_runtime: bool,
    ) -> Option<&'static str> {
        None
    }
}

pub(crate) struct ToolTurnPostprocess {
    pub(crate) output: RespondOutput,
    pub(crate) outcome: AgentTurnOutcome,
    pub(crate) diagnostics: ToolTurnDiagnostics,
}

pub(crate) struct ToolTurnDiagnostics {
    domains: Vec<Box<dyn DomainTurnDiagnostics>>,
}

impl ToolTurnDiagnostics {
    pub(crate) fn from_plain_output(output: &RespondOutput) -> Self {
        Self {
            domains: vec![Box::new(todo::agent_turn::diagnostics_from_plain_output(
                output,
            ))],
        }
    }

    pub(crate) fn log_tool_loop_results(&self, executed_tools: &[String]) {
        for domain in &self.domains {
            domain.log_tool_loop_results(executed_tools);
        }
    }

    pub(crate) fn extend_response_diagnostics(&self, target: &mut Map<String, Value>) {
        for domain in &self.domains {
            domain.extend_response_diagnostics(target);
        }
    }

    fn guard_error_code(
        &self,
        outcome: Option<&AgentTurnOutcome>,
        use_agent_runtime: bool,
    ) -> Option<&'static str> {
        self.domains
            .iter()
            .find_map(|domain| domain.guard_error_code(outcome, use_agent_runtime))
    }
}

pub(crate) fn postprocess_tool_turn(
    session_store: &SessionStore,
    task_store: &TaskStore,
    conversation_session: &mut SessionRecord,
    meta: &SessionMeta,
    interaction_meta: &SessionMeta,
    mut output: RespondOutput,
    context: ToolTurnContext,
) -> Result<ToolTurnPostprocess, LlmError> {
    let mut standalone_interaction = if interaction_meta.scope_key != meta.scope_key {
        Some(
            session_store
                .get_or_create_active(interaction_meta)
                .map_err(crate::runtime::respond::common::session_error)?,
        )
    } else {
        None
    };
    let state_session = standalone_interaction
        .as_mut()
        .unwrap_or(conversation_session);

    let outcome = project_tool_turn(task_store, state_session, meta, &output)?;
    if let Some(interaction) = standalone_interaction.as_mut() {
        session_store
            .save(interaction)
            .map_err(crate::runtime::respond::common::session_error)?;
    }

    let todo_guard_enabled = todo::agent_turn::should_validate_success(&context, &output);
    let validation = if outcome.can_replace_model_reply() {
        if outcome.should_preserve_model_reply() {
            apply_agent_turn_outcome_with_model_reply(&mut output, &outcome);
        } else {
            apply_agent_turn_outcome(&mut output, &outcome);
        }
        todo::agent_turn::success_validation_from_agent_outcome(&outcome)
    } else if outcome.has_unhandled_outcome() && !outcome.outcomes.is_empty() {
        apply_agent_turn_compat_output(&mut output, &outcome);
        todo::agent_turn::success_validation_from_agent_outcome(&outcome)
    } else if todo_guard_enabled {
        let validation = todo::agent_turn::validate_model_reply_success(&output);
        if !validation.passed() {
            output = todo::agent_turn::success_not_verified_output(output);
        }
        validation
    } else {
        todo::success_guard::TodoSuccessValidation::Passed {
            claimed_success: false,
        }
    };
    let diagnostics = ToolTurnDiagnostics {
        domains: vec![Box::new(todo::agent_turn::diagnostics_from_tool_results(
            &output.agent.tool_results,
            validation,
        ))],
    };
    Ok(ToolTurnPostprocess {
        output,
        outcome,
        diagnostics,
    })
}

pub(crate) fn agent_turn_diagnostics(outcome: Option<&AgentTurnOutcome>) -> Value {
    outcome
        .map(AgentTurnOutcome::diagnostics)
        .unwrap_or_else(|| {
            json!({
                "agent_turn_status": Value::Null,
                "tool_outcomes": [],
            })
        })
}

pub(crate) fn tool_turn_error_code(
    outcome: Option<&AgentTurnOutcome>,
    use_agent_runtime: bool,
    diagnostics: &ToolTurnDiagnostics,
) -> Option<&'static str> {
    if let Some(error_code) = diagnostics.guard_error_code(outcome, use_agent_runtime) {
        return Some(error_code);
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
    None
}

fn project_tool_turn(
    task_store: &TaskStore,
    session: &mut SessionRecord,
    meta: &SessionMeta,
    output: &RespondOutput,
) -> Result<AgentTurnOutcome, LlmError> {
    let todo_projection = todo::agent_turn::project_results(
        task_store,
        session,
        meta,
        &output.agent.tool_results,
        &output.agent.tool_attempts,
    )?;
    let visible_entity_snapshot = todo_projection.visible_entity_snapshot;
    let mut outcomes = Vec::new();
    let mut todo_outcomes = todo_projection.outcomes.into_iter().peekable();

    for (index, result) in output.agent.tool_results.iter().enumerate() {
        if is_retry_superseded_result(index, &output.agent.tool_attempts) {
            // 旧尝试仍保留在 Agent diagnostics；这里只丢弃它对应的用户展示块。
            let mut discarded = Vec::new();
            drain_todo_outcomes_for_result(index, &mut todo_outcomes, &mut discarded);
            continue;
        }
        if todo_projection.consumed_result_indexes.contains(&index) {
            drain_todo_outcomes_for_result(index, &mut todo_outcomes, &mut outcomes);
        } else if let Some(outcome) = tool_outcome_from_weather_result(result) {
            outcomes.push(outcome);
        } else if let Some(outcome) = tool_outcome_from_train_result(result) {
            outcomes.push(outcome);
        } else if let Some(outcome) = tool_outcome_from_rss_result(result) {
            outcomes.push(outcome);
        } else if let Some(outcome) = tool_outcome_from_web_search_result(result) {
            outcomes.push(outcome);
        } else if let Some(outcome) = tool_outcome_from_knowledge_result(result) {
            outcomes.push(outcome);
        } else if let Some(outcome) = memory::agent_turn::tool_outcome_from_result(result) {
            outcomes.push(outcome);
        } else {
            outcomes.push(ToolExecutionOutcome::generic(result));
        }
    }
    outcomes.extend(todo_outcomes.map(|(_, outcome)| outcome));

    Ok(AgentTurnOutcome::from_outcomes_with_visible_snapshot(
        outcomes,
        visible_entity_snapshot,
    ))
}

pub(crate) fn is_retry_superseded_result(
    result_index: usize,
    attempts: &[qq_maid_llm::provider::ToolExecutionAttempt],
) -> bool {
    attempts
        .iter()
        .any(|attempt| attempt.retry_of == Some(result_index))
}

fn drain_todo_outcomes_for_result(
    result_index: usize,
    todo_outcomes: &mut std::iter::Peekable<impl Iterator<Item = (usize, ToolExecutionOutcome)>>,
    outcomes: &mut Vec<ToolExecutionOutcome>,
) {
    while todo_outcomes
        .peek()
        .is_some_and(|(outcome_index, _)| *outcome_index == result_index)
    {
        if let Some((_, outcome)) = todo_outcomes.next() {
            outcomes.push(outcome);
        }
    }
}

fn apply_agent_turn_outcome(output: &mut RespondOutput, outcome: &AgentTurnOutcome) {
    let body = outcome.render_body();
    output.reply = body.markdown.clone().unwrap_or_else(|| body.text.clone());
    output.text = body.text;
    output.markdown = body.markdown;
}

fn apply_agent_turn_outcome_with_model_reply(
    output: &mut RespondOutput,
    outcome: &AgentTurnOutcome,
) {
    let body = outcome.render_body();
    let model_text = output.reply.trim();
    if model_text.is_empty() {
        apply_agent_turn_outcome(output, outcome);
        return;
    }

    let text = if body.text.trim().is_empty() {
        model_text.to_owned()
    } else {
        format!("{}\n\n{}", body.text.trim(), model_text)
    };
    let markdown = body
        .markdown
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|markdown| format!("{}\n\n{}", markdown.trim(), model_text));
    output.reply = markdown.clone().unwrap_or_else(|| text.clone());
    output.text = text;
    output.markdown = markdown;
}

fn apply_agent_turn_compat_output(output: &mut RespondOutput, outcome: &AgentTurnOutcome) {
    let body = outcome.render_compat_body();
    output.reply = body.markdown.clone().unwrap_or_else(|| body.text.clone());
    output.text = body.text;
    output.markdown = body.markdown;
}

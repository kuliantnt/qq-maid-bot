//! Tool Loop 整轮后处理。
//!
//! Respond/chat_flow 只负责发起 Tool Loop 和保存最终回复；一轮工具结果如何投影、
//! 哪些 domain 要写 session 快照、怎样补充诊断字段，都由本模块统一调度。

use serde_json::{Map, Value, json};

use qq_maid_llm::agent_loop::AgentStopReason;

use crate::{
    error::LlmError,
    runtime::{
        respond::{
            RespondRequest,
            agent_composition::{AgentReplySource, compose_tool_turn_output},
            agent_outcome::{AgentTurnOutcome, AgentTurnStatus, ToolExecutionOutcome},
            llm_service::RespondOutput,
        },
        session::{SessionMeta, SessionRecord, SessionStore},
        tools::{TaskStore, memory, todo},
    },
    service::VisibleEntitySnapshot,
    util::metrics::LlmMetrics,
};

use super::agent_presenters::{
    tool_outcome_from_knowledge_result, tool_outcome_from_rss_result,
    tool_outcome_from_weather_result,
};
use super::search::agent_turn::project_results as project_search_results;

pub(crate) type IndexedToolOutcomes = Vec<(usize, ToolExecutionOutcome)>;

/// 单个业务域对整轮 Tool 结果的结构化投影。
///
/// 领域只报告它消费的结果和对应回执；跨领域的原始调用顺序、重试覆盖和未知 Tool
/// 回退统一由本模块处理，避免把整轮编排错误地下沉到任一业务域。
pub(crate) struct DomainResultProjection {
    pub(crate) consumed_result_indexes: std::collections::HashSet<usize>,
    pub(crate) outcomes: IndexedToolOutcomes,
    pub(crate) visible_entity_snapshot: Option<VisibleEntitySnapshot>,
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

    fn blocks_reply_composition(&self) -> bool {
        false
    }
}

/// 领域后处理器只接收通用 Tool Turn 结果，并自行完成领域验真与诊断构造。
///
/// 通用调度层不读取领域候选、成功声明或工具名称等具体规则。
pub(crate) trait DomainTurnPostprocessor {
    fn postprocess_output(
        self: Box<Self>,
        projected_outcome: Option<&mut AgentTurnOutcome>,
        output: &mut RespondOutput,
        state_session: &mut SessionRecord,
        reply_source: AgentReplySource,
    ) -> Box<dyn DomainTurnDiagnostics>;
}

pub(crate) struct ToolTurnPostprocess {
    pub(crate) output: RespondOutput,
    pub(crate) outcome: AgentTurnOutcome,
    pub(crate) diagnostics: ToolTurnDiagnostics,
}

pub(crate) struct ToolTurnDiagnostics {
    domains: Vec<Box<dyn DomainTurnDiagnostics>>,
}

/// 一次 Tool Turn 后处理所需的共享上下文。
///
/// 回复来源是调用方明确选择的 typed value，和 session / domain 上下文一起传入；
/// 不从 request 文本、工具名称或执行结果反推展示语义。
pub(crate) struct ToolTurnPostprocessContext<'a> {
    pub(crate) session_store: &'a SessionStore,
    pub(crate) task_store: &'a TaskStore,
    pub(crate) conversation_session: &'a mut SessionRecord,
    pub(crate) meta: &'a SessionMeta,
    pub(crate) interaction_meta: &'a SessionMeta,
    pub(crate) req: &'a RespondRequest,
    pub(crate) reply_source: AgentReplySource,
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

    fn blocks_reply_composition(&self) -> bool {
        self.domains
            .iter()
            .any(|domain| domain.blocks_reply_composition())
    }
}

pub(crate) fn postprocess_tool_turn(
    context: ToolTurnPostprocessContext<'_>,
    mut output: RespondOutput,
) -> Result<ToolTurnPostprocess, LlmError> {
    let ToolTurnPostprocessContext {
        session_store,
        task_store,
        conversation_session,
        meta,
        interaction_meta,
        req,
        reply_source,
    } = context;
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

    let domain_postprocessors: Vec<Box<dyn DomainTurnPostprocessor>> = vec![Box::new(
        todo::agent_turn::TodoTurnPostprocessor::for_request(req, state_session, &output.reply),
    )];

    let mut outcome = project_tool_turn(task_store, state_session, meta, &output)?;
    if output.model_reply_empty
        && !outcome.has_incomplete_result()
        && !outcome.has_unhandled_outcome()
        && !outcome.can_render_deterministic_reply()
    {
        // 最终模型正文为空时，Internal outcome 无法被完整降级。至少有可信正文
        // 的混合结果属于部分成功；完全没有可展示正文则不能继续标记整轮成功。
        outcome.status = if outcome.has_renderable_deterministic_body() {
            AgentTurnStatus::PartialSuccess
        } else {
            AgentTurnStatus::Failed
        };
    }

    // 即使最终模型正文为空或失败，也必须把完整 Tool Turn 投影成 outcome，供
    // session 快照、Todo 验真、来源和确定性 fallback 使用；只有完全没有结果时
    // 才视为普通模型回复，不进入 domain presentation。
    let has_projected_outcome = !outcome.outcomes.is_empty() || outcome.has_incomplete_result();
    let mut domains = Vec::with_capacity(domain_postprocessors.len());
    for postprocessor in domain_postprocessors {
        domains.push(postprocessor.postprocess_output(
            has_projected_outcome.then_some(&mut outcome),
            &mut output,
            state_session,
            reply_source,
        ));
    }
    let diagnostics = ToolTurnDiagnostics { domains };
    // 领域 guard 必须先于正文合成执行；如果 guard 已生成安全回复，composer 不能
    // 再用模型正文或确定性列表覆盖它。
    if has_projected_outcome && !diagnostics.blocks_reply_composition() {
        compose_tool_turn_output(&mut output, &outcome, reply_source);
    }
    if let Some(interaction) = standalone_interaction.as_mut() {
        session_store
            .save(interaction)
            .map_err(crate::runtime::respond::common::session_error)?;
    }
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
        if outcome.has_incomplete_result() {
            return Some("agent_turn_incomplete");
        }
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
    let search_projection =
        project_search_results(&output.agent.tool_results, &output.agent.tool_attempts);
    let search_provenance = search_projection.provenance;
    let search_consumed_result_indexes = search_projection.consumed_result_indexes;
    let mut outcomes = Vec::new();
    let mut todo_outcomes = todo_projection.outcomes.into_iter().peekable();
    let mut search_outcomes = search_projection.outcomes.into_iter().peekable();

    for (index, result) in output.agent.tool_results.iter().enumerate() {
        if is_retry_superseded_result(index, &output.agent.tool_attempts) {
            let mut discarded = Vec::new();
            drain_domain_outcomes_for_result(index, &mut todo_outcomes, &mut discarded);
            drain_domain_outcomes_for_result(index, &mut search_outcomes, &mut discarded);
            continue;
        }
        if todo_projection.consumed_result_indexes.contains(&index) {
            drain_domain_outcomes_for_result(index, &mut todo_outcomes, &mut outcomes);
        } else if search_consumed_result_indexes.contains(&index) {
            drain_domain_outcomes_for_result(index, &mut search_outcomes, &mut outcomes);
        } else if let Some(outcome) = tool_outcome_from_weather_result(result) {
            outcomes.push(outcome);
        } else if let Some(outcome) = super::train::tool_outcome_from_result(result) {
            outcomes.push(outcome);
        } else if let Some(outcome) = tool_outcome_from_rss_result(result) {
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
    outcomes.extend(search_outcomes.map(|(_, outcome)| outcome));

    let mut outcome =
        AgentTurnOutcome::from_outcomes_with_visible_snapshot_and_provenance_and_incomplete(
            outcomes,
            visible_entity_snapshot,
            search_provenance,
            output.agent.tools_with_unknown_result.clone(),
            output.agent.has_incomplete_tool_loop(),
        );
    outcome.published_tool_result_indexes = published_tool_result_indexes(output);
    Ok(outcome)
}

fn published_tool_result_indexes(output: &RespondOutput) -> Vec<usize> {
    let declared_call_ids = output
        .display_contract
        .published_tool_call_ids
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    if declared_call_ids.is_empty() {
        return Vec::new();
    }

    // 候选链和重试都可能把轨迹追加到同一份 diagnostics；同一 call id 只取
    // 最后一次尝试，避免最终候选声明一个复用的 id 时误绑定前一候选结果。
    let mut seen_call_ids = std::collections::HashSet::new();
    let mut indexes = output
        .agent
        .tool_attempts
        .iter()
        .rev()
        .filter(|attempt| {
            declared_call_ids.contains(attempt.call_id.as_str())
                && seen_call_ids.insert(attempt.call_id.as_str())
        })
        .filter_map(|attempt| {
            (attempt.result_index < output.agent.tool_results.len()).then_some(attempt.result_index)
        })
        .collect::<Vec<_>>();
    indexes.sort_unstable();
    indexes
}

fn drain_domain_outcomes_for_result(
    result_index: usize,
    domain_outcomes: &mut std::iter::Peekable<impl Iterator<Item = (usize, ToolExecutionOutcome)>>,
    outcomes: &mut Vec<ToolExecutionOutcome>,
) {
    while domain_outcomes
        .peek()
        .is_some_and(|(outcome_index, _)| *outcome_index == result_index)
    {
        if let Some((_, outcome)) = domain_outcomes.next() {
            outcomes.push(outcome);
        }
    }
}

/// 重试后的旧结果仍保留在原始 Agent 轨迹中，但不能再参与用户展示或领域回执。
/// 这是所有领域共用的 Tool Loop 语义，不属于 Todo 专用逻辑。
pub(crate) fn is_retry_superseded_result(
    result_index: usize,
    attempts: &[qq_maid_llm::provider::ToolExecutionAttempt],
) -> bool {
    attempts
        .iter()
        .any(|attempt| attempt.retry_of == Some(result_index))
}

/// 最终模型轮次失败后，使用已经形成结果的工具轨迹作为候选回退。
///
/// 最终正文失败可以直接回退；如果 Tool Loop 在仍有未完成调用或未知副作用时终止，
/// 后续 projection 会消费同一份 typed 完整性元数据并追加对应警告。这里不读取工具
/// 名称、不编造业务成功文案，也不接受取消；只在已经形成至少一个工具结果时回退。
pub(crate) fn fallback_output_after_agent_failure(
    err: &LlmError,
    model: &str,
) -> Option<RespondOutput> {
    let agent = err.agent.as_deref()?;
    if matches!(agent.stop_reason, Some(AgentStopReason::Cancelled))
        || agent.tool_results.is_empty()
    {
        return None;
    }

    Some(RespondOutput {
        reply: String::new(),
        text: String::new(),
        markdown: None,
        parts: Vec::new(),
        metrics: LlmMetrics {
            provider: "rust".to_owned(),
            model: format!("{model}:agent-tool-result-fallback"),
            stream: false,
            ttfe_ms: None,
            ttft_ms: None,
            total_latency_ms: 0,
        },
        usage: None,
        agent: agent.clone(),
        display_contract: Default::default(),
        model_reply_empty: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qq_maid_llm::agent_loop::{AgentRunDiagnostics, AgentStopReason, ToolExecutionResult};

    #[test]
    fn cancelled_agent_never_enters_tool_result_fallback() {
        let diagnostics = AgentRunDiagnostics {
            stop_reason: Some(AgentStopReason::Cancelled),
            tool_results: vec![ToolExecutionResult {
                name: "create_todo".to_owned(),
                output: serde_json::json!({"ok": true}),
                succeeded: true,
            }],
            ..AgentRunDiagnostics::default()
        };
        let error =
            LlmError::new("cancelled", "request cancelled", "agent").with_agent(diagnostics);

        assert!(fallback_output_after_agent_failure(&error, "model").is_none());
    }

    #[test]
    fn incomplete_tool_loop_keeps_known_results_for_incomplete_presentation() {
        let diagnostics = AgentRunDiagnostics {
            emitted_tools: vec!["get_weather".to_owned(), "tool_b".to_owned()],
            executed_tools: vec!["get_weather".to_owned()],
            tool_execution_attempted: true,
            tool_results: vec![ToolExecutionResult {
                name: "get_weather".to_owned(),
                output: serde_json::json!({"ok": true}),
                succeeded: true,
            }],
            stop_reason: Some(AgentStopReason::Failed),
            ..AgentRunDiagnostics::default()
        };
        let error = LlmError::new(
            "tool_calls_disabled",
            "provider returned another tool call while finalizing",
            "tool_loop",
        )
        .with_agent(diagnostics);

        let output = fallback_output_after_agent_failure(&error, "model")
            .expect("known results remain available for incomplete presentation");

        assert!(output.agent.has_incomplete_tool_loop());
        assert_eq!(output.agent.tool_results.len(), 1);
        assert_eq!(output.agent.tool_results[0].name, "get_weather");
        assert!(output.model_reply_empty);
    }

    #[test]
    fn unknown_side_effect_does_not_block_known_result_fallback() {
        let diagnostics = AgentRunDiagnostics {
            emitted_tools: vec!["get_weather".to_owned(), "create_todo".to_owned()],
            executed_tools: vec!["get_weather".to_owned(), "create_todo".to_owned()],
            tool_execution_attempted: true,
            tool_results: vec![ToolExecutionResult {
                name: "get_weather".to_owned(),
                output: serde_json::json!({"ok": true}),
                succeeded: true,
            }],
            tools_with_unknown_result: vec!["create_todo".to_owned()],
            stop_reason: Some(AgentStopReason::Failed),
            ..AgentRunDiagnostics::default()
        };
        let error = LlmError::new(
            "tool_execution_failed",
            "side effect result is unknown",
            "tool_loop",
        )
        .with_agent(diagnostics);

        let output = fallback_output_after_agent_failure(&error, "model")
            .expect("known result should remain available with an unknown side effect");

        assert_eq!(output.agent.tool_results.len(), 1);
        assert_eq!(
            output.agent.tools_with_unknown_result,
            vec!["create_todo".to_owned()]
        );
    }
}

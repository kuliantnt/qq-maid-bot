//! Todo 接入通用 Tool Turn 后处理的 domain adapter。

use std::collections::HashSet;

use qq_maid_llm::provider::ToolExecutionResult;
use serde_json::{Map, Value, json};

use crate::{
    error::LlmError,
    runtime::{
        respond::{
            RespondRequest,
            agent_composition::AgentReplySource,
            agent_outcome::{
                AgentTurnOutcome, ResponseBlock, ToolEffect, ToolExecutionOutcome,
                ToolOutcomeStatus,
            },
            common::{CommandBody, join_body_text},
            llm_service::RespondOutput,
        },
        session::{SessionMeta, SessionRecord},
        tools::{
            TaskStore,
            agent_turn::{
                DomainResultProjection, DomainTurnDiagnostics, DomainTurnPostprocessor,
                is_retry_superseded_result,
            },
            todo,
        },
    },
    util::metrics::LlmMetrics,
};

/// 捕获投影前的 Todo 会话上下文和模型原始回复，避免通用 Tool Turn 调度层
/// 感知验真候选细节，也避免事实卡或工具回执干扰成功声明判定。
pub(crate) struct TodoTurnPostprocessor {
    candidate_scope: todo::success_guard::TodoSuccessVerificationScope,
    /// Todo 成功验真仍需检查模型是否声称写入成功；这里只不再用它推断列表是否展示。
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
        // 自然语言 Agent 的非空模型正文通常是唯一主体；只有模型通过结构化展示
        // 契约声明实际展示了成功的 list_todos 结果时，RelatedList 对应的快照才
        // 继续有效。不能因为正文非空，或正文看起来像列表，就推断用户看到了编号。
        let published_list_indexes = published_todo_list_result_indexes(output);
        let has_published_list = projected_outcome.as_deref().is_some_and(|outcome| {
            outcome.has_related_list() && !published_list_indexes.is_empty()
        });
        let has_display_contract = !output.display_contract.published_tool_call_ids.is_empty();
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

        // `published_tool_call_ids` 只是模型选择的结果索引；真正出站的编号列表由
        // Todo renderer 生成。这样模型即使只返回“查询完成”也不能伪造用户已看到编号。
        if !guard_applied
            && has_display_contract
            && has_published_list
            && let Some(outcome) = projected_outcome.as_deref()
        {
            append_published_todo_lists(output, outcome);
        }

        // `aggregate_todo_tool_results` 已用同一批真实条目建立 RelatedList 与快照。
        // 只有该结构化列表实际由 Todo renderer 发布时，快照才拥有跨轮编号的展示契约。
        if let Some(outcome) = projected_outcome {
            let has_todo_list_result = output
                .agent
                .tool_results
                .iter()
                .any(|result| result.name == todo::LIST_TODOS_TOOL_NAME);
            let unique_published_list = has_published_list && published_list_indexes.len() == 1;
            let clears_list_snapshot = outcome.has_related_list()
                && (guard_applied
                    || (has_display_contract && has_todo_list_result && !unique_published_list)
                    || (!has_display_contract
                        && model_primary_hides_related_list
                        && !unique_published_list));
            if clears_list_snapshot {
                outcome.visible_entity_snapshot = None;
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

fn published_todo_list_result_indexes(output: &RespondOutput) -> Vec<usize> {
    published_todo_list_result_indexes_from_trace(
        &output.agent.tool_results,
        &output.agent.tool_attempts,
        &output.display_contract.published_tool_call_ids,
    )
}

pub(crate) fn project_results(
    task_store: &TaskStore,
    session: &mut SessionRecord,
    meta: &SessionMeta,
    results: &[ToolExecutionResult],
    attempts: &[qq_maid_llm::provider::ToolExecutionAttempt],
    published_tool_call_ids: &[String],
) -> Result<DomainResultProjection, LlmError> {
    let owner = TaskStore::owner(meta.user_id.as_deref(), &meta.scope_key);
    let published_list_indexes =
        published_todo_list_result_indexes_from_trace(results, attempts, published_tool_call_ids);
    let mut aggregation = todo::flow::aggregate_todo_tool_results(
        task_store,
        session,
        &owner,
        results,
        attempts,
        &published_list_indexes,
    )?;
    let projected_published_list_indexes = published_list_indexes
        .iter()
        .copied()
        .filter(|index| {
            aggregation
                .list_snapshots
                .iter()
                .any(|(snapshot_index, _)| snapshot_index == index)
        })
        .collect::<Vec<_>>();
    if !projected_published_list_indexes.is_empty() {
        let published = projected_published_list_indexes
            .iter()
            .collect::<HashSet<_>>();
        aggregation.outcomes.retain(|(index, outcome)| {
            !outcome_has_related_list(outcome) || published.contains(index)
        });
    }

    let visible_entity_snapshot = match projected_published_list_indexes.as_slice() {
        [index] if published_list_indexes.len() == 1 => {
            if let Some((_, snapshot)) = aggregation
                .list_snapshots
                .iter()
                .find(|(snapshot_index, _)| snapshot_index == index)
            {
                // 同一轮可能先后查询多个列表；发布前一个列表时，恢复它对应的
                // 快照，不能继续使用最后一次内部写入的列表。
                session.last_todo_query = Some(snapshot.clone());
            }
            todo::todo_visible_entity_snapshot(session, Some(meta))
        }
        _ if !published_list_indexes.is_empty() => {
            // 一个 session 只能保存一份编号列表；无法唯一映射时清除快照，
            // 避免下一轮“第一条”误操作未明确展示的列表。
            session.last_todo_query = None;
            None
        }
        _ => aggregation.visible_entity_snapshot(session, meta),
    };
    Ok(DomainResultProjection {
        consumed_result_indexes: aggregation.consumed_result_indexes,
        outcomes: aggregation.outcomes,
        visible_entity_snapshot,
    })
}

fn published_todo_list_result_indexes_from_trace(
    results: &[ToolExecutionResult],
    attempts: &[qq_maid_llm::provider::ToolExecutionAttempt],
    published_tool_call_ids: &[String],
) -> Vec<usize> {
    let declared_call_ids = published_tool_call_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if declared_call_ids.is_empty() {
        return Vec::new();
    }

    // 同一个 call id 可能因重试出现在多条轨迹中，按最后一次尝试绑定真实结果。
    let mut seen_call_ids = HashSet::new();
    let mut indexes = attempts
        .iter()
        .rev()
        .filter(|attempt| {
            declared_call_ids.contains(attempt.call_id.as_str())
                && seen_call_ids.insert(attempt.call_id.as_str())
        })
        .filter_map(|attempt| {
            (attempt.result_index < results.len()).then_some(attempt.result_index)
        })
        .filter(|index| {
            let result = &results[*index];
            result.name == todo::LIST_TODOS_TOOL_NAME
                && result.succeeded
                && !is_retry_superseded_result(*index, attempts)
        })
        .collect::<Vec<_>>();
    indexes.sort_unstable();
    indexes
}

fn outcome_has_related_list(outcome: &ToolExecutionOutcome) -> bool {
    outcome
        .blocks
        .iter()
        .any(|block| matches!(block, ResponseBlock::RelatedList(_)))
}

fn append_published_todo_lists(output: &mut RespondOutput, outcome: &AgentTurnOutcome) {
    let mut text_parts = Vec::new();
    let mut markdown_parts = Vec::new();
    for item in &outcome.outcomes {
        for block in &item.blocks {
            let ResponseBlock::RelatedList(body) = block else {
                continue;
            };
            if !body.text.trim().is_empty() {
                text_parts.push(body.text.trim().to_owned());
            }
            let markdown = body
                .markdown
                .as_deref()
                .unwrap_or(body.text.as_str())
                .trim();
            if !markdown.is_empty() {
                markdown_parts.push(markdown.to_owned());
            }
        }
    }
    if text_parts.is_empty() {
        return;
    }

    let body = CommandBody {
        text: text_parts.join("\n\n"),
        markdown: (!markdown_parts.is_empty()).then(|| markdown_parts.join("\n\n")),
    };
    let model_text = output.text.trim();
    let model_markdown = output
        .markdown
        .as_deref()
        .unwrap_or(output.reply.as_str())
        .trim();
    output.text = join_body_text(model_text, body.text.trim());
    let markdown = body
        .markdown
        .as_deref()
        .unwrap_or(body.text.as_str())
        .trim();
    let composed_markdown = join_body_text(model_markdown, markdown);
    output.reply = composed_markdown.clone();
    output.markdown = Some(composed_markdown);
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

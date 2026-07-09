//! Tool Outcome Projection Adapter 调度层。
//!
//! 本模块只负责把一轮 Tool Loop 的原始执行结果分派给各 domain adapter，
//! 生成通用 `AgentTurnOutcome`。具体业务字段解析、回执渲染和 session 快照写入
//! 仍留在各 domain adapter 内，避免聊天主流程继续理解 Todo/RSS/Weather/Train 细节。

use crate::{
    error::LlmError,
    runtime::{
        session::SessionRecord,
        tools::todo::{TodoOwner, TodoStore, flow::aggregate_todo_tool_results},
    },
};

use super::{
    agent_outcome::{AgentTurnOutcome, ResponseBlock, ToolExecutionOutcome, ToolOutcomeStatus},
    llm_service::RespondOutput,
    tool_presenters::{
        tool_outcome_from_rss_result, tool_outcome_from_train_result,
        tool_outcome_from_weather_result, tool_outcome_from_web_search_result,
    },
};

pub(super) fn project_tool_turn(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    output: &RespondOutput,
) -> Result<AgentTurnOutcome, LlmError> {
    let todo_aggregation =
        aggregate_todo_tool_results(todo_store, session, owner, &output.tool_results)?;
    let mut outcomes = Vec::new();
    let mut todo_outcomes = todo_aggregation.outcomes.into_iter().peekable();

    for (index, result) in output.tool_results.iter().enumerate() {
        if todo_aggregation.consumed_result_indexes.contains(&index) {
            drain_todo_outcomes_for_result(index, &mut todo_outcomes, &mut outcomes);
        } else if let Some(outcome) = tool_outcome_from_weather_result(result) {
            outcomes.push(outcome);
        } else if let Some(outcome) = tool_outcome_from_train_result(result) {
            outcomes.push(outcome);
        } else if let Some(outcome) = tool_outcome_from_rss_result(result) {
            outcomes.push(outcome);
        } else if let Some(outcome) = tool_outcome_from_web_search_result(result) {
            outcomes.push(outcome);
        } else {
            outcomes.push(ToolExecutionOutcome::generic(result));
        }
    }
    outcomes.extend(todo_outcomes.map(|(_, outcome)| outcome));

    Ok(AgentTurnOutcome::from_outcomes(outcomes))
}

pub(super) fn turn_shows_todo_visible_list(outcome: Option<&AgentTurnOutcome>) -> bool {
    outcome.is_some_and(|outcome| {
        outcome.outcomes.iter().any(|item| {
            item.domain == "todo"
                && item.status == ToolOutcomeStatus::Succeeded
                && item
                    .blocks
                    .iter()
                    .any(|block| matches!(block, ResponseBlock::RelatedList(_)))
        })
    })
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

//! Todo Tool 结果的整轮聚合。
//!
//! 这里负责筛选 Todo 结果、处理重试覆盖和刷新可见快照；单个结果的确定性回执
//! 仍由 `receipt` 门面中的投影与 receipt helper 负责。

use std::collections::HashSet;

use qq_maid_llm::provider::{ToolExecutionAttempt, ToolExecutionResult};
use serde_json::Value;

use crate::{
    error::LlmError,
    runtime::{
        respond::agent_outcome::{
            ResponseBlock, ToolEffect, ToolExecutionOutcome, ToolOutcomeStatus,
        },
        session::{SessionMeta, SessionRecord},
        tools::{
            agent_turn::is_retry_superseded_result,
            todo::{
                TodoOwner, TodoStore, todo_last_action_visible_entity_snapshot,
                todo_visible_entity_snapshot,
            },
        },
    },
    service::VisibleEntitySnapshot,
};

use super::super::scope::TodoToolScope;
use super::LIST_TODOS_TOOL_NAME;

pub(crate) struct TodoTurnAggregation {
    pub consumed_result_indexes: HashSet<usize>,
    pub outcomes: Vec<(usize, ToolExecutionOutcome)>,
}

impl TodoTurnAggregation {
    pub(crate) fn visible_entity_snapshot(
        &self,
        session: &SessionRecord,
        meta: &SessionMeta,
    ) -> Option<VisibleEntitySnapshot> {
        if self.turn_shows_visible_list() {
            return todo_visible_entity_snapshot(session, Some(meta));
        }
        if self.has_successful_single_action() {
            return todo_last_action_visible_entity_snapshot(session, Some(meta));
        }
        None
    }

    fn turn_shows_visible_list(&self) -> bool {
        self.outcomes.iter().any(|(_, item)| {
            item.domain == "todo"
                && item.status == ToolOutcomeStatus::Succeeded
                && item
                    .blocks
                    .iter()
                    .any(|block| matches!(block, ResponseBlock::RelatedList(_)))
        })
    }

    fn has_successful_single_action(&self) -> bool {
        self.outcomes
            .iter()
            .filter(|(_, item)| {
                item.domain == "todo"
                    && item.status == ToolOutcomeStatus::Succeeded
                    && matches!(
                        item.effect,
                        ToolEffect::Created | ToolEffect::Updated | ToolEffect::Completed
                    )
            })
            .count()
            == 1
    }
}

pub(crate) fn aggregate_todo_tool_results(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    results: &[ToolExecutionResult],
    attempts: &[ToolExecutionAttempt],
) -> Result<TodoTurnAggregation, LlmError> {
    let todo_indexes = results
        .iter()
        .enumerate()
        .filter_map(|(index, result)| is_todo_tool_result(result).then_some(index))
        .collect::<Vec<_>>();
    let consumed_result_indexes = todo_indexes.iter().copied().collect::<HashSet<_>>();
    let mut outcomes = Vec::new();
    for index in todo_indexes.iter().copied() {
        let result = &results[index];
        if todo_validation_failure_was_corrected(index, results, attempts) {
            // 原始失败继续保留在 Agent diagnostics 中；这里只避免把模型已经自纠的
            // 参数错误投影给用户，成功回执仍必须来自后续真实 Todo Tool 结果。
            continue;
        }
        let pending_query = if result.name == LIST_TODOS_TOOL_NAME {
            if is_retry_superseded_result(index, attempts) {
                None
            } else {
                attempts
                    .iter()
                    .find(|attempt| attempt.result_index == index)
                    .and_then(|attempt| {
                        TodoToolScope::consume_internal_query(session, &owner.key, &attempt.call_id)
                    })
            }
        } else {
            None
        };
        if result.name == LIST_TODOS_TOOL_NAME && !is_user_visible_list_query(results, index) {
            continue;
        }
        if let Some(outcome) = super::tool_outcome_from_todo_result(
            todo_store,
            session,
            owner,
            result,
            pending_query.as_ref(),
        )? {
            outcomes.push((index, outcome));
        }
    }
    super::refresh_todo_snapshot_for_turn(todo_store, session, owner, &outcomes)?;
    Ok(TodoTurnAggregation {
        consumed_result_indexes,
        outcomes,
    })
}

fn todo_validation_failure_was_corrected(
    result_index: usize,
    results: &[ToolExecutionResult],
    attempts: &[ToolExecutionAttempt],
) -> bool {
    let result = &results[result_index];
    if result.succeeded || !is_tool_argument_failure(&result.output) {
        return false;
    }
    if is_retry_superseded_result(result_index, attempts) {
        return true;
    }
    let Some(failed_round) = tool_result_round(result_index, attempts) else {
        return false;
    };
    results
        .iter()
        .enumerate()
        .skip(result_index + 1)
        .any(|(later_index, later)| {
            later.succeeded
                && later.name == result.name
                && tool_result_round(later_index, attempts)
                    .is_some_and(|later_round| later_round > failed_round)
        })
}

fn tool_result_round(result_index: usize, attempts: &[ToolExecutionAttempt]) -> Option<usize> {
    attempts
        .iter()
        .find(|attempt| attempt.result_index == result_index)
        .map(|attempt| attempt.round)
}

fn is_tool_argument_failure(output: &Value) -> bool {
    let code = output
        .get("error_code")
        .and_then(Value::as_str)
        .or_else(|| output.pointer("/error/code").and_then(Value::as_str));
    let kind = output.pointer("/error/kind").and_then(Value::as_str);
    matches!(code, Some("bad_tool_arguments" | "invalid_arguments"))
        || kind == Some("invalid_arguments")
}

fn is_todo_tool_result(result: &ToolExecutionResult) -> bool {
    result.name == LIST_TODOS_TOOL_NAME
        || result.name == super::GET_TODO_TOOL_NAME
        || super::todo_write_operation(&result.name).is_some()
}

fn is_user_visible_list_query(results: &[ToolExecutionResult], index: usize) -> bool {
    // 失败的列表调用本身就是用户可见的真实失败；后续独立写操作不能把它当作
    // 内部查询吞掉。成功列表仍可作为写操作前的内部定位查询而不单独展示。
    if !results[index].succeeded {
        return true;
    }
    !results.iter().skip(index + 1).any(|result| {
        result.name == super::GET_TODO_TOOL_NAME
            || super::todo_write_operation(&result.name).is_some()
    })
}

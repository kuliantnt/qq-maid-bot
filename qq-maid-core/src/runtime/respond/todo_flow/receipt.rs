//! Todo Tool 整轮聚合与确定性回执。
//!
//! Tool Loop 只负责理解意图并执行白名单 Tool；Todo Tool 的整轮结果在这里统一判断
//! 用户可见输出、内部辅助查询、相关列表刷新和 `last_todo_query` 快照，避免通用
//! chat_flow 或 AgentTurnOutcome 理解 Todo 的具体工具语义。

use std::collections::HashSet;

use chrono::NaiveDate;
use serde_json::Value;

use crate::{
    error::LlmError,
    provider::ToolExecutionResult,
    runtime::{
        respond::{
            agent_outcome::{
                OutcomePresentation, ResponseBlock, ToolEffect, ToolExecutionOutcome,
                ToolOutcomeStatus,
            },
            common::{CommandBody, todo_error, truncate_chars},
        },
        session::SessionRecord,
        todo::{
            ReminderFieldMode, TodoCardOptions, TodoItem, TodoListDateField, TodoListDateFilter,
            TodoOwner, TodoRecurrenceKind, TodoRecurrenceUnit, TodoRenderItem, TodoStatus,
            TodoStore, format_todo_cards, preview_next_reminder_at,
        },
    },
};

use super::format::{
    append_todo_collapse_hint, format_todo_natural_list_item, todo_due_chip, todo_timestamp_chip,
    visible_todo_all_board_items, visible_todo_items,
};

const LIST_TODOS_TOOL_NAME: &str = "list_todos";
const GET_TODO_TOOL_NAME: &str = "get_todo";

#[derive(Debug, Clone)]
pub(in crate::runtime::respond) struct TodoWriteReceipt {
    pub body: CommandBody,
    pub command: &'static str,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TodoWriteOperation {
    Create,
    Edit,
    Complete,
    Cancel,
    Restore,
    Merge,
    DeletePending,
}

#[derive(Debug, Clone)]
struct RelatedListSpec {
    status: TodoStatus,
    query_type: &'static str,
    condition: String,
    due_date: Option<NaiveDate>,
    due_range: Option<(NaiveDate, NaiveDate)>,
    date_field: TodoListDateField,
    title: &'static str,
    empty_text: &'static str,
    time_value: fn(&TodoItem) -> Option<String>,
}

struct RelatedReceiptDraft {
    lines: Vec<String>,
    markdown_lines: Vec<String>,
    spec: RelatedListSpec,
    command: &'static str,
    trailing_hint: Option<&'static str>,
}

pub(in crate::runtime::respond) struct TodoTurnAggregation {
    pub consumed_result_indexes: HashSet<usize>,
    pub outcomes: Vec<(usize, ToolExecutionOutcome)>,
}

pub(in crate::runtime::respond) fn aggregate_todo_tool_results(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    results: &[ToolExecutionResult],
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
        if result.name == LIST_TODOS_TOOL_NAME && !is_user_visible_list_query(results, index) {
            continue;
        }
        if let Some(outcome) = tool_outcome_from_todo_result(todo_store, session, owner, result)? {
            outcomes.push((index, outcome));
        }
    }
    append_todo_related_list_for_turn(
        todo_store,
        session,
        owner,
        todo_indexes.last().copied(),
        &mut outcomes,
    )?;
    Ok(TodoTurnAggregation {
        consumed_result_indexes,
        outcomes,
    })
}

fn is_todo_tool_result(result: &ToolExecutionResult) -> bool {
    result.name == LIST_TODOS_TOOL_NAME
        || result.name == GET_TODO_TOOL_NAME
        || todo_write_operation(&result.name).is_some()
}

fn is_user_visible_list_query(results: &[ToolExecutionResult], index: usize) -> bool {
    !results.iter().skip(index + 1).any(|result| {
        result.name == GET_TODO_TOOL_NAME || todo_write_operation(&result.name).is_some()
    })
}

fn tool_outcome_from_todo_result(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    result: &ToolExecutionResult,
) -> Result<Option<ToolExecutionOutcome>, LlmError> {
    if result.name == LIST_TODOS_TOOL_NAME {
        return list_todos_outcome(todo_store, session, owner, result).map(Some);
    }
    if result.name == GET_TODO_TOOL_NAME {
        return Ok(Some(get_todo_outcome(result)));
    }
    let Some(operation) = todo_write_operation(&result.name) else {
        return Ok(None);
    };
    let status = ToolOutcomeStatus::from_tool_result(result);
    let receipt = receipt_from_tool_result_with_status(todo_store, session, owner, result, status)?;
    Ok(Some(ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "todo".to_owned(),
        status,
        effect: tool_effect_for_operation(operation),
        presentation: OutcomePresentation::Trusted,
        blocks: vec![response_block_for_receipt(status, receipt.body.clone())],
        error_code: receipt.error_code.clone(),
        command: Some(receipt.command.to_owned()),
    }))
}

fn append_todo_related_list_for_turn(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    anchor_index: Option<usize>,
    outcomes: &mut Vec<(usize, ToolExecutionOutcome)>,
) -> Result<(), LlmError> {
    if outcomes.iter().any(|(_, outcome)| {
        outcome.domain == "todo"
            && outcome.tool_name == LIST_TODOS_TOOL_NAME
            && outcome.status == ToolOutcomeStatus::Succeeded
    }) {
        return Ok(());
    }

    let mut specs = Vec::new();
    for (_, outcome) in outcomes.iter() {
        if outcome.domain != "todo" || outcome.status != ToolOutcomeStatus::Succeeded {
            continue;
        }
        match outcome.effect {
            ToolEffect::Created | ToolEffect::Updated | ToolEffect::Completed => {
                specs.push(pending_list_spec())
            }
            ToolEffect::Cancelled => specs.push(cancelled_list_spec()),
            ToolEffect::Deleted => specs.push(completed_list_spec()),
            ToolEffect::ReadOnly | ToolEffect::ExternalSideEffect => {}
        }
    }
    if specs.is_empty() {
        return Ok(());
    }
    let Some(anchor_index) = anchor_index else {
        return Ok(());
    };
    let spec = merge_related_list_specs(&specs);
    let body = related_list_body(todo_store, session, owner, &spec)?;
    outcomes.push((
        anchor_index,
        ToolExecutionOutcome {
            tool_name: "todo_related_list".to_owned(),
            domain: "todo".to_owned(),
            status: ToolOutcomeStatus::Succeeded,
            effect: ToolEffect::ReadOnly,
            presentation: OutcomePresentation::Trusted,
            blocks: vec![ResponseBlock::RelatedList(body)],
            error_code: None,
            command: None,
        },
    ));
    Ok(())
}

fn get_todo_outcome(result: &ToolExecutionResult) -> ToolExecutionOutcome {
    let status = ToolOutcomeStatus::from_tool_result(result);
    let error_code = structured_error_code(&result.output);
    let block = match status {
        ToolOutcomeStatus::Succeeded => ResponseBlock::FactCard(todo_detail_body(&result.output)),
        ToolOutcomeStatus::Skipped => ResponseBlock::Warning(CommandBody::plain(
            skip_reply_for_tool_result(&result.output),
        )),
        ToolOutcomeStatus::RequiresClarification => ResponseBlock::Clarification(
            CommandBody::plain(error_reply_for_tool_result(&result.output)),
        ),
        ToolOutcomeStatus::PendingConfirmation | ToolOutcomeStatus::Failed => ResponseBlock::Error(
            CommandBody::plain(error_reply_for_tool_result(&result.output)),
        ),
    };

    ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "todo".to_owned(),
        status,
        effect: ToolEffect::ReadOnly,
        presentation: OutcomePresentation::Trusted,
        blocks: vec![block],
        error_code,
        command: Some("todo_detail".to_owned()),
    }
}

fn list_todos_outcome(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    result: &ToolExecutionResult,
) -> Result<ToolExecutionOutcome, LlmError> {
    let status = ToolOutcomeStatus::from_tool_result(result);
    let error_code = structured_error_code(&result.output);
    if status == ToolOutcomeStatus::Failed {
        return Ok(ToolExecutionOutcome {
            tool_name: result.name.clone(),
            domain: "todo".to_owned(),
            status,
            effect: ToolEffect::ReadOnly,
            presentation: OutcomePresentation::Trusted,
            blocks: vec![ResponseBlock::Error(CommandBody::plain(
                error_reply_for_tool_result(&result.output),
            ))],
            error_code,
            command: Some("todo_tool_error".to_owned()),
        });
    }
    if status == ToolOutcomeStatus::Skipped {
        return Ok(ToolExecutionOutcome {
            tool_name: result.name.clone(),
            domain: "todo".to_owned(),
            status,
            effect: ToolEffect::ReadOnly,
            presentation: OutcomePresentation::Trusted,
            blocks: vec![ResponseBlock::Warning(CommandBody::plain(
                skip_reply_for_tool_result(&result.output),
            ))],
            error_code,
            command: Some("todo_tool_skipped".to_owned()),
        });
    }

    let spec = list_spec_from_output(&result.output);
    let items = list_for_related_spec(todo_store, owner, &spec).map_err(todo_error)?;
    let total_count = items.len();
    let shown = visible_related_items(&items, &spec).to_vec();
    let truncated = total_count > shown.len();
    // `list_todos` 若成为最终用户可见结果，必须同步写入真实可见快照；
    // 仅在 Tool 内部执行但未展示时，原有内部查询上下文仍由 TodoToolScope 保持。
    session.remember_last_todo_query(
        &owner.key,
        spec.query_type,
        spec.condition.clone(),
        shown.iter().map(|item| item.id.clone()).collect(),
    );

    let mut lines = Vec::new();
    let mut markdown_lines = Vec::new();
    append_related_list(&mut lines, &shown, total_count, truncated, &spec, false);
    append_related_list(
        &mut markdown_lines,
        &shown,
        total_count,
        truncated,
        &spec,
        true,
    );
    Ok(ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "todo".to_owned(),
        status,
        effect: ToolEffect::ReadOnly,
        presentation: OutcomePresentation::Trusted,
        blocks: vec![ResponseBlock::RelatedList(CommandBody::dual(
            lines.join("\n"),
            markdown_lines.join("\n"),
        ))],
        error_code,
        command: Some("todo_list".to_owned()),
    })
}

fn todo_detail_body(output: &Value) -> CommandBody {
    todo_detail_card_item_from_value(output.get("item"))
        .map(|item| todo_detail_card_body("待办详情", &item))
        .unwrap_or_else(|| CommandBody::plain("没有找到待办详情。"))
}

pub(in crate::runtime::respond) fn receipt_after_created(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    item: &TodoItem,
) -> Result<TodoWriteReceipt, LlmError> {
    receipt_with_related_list_body(
        todo_store,
        session,
        owner,
        todo_detail_card_body("✅ 已新增待办", &todo_detail_card_item_from_todo(item)),
        pending_list_spec(),
        "todo_confirm",
        None,
    )
}

pub(in crate::runtime::respond) fn receipt_after_deleted(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    status: TodoStatus,
    deleted_count: usize,
    skipped_count: usize,
) -> Result<TodoWriteReceipt, LlmError> {
    let status_text = status_label(&status);
    let mut lines = vec![format!("🗑️ 已永久删除 {deleted_count} 条{status_text}")];
    let mut markdown_lines = vec![format!("# 🗑️ 已永久删除 {deleted_count} 条{status_text}")];
    if skipped_count > 0 {
        let line = format!("跳过 {skipped_count} 条已不存在或状态已变化的待办。");
        lines.push(line.clone());
        markdown_lines.push(line);
    }
    let spec = match status {
        TodoStatus::Completed => completed_list_spec(),
        TodoStatus::Cancelled => cancelled_list_spec(),
        TodoStatus::Pending => pending_list_spec(),
    };
    receipt_with_related_list(
        todo_store,
        session,
        owner,
        RelatedReceiptDraft {
            lines,
            markdown_lines,
            spec,
            command: "todo_confirm",
            trailing_hint: None,
        },
    )
}

fn receipt_from_tool_result_with_status(
    _todo_store: &TodoStore,
    _session: &mut SessionRecord,
    _owner: &TodoOwner,
    result: &ToolExecutionResult,
    status: ToolOutcomeStatus,
) -> Result<TodoWriteReceipt, LlmError> {
    let Some(operation) = todo_write_operation(&result.name) else {
        return Err(LlmError::new(
            "bad_tool_result",
            format!("tool `{}` is not a Todo write tool", result.name),
            "todo_receipt",
        ));
    };
    if status == ToolOutcomeStatus::Skipped {
        return Ok(simple_receipt(
            CommandBody::plain(skip_reply_for_tool_result(&result.output)),
            "todo_tool_skipped",
            structured_error_code(&result.output),
        ));
    }
    if status == ToolOutcomeStatus::RequiresClarification {
        let question = string_field(&result.output, "question")
            .or_else(|| string_field(&result.output, "message"))
            .unwrap_or_else(|| "请再具体说明要操作哪条待办。".to_owned());
        return Ok(simple_receipt(
            CommandBody::plain(question),
            "todo_clarify_wait",
            structured_error_code(&result.output),
        ));
    }
    if status == ToolOutcomeStatus::Failed {
        return Ok(simple_receipt(
            CommandBody::plain(error_reply_for_tool_result(&result.output)),
            "todo_tool_error",
            structured_error_code(&result.output),
        ));
    }
    if status == ToolOutcomeStatus::PendingConfirmation {
        return Ok(pending_confirmation_receipt(&result.output));
    }

    let receipt = match operation {
        TodoWriteOperation::Create => {
            let items = receipt_items_from_array(&result.output, "created_items")
                .or_else(|| item_from_value(result.output.get("created")).map(|item| vec![item]))
                .unwrap_or_default();
            if let Some(item) = todo_detail_card_item_from_value(result.output.get("created"))
                && items.len() <= 1
            {
                let body = todo_detail_card_body("✅ 已新增待办", &item);
                return mutation_receipt(body, "todo_create");
            }
            mutation_receipt(
                CommandBody::dual(
                    success_items_lines("✅ 已新增待办", &items).join("\n"),
                    success_items_markdown_lines("✅ 已新增待办", &items).join("\n"),
                ),
                "todo_create",
            )?
        }
        TodoWriteOperation::Edit => {
            let item = item_from_value(result.output.get("updated"));
            if let Some(item) = todo_detail_card_item_from_value(result.output.get("updated")) {
                let body = todo_detail_card_body("✏️ 已修改待办", &item);
                return mutation_receipt(body, "todo_edit");
            }
            let lines = success_lines("✏️ 已修改待办", item.as_ref());
            let markdown_lines = success_markdown_lines("✏️ 已修改待办", item.as_ref());
            mutation_receipt(
                CommandBody::dual(lines.join("\n"), markdown_lines.join("\n")),
                "todo_edit",
            )?
        }
        TodoWriteOperation::Complete => {
            let count = result
                .output
                .get("completed")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
                + result
                    .output
                    .get("advanced")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
            let mut items =
                todo_detail_card_items_from_array(&result.output, "completed").unwrap_or_default();
            items.extend(
                todo_detail_card_items_from_array(&result.output, "advanced").unwrap_or_default(),
            );
            if !items.is_empty() {
                let has_advanced = result
                    .output
                    .get("advanced")
                    .and_then(Value::as_array)
                    .is_some_and(|values| !values.is_empty());
                let title = if has_advanced {
                    format!("✅ 已完成本次待办 · {count}条")
                } else {
                    format!("✅ 已完成待办 · {count}条")
                };
                let body = todo_detail_cards_body(&title, &items);
                return mutation_receipt(body, "todo_complete");
            }
            let lines = success_count_lines(
                "✅ 已完成本次待办",
                count,
                "条",
                "completed",
                &result.output,
            );
            let markdown_lines = success_count_markdown_lines(
                "✅ 已完成本次待办",
                count,
                "条",
                "completed",
                &result.output,
            );
            mutation_receipt(
                CommandBody::dual(lines.join("\n"), markdown_lines.join("\n")),
                "todo_complete",
            )?
        }
        TodoWriteOperation::Cancel => {
            let count = result
                .output
                .get("cancelled")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if let Some(items) = todo_detail_card_items_from_array(&result.output, "cancelled") {
                let body = todo_detail_cards_body(&format!("⛔ 已取消待办 · {count}条"), &items);
                return mutation_receipt(body, "todo_cancel");
            }
            let lines =
                success_count_lines("⛔ 已取消待办", count, "条", "cancelled", &result.output);
            let markdown_lines = success_count_markdown_lines(
                "⛔ 已取消待办",
                count,
                "条",
                "cancelled",
                &result.output,
            );
            mutation_receipt(
                CommandBody::dual(lines.join("\n"), markdown_lines.join("\n")),
                "todo_cancel",
            )?
        }
        TodoWriteOperation::Restore => {
            let count = result
                .output
                .get("restored")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            if let Some(items) = todo_detail_card_items_from_array(&result.output, "restored") {
                let body = todo_detail_cards_body(&format!("↩️ 已恢复待办 · {count}条"), &items);
                return mutation_receipt(body, "todo_restore");
            }
            let lines =
                success_count_lines("↩️ 已恢复待办", count, "条", "restored", &result.output);
            let markdown_lines = success_count_markdown_lines(
                "↩️ 已恢复待办",
                count,
                "条",
                "restored",
                &result.output,
            );
            mutation_receipt(
                CommandBody::dual(lines.join("\n"), markdown_lines.join("\n")),
                "todo_restore",
            )?
        }
        TodoWriteOperation::Merge => {
            let target = result
                .output
                .get("merged")
                .and_then(|value| value.get("target"))
                .and_then(|value| item_from_value(Some(value)));
            let lines = success_lines("🔀 已合并待办", target.as_ref());
            let markdown_lines = success_markdown_lines("🔀 已合并待办", target.as_ref());
            mutation_receipt(
                CommandBody::dual(lines.join("\n"), markdown_lines.join("\n")),
                "todo_merge",
            )?
        }
        TodoWriteOperation::DeletePending => pending_confirmation_receipt(&result.output),
    };
    Ok(receipt)
}

fn response_block_for_receipt(status: ToolOutcomeStatus, body: CommandBody) -> ResponseBlock {
    match status {
        ToolOutcomeStatus::Succeeded => ResponseBlock::MutationReceipt(body),
        ToolOutcomeStatus::PendingConfirmation => ResponseBlock::Confirmation(body),
        ToolOutcomeStatus::RequiresClarification => ResponseBlock::Clarification(body),
        ToolOutcomeStatus::Failed => ResponseBlock::Error(body),
        ToolOutcomeStatus::Skipped => ResponseBlock::Warning(body),
    }
}

fn tool_effect_for_operation(operation: TodoWriteOperation) -> ToolEffect {
    match operation {
        TodoWriteOperation::Create => ToolEffect::Created,
        TodoWriteOperation::Edit => ToolEffect::Updated,
        TodoWriteOperation::Complete => ToolEffect::Completed,
        TodoWriteOperation::Cancel => ToolEffect::Cancelled,
        TodoWriteOperation::Restore => ToolEffect::Updated,
        TodoWriteOperation::Merge => ToolEffect::Updated,
        TodoWriteOperation::DeletePending => ToolEffect::Deleted,
    }
}

fn receipt_with_related_list(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    draft: RelatedReceiptDraft,
) -> Result<TodoWriteReceipt, LlmError> {
    let RelatedReceiptDraft {
        mut lines,
        mut markdown_lines,
        spec,
        command,
        trailing_hint,
    } = draft;
    lines.push(String::new());
    markdown_lines.push(String::new());
    let related = related_list_body(todo_store, session, owner, &spec)?;
    lines.push(related.text);
    markdown_lines.push(
        related
            .markdown
            .unwrap_or_else(|| lines.last().cloned().unwrap_or_default()),
    );
    if let Some(hint) = trailing_hint {
        lines.push(String::new());
        lines.push(hint.to_owned());
        markdown_lines.push(String::new());
        markdown_lines.push(hint.to_owned());
    }

    Ok(TodoWriteReceipt {
        body: CommandBody::dual(lines.join("\n"), markdown_lines.join("\n")),
        command,
        error_code: None,
    })
}

fn receipt_with_related_list_body(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    body: CommandBody,
    spec: RelatedListSpec,
    command: &'static str,
    trailing_hint: Option<&'static str>,
) -> Result<TodoWriteReceipt, LlmError> {
    let text = body.text;
    let markdown = body.markdown.unwrap_or_else(|| text.clone());
    receipt_with_related_list(
        todo_store,
        session,
        owner,
        RelatedReceiptDraft {
            lines: vec![text],
            markdown_lines: vec![markdown],
            spec,
            command,
            trailing_hint,
        },
    )
}

fn related_list_body(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    spec: &RelatedListSpec,
) -> Result<CommandBody, LlmError> {
    let items = list_for_related_spec(todo_store, owner, spec).map_err(todo_error)?;
    let total_count = items.len();
    let shown = visible_related_items(&items, spec).to_vec();
    let truncated = total_count > shown.len();
    // 快照只保存本次真正展示的可编号条目；隐藏项不能拥有用户没看到的编号。
    session.remember_last_todo_query(
        &owner.key,
        spec.query_type,
        spec.condition.clone(),
        shown.iter().map(|item| item.id.clone()).collect(),
    );
    let mut lines = Vec::new();
    let mut markdown_lines = Vec::new();
    append_related_list(&mut lines, &shown, total_count, truncated, spec, false);
    append_related_list(
        &mut markdown_lines,
        &shown,
        total_count,
        truncated,
        spec,
        true,
    );
    Ok(CommandBody::dual(
        lines.join("\n"),
        markdown_lines.join("\n"),
    ))
}

fn visible_related_items<'a>(items: &'a [TodoItem], spec: &RelatedListSpec) -> &'a [TodoItem] {
    if spec.query_type == "all" {
        visible_todo_all_board_items(items, false)
    } else {
        visible_todo_items(items, false)
    }
}

fn merge_related_list_specs(specs: &[RelatedListSpec]) -> RelatedListSpec {
    let Some(first) = specs.first() else {
        return pending_list_spec();
    };
    if specs
        .iter()
        .all(|spec| spec.query_type == first.query_type && spec.condition == first.condition)
    {
        return first.clone();
    }
    all_list_spec()
}

fn pending_confirmation_receipt(output: &Value) -> TodoWriteReceipt {
    let pending_action = output
        .get("pending_action")
        .and_then(Value::as_str)
        .unwrap_or("");
    let body = match pending_action {
        "delete" => {
            let items = receipt_items_from_array(output, "items")
                .or_else(|| item_from_value(output.get("item")).map(|item| vec![item]))
                .unwrap_or_default();
            let count = items.len();
            let source = string_field(output, "selection_source");
            let mut lines = vec![format!("⚠️ 确认删除以下 {count} 项待办吗？")];
            let mut markdown_lines = vec![format!("# ⚠️ 确认删除以下 {count} 项待办吗？")];
            if let Some(source) = source {
                lines.push(format!("范围：{source}"));
                markdown_lines.push(format!("范围：{source}"));
            }
            if let Some(detail_items) =
                todo_detail_card_items_from_array(output, "items").or_else(|| {
                    todo_detail_card_item_from_value(output.get("item")).map(|item| vec![item])
                })
            {
                for (index, item) in detail_items.iter().enumerate() {
                    lines.push(String::new());
                    markdown_lines.push(String::new());
                    append_todo_detail_card_lines(&mut lines, item, false, index, true);
                    append_todo_detail_card_lines(&mut markdown_lines, item, true, index, true);
                }
            } else {
                for (index, item) in items.iter().enumerate() {
                    lines.push(format!("{}. {}", index + 1, item.title));
                    markdown_lines.push(format!("{}. {}", index + 1, item.title));
                }
            }
            lines.push(String::new());
            lines.push("删除后不可恢复。".to_owned());
            lines.push("回复“确认”继续，回复“取消”放弃。".to_owned());
            markdown_lines.push(String::new());
            markdown_lines.push("删除后不可恢复。".to_owned());
            markdown_lines.push("回复“确认”继续，回复“取消”放弃。".to_owned());
            CommandBody::dual(lines.join("\n"), markdown_lines.join("\n"))
        }
        _ => CommandBody::plain(
            string_field(output, "message").unwrap_or_else(|| "这次待办操作需要确认。".to_owned()),
        ),
    };
    simple_receipt(body, "todo_pending", None)
}

fn simple_receipt(
    body: CommandBody,
    command: &'static str,
    error_code: Option<String>,
) -> TodoWriteReceipt {
    TodoWriteReceipt {
        body,
        command,
        error_code,
    }
}

fn mutation_receipt(
    body: CommandBody,
    command: &'static str,
) -> Result<TodoWriteReceipt, LlmError> {
    Ok(TodoWriteReceipt {
        body,
        command,
        error_code: None,
    })
}

fn append_related_list(
    rows: &mut Vec<String>,
    items: &[TodoItem],
    total_count: usize,
    truncated: bool,
    spec: &RelatedListSpec,
    markdown: bool,
) {
    if total_count == 0 {
        rows.push(spec.empty_text.to_owned());
        return;
    }
    rows.push(if markdown {
        format!("## {} · 共 {} 项", spec.title, total_count)
    } else {
        format!("{} · 共 {} 项", spec.title, total_count)
    });
    for (index, item) in items.iter().enumerate() {
        rows.push(format_todo_natural_list_item(
            index,
            item,
            (spec.time_value)(item),
            markdown,
            None,
        ));
    }
    if truncated {
        let (range_label, command) = collapse_prompt_for_related_spec(spec);
        append_todo_collapse_hint(
            rows,
            total_count.saturating_sub(items.len()),
            range_label,
            command,
        );
    }
}

fn collapse_prompt_for_related_spec(
    spec: &RelatedListSpec,
) -> (Option<&'static str>, &'static str) {
    match spec.query_type {
        "list" => (Some("进行中待办"), "查看全部进行中待办"),
        "completed-list" => (Some("已完成待办"), "查看全部已完成待办"),
        "cancelled-list" => (Some("已取消待办"), "查看全部已取消待办"),
        "all" => (Some("待办"), "查看完整结果"),
        _ => (None, "查看完整结果"),
    }
}

fn list_for_spec(
    todo_store: &TodoStore,
    owner: &TodoOwner,
    spec: &RelatedListSpec,
) -> Result<Vec<TodoItem>, crate::runtime::todo::TodoError> {
    match &spec.status {
        TodoStatus::Pending => todo_store.list_pending(owner),
        TodoStatus::Completed => todo_store.list_completed(owner),
        TodoStatus::Cancelled => todo_store.list_cancelled(owner),
    }
}

fn list_for_related_spec(
    todo_store: &TodoStore,
    owner: &TodoOwner,
    spec: &RelatedListSpec,
) -> Result<Vec<TodoItem>, crate::runtime::todo::TodoError> {
    if spec.query_type == "all" {
        if let Some((start, end)) = spec.due_range {
            todo_store.list_all_by_date_filter_for_board(
                owner,
                TodoListDateFilter {
                    start,
                    end,
                    field: spec.date_field,
                },
            )
        } else if let Some(due_date) = spec.due_date {
            todo_store.list_all_by_due_date_for_board(owner, due_date)
        } else {
            todo_store.list_all_for_board(owner)
        }
    } else if let Some((start, end)) = spec.due_range {
        todo_store.list_by_date_filter(
            owner,
            spec.status.clone(),
            TodoListDateFilter {
                start,
                end,
                field: spec.date_field,
            },
        )
    } else if let Some(due_date) = spec.due_date {
        todo_store.list_by_due_date(owner, spec.status.clone(), due_date)
    } else {
        list_for_spec(todo_store, owner, spec)
    }
}

fn todo_write_operation(name: &str) -> Option<TodoWriteOperation> {
    match name {
        "create_todo" => Some(TodoWriteOperation::Create),
        "edit_todo" => Some(TodoWriteOperation::Edit),
        "complete_todos" => Some(TodoWriteOperation::Complete),
        "cancel_todo" => Some(TodoWriteOperation::Cancel),
        "restore_todos" => Some(TodoWriteOperation::Restore),
        "merge_todos" => Some(TodoWriteOperation::Merge),
        "delete_todos" => Some(TodoWriteOperation::DeletePending),
        _ => None,
    }
}

fn pending_list_spec() -> RelatedListSpec {
    RelatedListSpec {
        status: TodoStatus::Pending,
        query_type: "list",
        condition: String::new(),
        due_date: None,
        due_range: None,
        date_field: TodoListDateField::Planned,
        title: "🚧 当前进行中",
        empty_text: "当前没有进行中的待办。",
        time_value: todo_due_chip,
    }
}

fn completed_list_spec() -> RelatedListSpec {
    RelatedListSpec {
        status: TodoStatus::Completed,
        query_type: "completed-list",
        condition: "已完成列表".to_owned(),
        due_date: None,
        due_range: None,
        date_field: TodoListDateField::Planned,
        title: "✅ 当前已完成",
        empty_text: "当前没有已完成待办。",
        time_value: display_todo_completed_at,
    }
}

fn cancelled_list_spec() -> RelatedListSpec {
    RelatedListSpec {
        status: TodoStatus::Cancelled,
        query_type: "cancelled-list",
        condition: "已取消列表".to_owned(),
        due_date: None,
        due_range: None,
        date_field: TodoListDateField::Planned,
        title: "⛔ 当前已取消",
        empty_text: "当前没有已取消待办。",
        time_value: display_todo_cancelled_at,
    }
}

fn list_spec_from_output(output: &Value) -> RelatedListSpec {
    let status = string_field(output, "status");
    let mut spec = match status.as_deref() {
        Some("completed") => completed_list_spec(),
        Some("cancelled") => cancelled_list_spec(),
        Some("all") => RelatedListSpec { ..all_list_spec() },
        _ => pending_list_spec(),
    };
    if let Some(due_date) = string_field(output, "due_date")
        .and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok())
    {
        spec.condition = due_date.format("%Y-%m-%d").to_string();
        spec.due_date = Some(due_date);
        if matches!(status.as_deref(), None | Some("pending")) {
            spec.query_type = "due-date";
        }
    } else if let (Some(start), Some(end)) = (
        string_field(output, "due_start")
            .and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok()),
        string_field(output, "due_end")
            .and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok()),
    ) {
        spec.condition = string_field(output, "date_range_text").unwrap_or_else(|| {
            format!("{} 至 {}", start.format("%Y-%m-%d"), end.format("%Y-%m-%d"))
        });
        spec.due_range = Some((start, end));
        spec.date_field = date_field_from_output(output, status.as_deref());
        if matches!(status.as_deref(), None | Some("pending")) {
            spec.query_type = "due-date";
        }
    }
    spec
}

fn all_list_spec() -> RelatedListSpec {
    RelatedListSpec {
        status: TodoStatus::Pending,
        query_type: "all",
        condition: "全部待办".to_owned(),
        due_date: None,
        due_range: None,
        date_field: TodoListDateField::Planned,
        title: "📋 全部待办",
        empty_text: "当前没有待办。",
        time_value: todo_due_chip,
    }
}

fn date_field_from_output(output: &Value, status: Option<&str>) -> TodoListDateField {
    match string_field(output, "date_range_field").as_deref() {
        Some("completed_at") => TodoListDateField::CompletedAt,
        Some("cancelled_at") => TodoListDateField::CancelledAt,
        Some("planned") => TodoListDateField::Planned,
        _ => match status {
            Some("completed") => TodoListDateField::CompletedAt,
            Some("cancelled") => TodoListDateField::CancelledAt,
            _ => TodoListDateField::Planned,
        },
    }
}

fn display_todo_completed_at(item: &TodoItem) -> Option<String> {
    item.completed_at.as_deref().and_then(todo_timestamp_chip)
}

fn display_todo_cancelled_at(item: &TodoItem) -> Option<String> {
    item.cancelled_at.as_deref().and_then(todo_timestamp_chip)
}

fn success_lines(title: &str, item: Option<&ReceiptItem>) -> Vec<String> {
    let mut lines = vec![title.to_owned()];
    if let Some(item) = item {
        lines.push(String::new());
        lines.push(format!("- {}", item.title));
        if let Some(time) = item
            .display_time
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            lines.push(format!("  时间：{time}"));
        }
    }
    lines
}

fn success_markdown_lines(title: &str, item: Option<&ReceiptItem>) -> Vec<String> {
    success_lines(&format!("# {title}"), item)
}

fn success_items_lines(title: &str, items: &[ReceiptItem]) -> Vec<String> {
    let mut lines = vec![if items.len() > 1 {
        format!("{title} · {} 条", items.len())
    } else {
        title.to_owned()
    }];
    for item in items {
        lines.push(format!("- {}", item.title));
        if let Some(time) = item
            .display_time
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            lines.push(format!("  时间：{time}"));
        }
    }
    lines
}

fn success_items_markdown_lines(title: &str, items: &[ReceiptItem]) -> Vec<String> {
    let mut lines = success_items_lines(title, items);
    if let Some(first) = lines.first_mut() {
        *first = format!("# {first}");
    }
    lines
}

fn success_count_lines(
    title: &str,
    count: usize,
    unit: &str,
    field: &str,
    output: &Value,
) -> Vec<String> {
    let mut lines = vec![format!("{title} · {count}{unit}")];
    if let Some(items) = output.get(field).and_then(Value::as_array) {
        for item in items
            .iter()
            .filter_map(|value| item_from_value(Some(value)))
        {
            lines.push(format!("- {}", item.title));
        }
    }
    lines
}

fn success_count_markdown_lines(
    title: &str,
    count: usize,
    unit: &str,
    field: &str,
    output: &Value,
) -> Vec<String> {
    let mut lines = success_count_lines(title, count, unit, field, output);
    if let Some(first) = lines.first_mut() {
        *first = format!("# {first}");
    }
    lines
}

#[derive(Debug, Clone)]
struct ReceiptItem {
    title: String,
    display_time: Option<String>,
}

#[derive(Debug, Clone)]
struct TodoDetailCardItem {
    title: String,
    detail: Option<String>,
    due_date: Option<String>,
    due_at: Option<String>,
    reminder_at: Option<String>,
    recurrence_kind: TodoRecurrenceKind,
    recurrence_interval_days: u32,
    recurrence_interval: u32,
    recurrence_unit: TodoRecurrenceUnit,
    status: Option<String>,
    next_reminder_at: Option<String>,
    completed_at: Option<String>,
    cancelled_at: Option<String>,
}

fn item_from_value(value: Option<&Value>) -> Option<ReceiptItem> {
    let value = value?;
    let title = string_field(value, "title")?;
    Some(ReceiptItem {
        title: truncate_chars(&title, 80),
        display_time: string_field(value, "display_time"),
    })
}

fn receipt_items_from_array(output: &Value, key: &str) -> Option<Vec<ReceiptItem>> {
    let items = output
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(|value| item_from_value(Some(value)))
        .collect::<Vec<_>>();
    (!items.is_empty()).then_some(items)
}

fn todo_detail_card_item_from_value(value: Option<&Value>) -> Option<TodoDetailCardItem> {
    let value = value?;
    let title = string_field(value, "title")?;
    Some(TodoDetailCardItem {
        title: truncate_chars(&title, 120),
        detail: string_field(value, "detail").map(|value| truncate_chars(&value, 300)),
        due_date: string_field(value, "due_date"),
        due_at: string_field(value, "due_at"),
        reminder_at: string_field(value, "reminder_at"),
        recurrence_kind: recurrence_kind_field(value, "recurrence_kind")
            .unwrap_or(TodoRecurrenceKind::None),
        recurrence_interval_days: positive_u32_field(value, "recurrence_interval_days")
            .unwrap_or_default(),
        recurrence_interval: positive_u32_field(value, "recurrence_interval").unwrap_or_default(),
        recurrence_unit: recurrence_unit_field(value, "recurrence_unit")
            .unwrap_or(TodoRecurrenceUnit::Day),
        status: string_field(value, "status"),
        next_reminder_at: string_field(value, "next_reminder_at"),
        completed_at: string_field(value, "completed_at"),
        cancelled_at: string_field(value, "cancelled_at"),
    })
}

fn todo_detail_card_items_from_array(output: &Value, key: &str) -> Option<Vec<TodoDetailCardItem>> {
    let items = output
        .get(key)?
        .as_array()?
        .iter()
        .filter_map(|value| todo_detail_card_item_from_value(Some(value)))
        .collect::<Vec<_>>();
    (!items.is_empty()).then_some(items)
}

fn todo_detail_card_body(title: &str, item: &TodoDetailCardItem) -> CommandBody {
    todo_detail_cards_body(title, std::slice::from_ref(item))
}

fn todo_detail_cards_body(title: &str, items: &[TodoDetailCardItem]) -> CommandBody {
    let render_items = items
        .iter()
        .cloned()
        .map(todo_render_item_from_detail_card)
        .collect::<Vec<_>>();
    let body = format_todo_cards(
        title,
        &render_items,
        TodoCardOptions {
            reminder_mode: ReminderFieldMode::Current,
            show_next_reminder: true,
        },
    );
    CommandBody::dual(body.text, body.markdown)
}

fn append_todo_detail_card_lines(
    lines: &mut Vec<String>,
    item: &TodoDetailCardItem,
    markdown: bool,
    index: usize,
    numbered: bool,
) {
    let mut render_item = todo_render_item_from_detail_card(item.clone());
    if numbered {
        render_item.title = format!("{}. {}", index + 1, render_item.title);
    }
    let body = format_todo_cards(
        "__card__",
        &[render_item],
        TodoCardOptions {
            reminder_mode: ReminderFieldMode::Current,
            show_next_reminder: true,
        },
    );
    let rendered = if markdown { body.markdown } else { body.text };
    let mut parts = rendered.lines();
    let _ = parts.next();
    lines.extend(parts.map(str::to_owned));
}

fn recurrence_kind_field(value: &Value, key: &str) -> Option<TodoRecurrenceKind> {
    match value.get(key).and_then(Value::as_str) {
        Some("none") => Some(TodoRecurrenceKind::None),
        Some("daily") => Some(TodoRecurrenceKind::Daily),
        Some("every_n_days") => Some(TodoRecurrenceKind::EveryNDays),
        Some("weekly") => Some(TodoRecurrenceKind::Weekly),
        Some("every_n_weeks") => Some(TodoRecurrenceKind::EveryNWeeks),
        Some("monthly") => Some(TodoRecurrenceKind::Monthly),
        Some("every_n_months") => Some(TodoRecurrenceKind::EveryNMonths),
        Some("yearly") => Some(TodoRecurrenceKind::Yearly),
        Some("every_n_years") => Some(TodoRecurrenceKind::EveryNYears),
        _ => None,
    }
}

fn recurrence_unit_field(value: &Value, key: &str) -> Option<TodoRecurrenceUnit> {
    match value.get(key).and_then(Value::as_str) {
        Some("day") => Some(TodoRecurrenceUnit::Day),
        Some("week") => Some(TodoRecurrenceUnit::Week),
        Some("month") => Some(TodoRecurrenceUnit::Month),
        Some("year") => Some(TodoRecurrenceUnit::Year),
        _ => None,
    }
}

fn positive_u32_field(value: &Value, key: &str) -> Option<u32> {
    value.get(key).and_then(Value::as_u64)?.try_into().ok()
}

fn todo_render_item_from_detail_card(item: TodoDetailCardItem) -> TodoRenderItem {
    TodoRenderItem {
        title: item.title,
        detail: item.detail,
        due_date: item.due_date,
        due_at: item.due_at,
        reminder_at: item.reminder_at,
        recurrence_kind: item.recurrence_kind,
        recurrence_interval_days: item.recurrence_interval_days,
        recurrence_interval: item.recurrence_interval,
        recurrence_unit: item.recurrence_unit,
        status: item.status,
        next_reminder_at: item.next_reminder_at,
        completed_at: item.completed_at,
        cancelled_at: item.cancelled_at,
    }
}

fn todo_detail_card_item_from_todo(item: &TodoItem) -> TodoDetailCardItem {
    TodoDetailCardItem {
        title: item.title.clone(),
        detail: item.detail.clone(),
        due_date: item.due_date.clone(),
        due_at: item.due_at.clone(),
        reminder_at: item.reminder_at.clone(),
        recurrence_kind: item.recurrence_kind.clone(),
        recurrence_interval_days: item.recurrence_interval_days,
        recurrence_interval: item.recurrence_interval,
        recurrence_unit: item.recurrence_unit,
        status: Some(
            match item.status {
                TodoStatus::Pending => "pending",
                TodoStatus::Completed => "completed",
                TodoStatus::Cancelled => "cancelled",
            }
            .to_owned(),
        ),
        next_reminder_at: preview_next_reminder_at(item).ok().flatten(),
        completed_at: item.completed_at.clone(),
        cancelled_at: item.cancelled_at.clone(),
    }
}

fn error_reply_for_tool_result(output: &Value) -> String {
    let code = structured_error_code(output);
    match code.as_deref() {
        Some("todo_visible_numbers_unavailable") => {
            "没有可用的最近待办编号。请先查看对应待办列表，再按编号操作。".to_owned()
        }
        Some("todo_reference_unavailable") => {
            "找不到“刚才那条”待办。请先查看列表或明确说明要操作哪一条。".to_owned()
        }
        Some("todo_reference_invalid_state") => {
            "目标待办当前状态不允许执行这次操作。请查看最新列表后再试。".to_owned()
        }
        Some("todo_selection_not_found") => {
            "没有找到符合条件的待办，或可见编号已经失效。请查看最新列表后再操作。".to_owned()
        }
        Some("todo_delete_invalid_state") => {
            "目标待办当前无法永久删除，请查看最新列表后再试。".to_owned()
        }
        Some("todo_delete_mixed_status") => {
            "这次永久删除没有成功，请查看最新列表后再试。".to_owned()
        }
        Some("todo_pending_exists") | Some("todo_pending_conflict") => {
            "当前已有待确认的待办操作，请先回复“确认”或“取消”。".to_owned()
        }
        _ => string_field(output, "message")
            .or_else(|| {
                output
                    .get("error")
                    .and_then(|error| string_field(error, "message"))
            })
            .unwrap_or_else(|| "这次待办操作没有成功，没有修改待办。".to_owned()),
    }
}

fn skip_reply_for_tool_result(output: &Value) -> String {
    match string_field(output, "reason").as_deref() {
        Some("dependency_previous_call_failed") => {
            "前序工具没有成功，本次待办操作已跳过，数据库未因此继续修改。".to_owned()
        }
        Some(reason) => format!("本次待办操作已跳过：{reason}。"),
        None => "本次待办操作已跳过，数据库未因此继续修改。".to_owned(),
    }
}

fn structured_error_code(output: &Value) -> Option<String> {
    output
        .get("error_code")
        .and_then(Value::as_str)
        .or_else(|| {
            output
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn status_label(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "进行中待办",
        TodoStatus::Completed => "已完成待办",
        TodoStatus::Cancelled => "已取消待办",
    }
}

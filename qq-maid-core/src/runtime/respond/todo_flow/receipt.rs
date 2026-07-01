//! Todo 写操作的确定性回执。
//!
//! Tool Loop 只负责理解意图并执行白名单 Tool；一旦 Todo 写工具返回，最终用户可见
//! 文案、相关列表刷新和 `last_todo_query` 快照都在服务端生成，避免模型自由总结和
//! session 编号快照出现偏差。

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
        todo::{TodoItem, TodoOwner, TodoStatus, TodoStore, display_todo_time},
    },
    util::time_context::format_todo_time_for_display,
};

use super::format::{format_todo_inline, format_todo_inline_markdown};

const RECEIPT_LIST_LIMIT: usize = 5;
const LIST_TODOS_TOOL_NAME: &str = "list_todos";

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
    CancelPending,
    Restore,
    DeletePending,
}

#[derive(Debug, Clone)]
struct RelatedListSpec {
    status: TodoStatus,
    query_type: &'static str,
    condition: &'static str,
    title: &'static str,
    empty_text: &'static str,
    time_label: &'static str,
    time_value: fn(&TodoItem) -> String,
}

struct RelatedReceiptDraft {
    lines: Vec<String>,
    markdown_lines: Vec<String>,
    spec: RelatedListSpec,
    command: &'static str,
    trailing_hint: Option<&'static str>,
}

pub(in crate::runtime::respond) fn tool_outcome_from_todo_result(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    result: &ToolExecutionResult,
) -> Result<Option<ToolExecutionOutcome>, LlmError> {
    if result.name == LIST_TODOS_TOOL_NAME {
        return list_todos_outcome(todo_store, session, owner, result).map(Some);
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
    let shown = items
        .iter()
        .take(RECEIPT_LIST_LIMIT)
        .cloned()
        .collect::<Vec<_>>();
    let truncated = total_count > shown.len();
    // `list_todos` 若成为最终用户可见结果，必须同步写入真实可见快照；
    // 仅在 Tool 内部执行但未展示时，原有内部查询上下文仍由 TodoToolScope 保持。
    session.remember_last_todo_query(
        &owner.key,
        spec.query_type,
        spec.condition,
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

pub(in crate::runtime::respond) fn receipt_after_created(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    item: &TodoItem,
) -> Result<TodoWriteReceipt, LlmError> {
    let lines = vec![
        "✅ 已新增待办".to_owned(),
        String::new(),
        affected_item_line(item),
    ];
    let markdown_lines = vec![
        "# ✅ 已新增待办".to_owned(),
        String::new(),
        affected_item_line_markdown(item),
    ];
    receipt_with_related_list(
        todo_store,
        session,
        owner,
        RelatedReceiptDraft {
            lines,
            markdown_lines,
            spec: pending_list_spec(),
            command: "todo_confirm",
            trailing_hint: None,
        },
    )
}

pub(in crate::runtime::respond) fn receipt_after_cancelled(
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    item: &TodoItem,
) -> Result<TodoWriteReceipt, LlmError> {
    let lines = vec![
        "⛔ 已取消待办".to_owned(),
        String::new(),
        affected_item_line(item),
    ];
    let markdown_lines = vec![
        "# ⛔ 已取消待办".to_owned(),
        String::new(),
        affected_item_line_markdown(item),
    ];
    receipt_with_related_list(
        todo_store,
        session,
        owner,
        RelatedReceiptDraft {
            lines,
            markdown_lines,
            spec: cancelled_list_spec(),
            command: "todo_confirm",
            trailing_hint: Some("可说：删除全部取消的待办"),
        },
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
    todo_store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
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
            let item = item_from_value(result.output.get("created"));
            let lines = success_lines("✅ 已新增待办", item.as_ref());
            let markdown_lines = success_markdown_lines("✅ 已新增待办", item.as_ref());
            receipt_with_related_list(
                todo_store,
                session,
                owner,
                RelatedReceiptDraft {
                    lines,
                    markdown_lines,
                    spec: pending_list_spec(),
                    command: "todo_create",
                    trailing_hint: None,
                },
            )?
        }
        TodoWriteOperation::Edit => {
            let item = item_from_value(result.output.get("updated"));
            let lines = success_lines("✏️ 已修改待办", item.as_ref());
            let markdown_lines = success_markdown_lines("✏️ 已修改待办", item.as_ref());
            receipt_with_related_list(
                todo_store,
                session,
                owner,
                RelatedReceiptDraft {
                    lines,
                    markdown_lines,
                    spec: pending_list_spec(),
                    command: "todo_edit",
                    trailing_hint: None,
                },
            )?
        }
        TodoWriteOperation::Complete => {
            let count = result
                .output
                .get("completed")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let lines =
                success_count_lines("✅ 已完成待办", count, "条", "completed", &result.output);
            let markdown_lines = success_count_markdown_lines(
                "✅ 已完成待办",
                count,
                "条",
                "completed",
                &result.output,
            );
            receipt_with_related_list(
                todo_store,
                session,
                owner,
                RelatedReceiptDraft {
                    lines,
                    markdown_lines,
                    spec: pending_list_spec(),
                    command: "todo_complete",
                    trailing_hint: None,
                },
            )?
        }
        TodoWriteOperation::Restore => {
            let count = result
                .output
                .get("restored")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let lines =
                success_count_lines("↩️ 已恢复待办", count, "条", "restored", &result.output);
            let markdown_lines = success_count_markdown_lines(
                "↩️ 已恢复待办",
                count,
                "条",
                "restored",
                &result.output,
            );
            receipt_with_related_list(
                todo_store,
                session,
                owner,
                RelatedReceiptDraft {
                    lines,
                    markdown_lines,
                    spec: pending_list_spec(),
                    command: "todo_restore",
                    trailing_hint: None,
                },
            )?
        }
        TodoWriteOperation::CancelPending | TodoWriteOperation::DeletePending => {
            pending_confirmation_receipt(&result.output)
        }
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
        TodoWriteOperation::CancelPending => ToolEffect::Cancelled,
        TodoWriteOperation::Restore => ToolEffect::Updated,
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
    let items = list_for_spec(todo_store, owner, &spec).map_err(todo_error)?;
    let total_count = items.len();
    let shown = items
        .iter()
        .take(RECEIPT_LIST_LIMIT)
        .cloned()
        .collect::<Vec<_>>();
    let truncated = total_count > shown.len();
    // 快照只保存本次真正展示的可编号条目；隐藏项不能拥有用户没看到的编号。
    session.remember_last_todo_query(
        &owner.key,
        spec.query_type,
        spec.condition,
        shown.iter().map(|item| item.id.clone()).collect(),
    );

    lines.push(String::new());
    markdown_lines.push(String::new());
    append_related_list(&mut lines, &shown, total_count, truncated, &spec, false);
    append_related_list(
        &mut markdown_lines,
        &shown,
        total_count,
        truncated,
        &spec,
        true,
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

fn pending_confirmation_receipt(output: &Value) -> TodoWriteReceipt {
    let pending_action = output
        .get("pending_action")
        .and_then(Value::as_str)
        .unwrap_or("");
    let body = match pending_action {
        "cancel" => {
            let item = item_from_value(output.get("item"));
            let mut lines = vec!["请确认是否取消这条待办".to_owned()];
            let mut markdown_lines = vec!["# 请确认是否取消这条待办".to_owned()];
            if let Some(item) = item.as_ref() {
                lines.push(String::new());
                lines.push(format!("- {}", item.title));
                markdown_lines.push(String::new());
                markdown_lines.push(format!("- {}", item.title));
            }
            lines.push(String::new());
            lines.push("回复“确认”继续，回复“取消”放弃。".to_owned());
            markdown_lines.push(String::new());
            markdown_lines.push("回复“确认”继续，回复“取消”放弃。".to_owned());
            CommandBody::dual(lines.join("\n"), markdown_lines.join("\n"))
        }
        "delete" => {
            let count = output
                .get("items")
                .and_then(Value::as_array)
                .map(Vec::len)
                .or_else(|| output.get("item").map(|_| 1))
                .unwrap_or(0);
            let source = string_field(output, "selection_source");
            let mut lines = vec![format!("准备永久删除 {count} 条待办")];
            let mut markdown_lines = vec![format!("# 准备永久删除 {count} 条待办")];
            if let Some(source) = source {
                lines.push(format!("范围：{source}"));
                markdown_lines.push(format!("范围：{source}"));
            }
            lines.push("永久删除需要二次确认，确认前不会修改数据库。".to_owned());
            lines.push("回复“确认”继续，回复“取消”放弃。".to_owned());
            markdown_lines.push("永久删除需要二次确认，确认前不会修改数据库。".to_owned());
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
        if markdown {
            rows.push(format!(
                "{}. {}",
                index + 1,
                format_todo_inline_markdown(item)
            ));
            rows.push(format!(
                "   - **{}**：{}",
                spec.time_label,
                (spec.time_value)(item)
            ));
        } else {
            rows.push(format!("{}. {}", index + 1, format_todo_inline(item)));
            rows.push(format!(
                "   {}：{}",
                spec.time_label,
                (spec.time_value)(item)
            ));
        }
    }
    if truncated {
        rows.push(String::new());
        rows.push(format!(
            "还有 {} 项，可说“查看全部待办”。",
            total_count.saturating_sub(items.len())
        ));
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
        todo_store.list_all_for_board(owner)
    } else {
        list_for_spec(todo_store, owner, spec)
    }
}

fn todo_write_operation(name: &str) -> Option<TodoWriteOperation> {
    match name {
        "create_todo" => Some(TodoWriteOperation::Create),
        "edit_todo" => Some(TodoWriteOperation::Edit),
        "complete_todos" => Some(TodoWriteOperation::Complete),
        "cancel_todo" => Some(TodoWriteOperation::CancelPending),
        "restore_todos" => Some(TodoWriteOperation::Restore),
        "delete_todos" => Some(TodoWriteOperation::DeletePending),
        _ => None,
    }
}

fn pending_list_spec() -> RelatedListSpec {
    RelatedListSpec {
        status: TodoStatus::Pending,
        query_type: "list",
        condition: "",
        title: "🚧 当前进行中",
        empty_text: "当前没有进行中的待办。",
        time_label: "时间",
        time_value: display_todo_time,
    }
}

fn completed_list_spec() -> RelatedListSpec {
    RelatedListSpec {
        status: TodoStatus::Completed,
        query_type: "completed-list",
        condition: "已完成列表",
        title: "✅ 当前已完成",
        empty_text: "当前没有已完成待办。",
        time_label: "完成时间",
        time_value: display_todo_completed_at,
    }
}

fn cancelled_list_spec() -> RelatedListSpec {
    RelatedListSpec {
        status: TodoStatus::Cancelled,
        query_type: "cancelled-list",
        condition: "已取消列表",
        title: "⛔ 当前已取消",
        empty_text: "当前没有已取消待办。",
        time_label: "取消时间",
        time_value: display_todo_cancelled_at,
    }
}

fn list_spec_from_output(output: &Value) -> RelatedListSpec {
    match string_field(output, "status").as_deref() {
        Some("completed") => completed_list_spec(),
        Some("cancelled") => cancelled_list_spec(),
        Some("all") => RelatedListSpec {
            status: TodoStatus::Pending,
            query_type: "all",
            condition: "全部待办",
            title: "📋 全部待办",
            empty_text: "当前没有待办。",
            time_label: "时间",
            time_value: display_todo_time,
        },
        _ => pending_list_spec(),
    }
}

fn display_todo_completed_at(item: &TodoItem) -> String {
    item.completed_at
        .as_deref()
        .map(format_todo_time_for_display)
        .unwrap_or_else(|| "未知".to_owned())
}

fn display_todo_cancelled_at(item: &TodoItem) -> String {
    item.cancelled_at
        .as_deref()
        .map(format_todo_time_for_display)
        .unwrap_or_else(|| "未知".to_owned())
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

fn affected_item_line(item: &TodoItem) -> String {
    let mut line = format!("- {}", format_todo_inline(item));
    let time = display_todo_time(item);
    if !time.trim().is_empty() {
        line.push_str(&format!("\n  时间：{time}"));
    }
    line
}

fn affected_item_line_markdown(item: &TodoItem) -> String {
    let mut line = format!("- {}", format_todo_inline_markdown(item));
    let time = display_todo_time(item);
    if !time.trim().is_empty() {
        line.push_str(&format!("\n  - **时间**：{time}"));
    }
    line
}

#[derive(Debug, Clone)]
struct ReceiptItem {
    title: String,
    display_time: Option<String>,
}

fn item_from_value(value: Option<&Value>) -> Option<ReceiptItem> {
    let value = value?;
    let title = string_field(value, "title")?;
    Some(ReceiptItem {
        title: truncate_chars(&title, 80),
        display_time: string_field(value, "display_time"),
    })
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
            "进行中的待办不能永久删除；如不再需要，请先取消它。".to_owned()
        }
        Some("todo_delete_mixed_status") => {
            "不能把已完成和已取消待办混在同一次永久删除里。请分状态删除。".to_owned()
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

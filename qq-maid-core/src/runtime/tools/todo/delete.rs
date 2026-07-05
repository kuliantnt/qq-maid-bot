//! `delete_todos` Tool。

use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput, ToolPreparation};

use crate::{
    error::LlmError,
    runtime::{
        pending::PendingOperation,
        session::now_iso_cn,
        todo::{TodoItem, TodoStatus},
    },
};

use super::common::{
    DELETE_TODOS_TOOL_NAME, TODO_DELETE_MIXED_STATUS_CODE, TODO_REFERENCE_UNAVAILABLE_CODE,
    TODO_SELECTION_NOT_FOUND_CODE, bad_tool_arguments, optional_text, todo_numbers_schema,
    todo_reference_schema, todo_selection_request, todo_selection_text_schema, todo_tool_error,
    todo_tool_error_output,
};
use super::json::{status_label, todo_plain_item_json, todo_plain_items_json};
use super::scope::{
    SelectionScope, TodoToolScope, clarification_candidates_for_items, clarification_error_fields,
};
use super::selection::{
    prepare_selection_arguments, prepared_selection_ids, resolved_selection_from_arguments,
};

pub struct DeleteTodoTool {
    todo_store: crate::runtime::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
    /// 受限 Tool Loop 注入的请求级选择作用域；普通调用为 `None`。
    selection_scope: Option<SelectionScope>,
}

impl DeleteTodoTool {
    pub fn new(
        todo_store: crate::runtime::todo::TodoStore,
        session_store: crate::runtime::session::SessionStore,
        _notification_store: crate::storage::notification::NotificationOutboxStore,
    ) -> Self {
        Self {
            todo_store,
            session_store,
            selection_scope: None,
        }
    }

    /// 注入受限 Tool Loop 专属的请求级选择作用域，返回新实例。
    pub(crate) fn with_selection_scope(mut self, scope: SelectionScope) -> Self {
        self.selection_scope = Some(scope);
        self
    }
}

#[async_trait]
impl Tool for DeleteTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: DELETE_TODOS_TOOL_NAME.to_owned(),
            description: "发起永久删除待办，必须二次确认后才真正删除。支持四种互斥选择：numbers=用户最近实际看到的 visible_number；selection_text=用户原始编号范围；reference=\"last\"；query=标题或文本条件；all_status=\"completed\"/\"cancelled\" 表示全部对应状态。用户明确说“删除/永久删除”时使用本工具；用户说“不做了/取消/算了”时使用 cancel_todo。".to_owned(),
            parameters: delete_todos_schema(),
        }
    }

    fn prepare(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolPreparation, LlmError> {
        match delete_selection_request(&arguments)? {
            DeleteSelectionRequest::NumbersOrReference => prepare_selection_arguments(
                &self.session_store,
                &self.todo_store,
                context,
                arguments,
                true,
                self.selection_scope.clone(),
            ),
            DeleteSelectionRequest::Query(_) | DeleteSelectionRequest::AllOfStatus(_) => {
                Ok(ToolPreparation::ready(arguments))
            }
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope =
            TodoToolScope::load(&self.session_store, &context, self.selection_scope.clone())?;
        if let Some(output) = scope.take_dedup_output(&context, &arguments)? {
            return Ok(output);
        }

        let request = delete_selection_request(&arguments)?;
        let selection = match request {
            DeleteSelectionRequest::NumbersOrReference => {
                self.resolve_number_or_reference_selection(&mut scope, &arguments)?
            }
            DeleteSelectionRequest::Query(query) => {
                self.resolve_query_selection(&mut scope, &arguments, &query)?
            }
            DeleteSelectionRequest::AllOfStatus(status) => {
                self.resolve_all_of_status_selection(&scope, status)?
            }
        };

        let output = match selection {
            DeleteSelection::Items {
                items,
                source_condition,
            } => create_delete_confirmation(&mut scope, items, source_condition)?,
            DeleteSelection::Output(output) => output,
        };
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

enum DeleteSelectionRequest {
    NumbersOrReference,
    Query(String),
    AllOfStatus(TodoStatus),
}

enum DeleteSelection {
    Items {
        items: Vec<TodoItem>,
        source_condition: String,
    },
    Output(ToolOutput),
}

fn delete_todos_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "numbers": todo_numbers_schema("用户最近实际看到的待办列表 visible_number。只在用户明确说“第 N 个/删除4”时使用。"),
            "selection_text": todo_selection_text_schema(),
            "reference": todo_reference_schema("用户说“刚才那个/它/刚完成的/刚取消的”时传 last。"),
            "query": {
                "type": ["string", "null"],
                "description": "按标题、详情或原始文本在全部待办中查找目标；例如“和老公出门”“飞机票”。"
            },
            "all_status": {
                "type": ["string", "null"],
                "enum": ["completed", "cancelled", null],
                "description": "删除全部已完成或全部已取消待办时使用；只能是 completed 或 cancelled。"
            }
        },
        "required": ["numbers", "selection_text", "reference", "query", "all_status"],
        "additionalProperties": false
    })
}

fn delete_selection_request(arguments: &Value) -> Result<DeleteSelectionRequest, LlmError> {
    let numbers_selected = arguments.get("numbers").is_some_and(|value| match value {
        Value::Null => false,
        Value::Array(values) => !values.is_empty(),
        _ => true,
    });
    let selection_text_selected = arguments
        .get("selection_text")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    let reference_selected = arguments
        .get("reference")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    let query = optional_text(arguments, "query")?;
    let all_status = optional_all_status(arguments)?;
    let selected_count =
        usize::from(numbers_selected || selection_text_selected || reference_selected)
            + usize::from(query.is_some())
            + usize::from(all_status.is_some());
    if selected_count != 1 {
        return Err(bad_tool_arguments(
            "delete_todos requires exactly one of numbers/reference/query/all_status",
        ));
    }
    if numbers_selected || selection_text_selected || reference_selected {
        // 复用原有 numbers/reference 互斥校验，保持旧参数兼容。
        todo_selection_request(arguments, true)?;
        return Ok(DeleteSelectionRequest::NumbersOrReference);
    }
    if let Some(query) = query {
        return Ok(DeleteSelectionRequest::Query(query));
    }
    let Some(status) = all_status else {
        return Err(bad_tool_arguments(
            "delete_todos requires exactly one selector",
        ));
    };
    Ok(DeleteSelectionRequest::AllOfStatus(status))
}

fn optional_all_status(arguments: &Value) -> Result<Option<TodoStatus>, LlmError> {
    match arguments.get("all_status") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => match value.as_str() {
            "completed" => Ok(Some(TodoStatus::Completed)),
            "cancelled" => Ok(Some(TodoStatus::Cancelled)),
            _ => Err(bad_tool_arguments(
                "all_status must be completed, cancelled or null",
            )),
        },
        _ => Err(bad_tool_arguments("all_status must be string or null")),
    }
}

impl DeleteTodoTool {
    fn resolve_number_or_reference_selection(
        &self,
        scope: &mut TodoToolScope,
        arguments: &Value,
    ) -> Result<DeleteSelection, LlmError> {
        let resolved = resolved_selection_from_arguments(scope, &self.todo_store, arguments, true)?;
        if let Some(output) = resolved.error_output.as_ref() {
            let (error_code, message) = clarification_error_fields(output);
            return Ok(DeleteSelection::Output(scope.save_clarification(
                &self.todo_store,
                DELETE_TODOS_TOOL_NAME,
                arguments,
                true,
                error_code,
                message,
            )?));
        }
        let ids = prepared_selection_ids(&resolved);
        if ids.is_empty() {
            return Ok(DeleteSelection::Output(scope.save_clarification(
                &self.todo_store,
                DELETE_TODOS_TOOL_NAME,
                arguments,
                true,
                TODO_SELECTION_NOT_FOUND_CODE,
                "no visible numbers matched",
            )?));
        }
        let items = self.items_by_ids(scope, &ids)?;
        if items.is_empty() {
            return Ok(DeleteSelection::Output(scope.save_clarification(
                &self.todo_store,
                DELETE_TODOS_TOOL_NAME,
                arguments,
                true,
                TODO_REFERENCE_UNAVAILABLE_CODE,
                "selected todos no longer exist",
            )?));
        }
        Ok(DeleteSelection::Items {
            source_condition: selection_source_condition(&items, "选中的待办"),
            items,
        })
    }

    fn resolve_all_of_status_selection(
        &self,
        scope: &TodoToolScope,
        status: TodoStatus,
    ) -> Result<DeleteSelection, LlmError> {
        let items = match status {
            TodoStatus::Completed => self.todo_store.list_completed(&scope.owner),
            TodoStatus::Cancelled => self.todo_store.list_cancelled(&scope.owner),
            TodoStatus::Pending => unreachable!("all_status parser rejects pending"),
        }
        .map_err(todo_tool_error)?;
        if items.is_empty() {
            return Ok(DeleteSelection::Output(todo_tool_error_output(
                TODO_SELECTION_NOT_FOUND_CODE,
                &format!("没有可永久删除的{}。", status_label(&status)),
            )));
        }
        Ok(DeleteSelection::Items {
            source_condition: format!("全部{}", status_label(&status)),
            items,
        })
    }

    fn resolve_query_selection(
        &self,
        scope: &mut TodoToolScope,
        arguments: &Value,
        query: &str,
    ) -> Result<DeleteSelection, LlmError> {
        let all_items = self
            .todo_store
            .list_all(&scope.owner)
            .map_err(todo_tool_error)?;
        let exact = all_items
            .iter()
            .filter(|item| normalized(&item.title) == normalized(query))
            .cloned()
            .collect::<Vec<_>>();
        let matches = if exact.is_empty() {
            all_items
                .into_iter()
                .filter(|item| item_matches_query(item, query))
                .collect::<Vec<_>>()
        } else {
            exact
        };

        if matches.is_empty() {
            return Ok(DeleteSelection::Output(todo_tool_error_output(
                TODO_SELECTION_NOT_FOUND_CODE,
                "no todo matched query",
            )));
        }
        if matches.len() == 1 {
            return Ok(DeleteSelection::Items {
                source_condition: format!("标题或文本“{}”", query.trim()),
                items: matches,
            });
        }

        let candidates = clarification_candidates_for_items(&matches);
        Ok(DeleteSelection::Output(
            scope.save_clarification_with_candidates(
                DELETE_TODOS_TOOL_NAME,
                arguments,
                false,
                TODO_SELECTION_NOT_FOUND_CODE,
                "multiple todos matched query",
                candidates,
            )?,
        ))
    }

    fn items_by_ids(
        &self,
        scope: &TodoToolScope,
        ids: &[String],
    ) -> Result<Vec<TodoItem>, LlmError> {
        let mut items = Vec::new();
        for id in ids {
            let Some(item) = self
                .todo_store
                .get_by_id(&scope.owner, id)
                .map_err(todo_tool_error)?
            else {
                continue;
            };
            items.push(item);
        }
        Ok(items)
    }
}

fn create_delete_confirmation(
    scope: &mut TodoToolScope,
    items: Vec<TodoItem>,
    source_condition: String,
) -> Result<ToolOutput, LlmError> {
    let Some(status) = items.first().map(|item| item.status.clone()) else {
        return Ok(todo_tool_error_output(
            TODO_SELECTION_NOT_FOUND_CODE,
            "no todo selected for deletion",
        ));
    };
    if items.iter().any(|item| item.status != status) {
        return Ok(todo_tool_error_output(
            TODO_DELETE_MIXED_STATUS_CODE,
            "delete_todos requires selected todos to have the same status",
        ));
    }

    scope.ensure_no_pending()?;
    let created_at = now_iso_cn();
    let message = delete_confirmation_message(&items, &status);
    // `TodoDelete` 的历史 Pending 语义是确认后软取消。进行中待办的新版永久删除
    // 必须使用带 status 字段的 `TodoBulkDelete`，避免升级后无法区分旧确认意图。
    if items.len() == 1 && status != TodoStatus::Pending {
        scope.session.pending_operation = Some(PendingOperation::TodoDelete {
            initiator_user_id: scope.owner.user_id.clone(),
            owner_key: scope.owner.key.clone(),
            item: items[0].clone(),
            created_at,
        });
        scope.save()?;
        return Ok(ToolOutput::json(json!({
            "ok": true,
            "requires_confirmation": true,
            "pending_action": "delete",
            "message": message,
            "selection_source": source_condition,
            "item": todo_plain_item_json(&items[0]),
        })));
    }

    scope.session.pending_operation = Some(PendingOperation::TodoBulkDelete {
        initiator_user_id: scope.owner.user_id.clone(),
        owner_key: scope.owner.key.clone(),
        item_ids: items.iter().map(|item| item.id.clone()).collect(),
        matched_count: items.len(),
        status: status.clone(),
        summary: delete_summary(&items, 5),
        source_condition: source_condition.clone(),
        created_at,
    });
    scope.save()?;
    Ok(ToolOutput::json(json!({
        "ok": true,
        "requires_confirmation": true,
        "pending_action": "delete",
        "message": message,
        "selection_source": source_condition,
        "items": todo_plain_items_json(&items),
    })))
}

fn selection_source_condition(items: &[TodoItem], fallback: &str) -> String {
    match items {
        [item] => format!("{}：{}", fallback, item.title),
        _ => format!("{} {} 条", fallback, items.len()),
    }
}

fn delete_confirmation_message(items: &[TodoItem], status: &TodoStatus) -> String {
    match items {
        [item] => format!(
            "准备永久删除待办：{}\n回复“确认”继续，回复“取消”放弃。",
            item.title
        ),
        _ => format!(
            "准备永久删除 {} 条{}：\n{}\n\n回复“确认”继续，回复“取消”放弃。",
            items.len(),
            status_label(status),
            delete_summary(items, 5)
        ),
    }
}

fn delete_summary(items: &[TodoItem], limit: usize) -> String {
    let mut lines = items
        .iter()
        .take(limit)
        .map(|item| format!("- {}", item.title))
        .collect::<Vec<_>>();
    if items.len() > limit {
        lines.push(format!("- ……等 {} 条", items.len()));
    }
    lines.join("\n")
}

fn item_matches_query(item: &TodoItem, query: &str) -> bool {
    let query = normalized(query);
    if query.is_empty() {
        return false;
    }
    normalized(&item.title).contains(&query)
        || item
            .detail
            .as_deref()
            .is_some_and(|value| normalized(value).contains(&query))
        || item
            .raw_text
            .as_deref()
            .is_some_and(|value| normalized(value).contains(&query))
}

fn normalized(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, '“' | '”' | '"' | '\''))
        .collect::<String>()
        .to_lowercase()
}

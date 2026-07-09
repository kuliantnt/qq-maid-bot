//! `merge_todos` Tool。

use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput, ToolPreparation};

use crate::{
    error::LlmError,
    runtime::tools::todo::{TodoItem, TodoItemDraft, TodoStatus},
    storage::notification::NotificationOutboxStore,
};

use super::sync_reminder_task;

use super::common::{
    MERGE_TODOS_TOOL_NAME, TODO_REFERENCE_INVALID_STATE_CODE, TODO_SELECTION_NOT_FOUND_CODE,
    TodoSelectionRequest, bad_tool_arguments, todo_tool_error, todo_tool_error_output,
};
use super::json::todo_plain_item_json;
use super::scope::{SelectionScope, TodoToolScope, TodoToolSelectionResolution};

pub struct MergeTodoTool {
    todo_store: crate::runtime::tools::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
    notification_store: NotificationOutboxStore,
    selection_scope: Option<SelectionScope>,
}

impl MergeTodoTool {
    pub fn new(
        todo_store: crate::runtime::tools::todo::TodoStore,
        session_store: crate::runtime::session::SessionStore,
        notification_store: NotificationOutboxStore,
    ) -> Self {
        Self {
            todo_store,
            session_store,
            notification_store,
            selection_scope: None,
        }
    }

    pub(crate) fn with_selection_scope(mut self, scope: SelectionScope) -> Self {
        self.selection_scope = Some(scope);
        self
    }
}

#[async_trait]
impl Tool for MergeTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: MERGE_TODOS_TOOL_NAME.to_owned(),
            description: "合并两个未完成待办。source_number 是要被合入并物理删除的 visible_number，target_number 是保留并更新的 visible_number。用户说“把 7 合并到 6”时 source_number=7,target_number=6；用户说“6 和 7 合并”且未明确方向时，保守追问，不要调用本工具。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "source_number": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "源待办 visible_number；合并后会物理删除。"
                    },
                    "target_number": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "目标待办 visible_number；合并后保留。"
                    }
                },
                "required": ["source_number", "target_number"],
                "additionalProperties": false
            }),
        }
    }

    fn prepare(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<ToolPreparation, LlmError> {
        let source = required_number(&arguments, "source_number")?;
        let target = required_number(&arguments, "target_number")?;
        let mut scope =
            TodoToolScope::load(&self.session_store, context, self.selection_scope.clone())?;
        let resolved = match scope.resolve_selection(
            &TodoSelectionRequest::Numbers(vec![target, source]),
            &self.todo_store,
        )? {
            TodoToolSelectionResolution::Resolved(resolved) => resolved,
            TodoToolSelectionResolution::Output(output) => {
                let mut prepared = arguments;
                prepared["_error_output"] = output.value;
                return Ok(ToolPreparation::ready(prepared));
            }
        };
        let mut prepared = arguments;
        let object = prepared
            .as_object_mut()
            .ok_or_else(|| bad_tool_arguments("tool arguments must be a JSON object"))?;
        if let Some(id) = resolved
            .matched
            .iter()
            .find(|(label, _)| label_visible_number(label) == Some(target))
            .map(|(_, id)| json!(id))
        {
            object.insert("_target_id".to_owned(), id);
        }
        if let Some(id) = resolved
            .matched
            .iter()
            .find(|(label, _)| label_visible_number(label) == Some(source))
            .map(|(_, id)| json!(id))
        {
            object.insert("_source_id".to_owned(), id);
        }
        if !resolved.missing.is_empty() || resolved.error_output.is_some() {
            object.insert(
                "_error_output".to_owned(),
                json!({
                    "ok": false,
                    "error_code": TODO_SELECTION_NOT_FOUND_CODE,
                    "message": "visible number not found"
                }),
            );
        }
        Ok(ToolPreparation::ready(prepared))
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope =
            TodoToolScope::load(&self.session_store, &context, self.selection_scope.clone())?;
        if let Some(output) = arguments.get("_error_output").cloned() {
            return Ok(ToolOutput::json(output));
        }
        let target_id =
            match prepared_id_or_resolve(&mut scope, &self.todo_store, &arguments, "target")? {
                PreparedMergeId::Resolved(id) => id,
                PreparedMergeId::Output(output) => return Ok(output),
            };
        let source_id =
            match prepared_id_or_resolve(&mut scope, &self.todo_store, &arguments, "source")? {
                PreparedMergeId::Resolved(id) => id,
                PreparedMergeId::Output(output) => return Ok(output),
            };
        if target_id == source_id {
            return Ok(todo_tool_error_output(
                TODO_SELECTION_NOT_FOUND_CODE,
                "source and target must be different todos",
            ));
        }

        let Some(target) = self
            .todo_store
            .get_by_id(&scope.owner, &target_id)
            .map_err(todo_tool_error)?
        else {
            return Ok(todo_tool_error_output(
                TODO_SELECTION_NOT_FOUND_CODE,
                "target todo no longer exists",
            ));
        };
        let Some(source) = self
            .todo_store
            .get_by_id(&scope.owner, &source_id)
            .map_err(todo_tool_error)?
        else {
            return Ok(todo_tool_error_output(
                TODO_SELECTION_NOT_FOUND_CODE,
                "source todo no longer exists",
            ));
        };
        if target.status != TodoStatus::Pending || source.status != TodoStatus::Pending {
            return Ok(todo_tool_error_output(
                TODO_REFERENCE_INVALID_STATE_CODE,
                "merge_todos only accepts pending todos",
            ));
        }

        let mut draft =
            TodoItemDraft::from_item(&target, target.raw_text.clone().unwrap_or_default());
        draft.detail = Some(merged_detail(&target, &source));
        draft.raw_text = Some(format!("{}\n{}", target.title.trim(), source.title.trim()));
        let updated = self
            .todo_store
            .edit(&scope.owner, &target.id, draft)
            .map_err(todo_tool_error)?;
        if let Err(message) = sync_reminder_task(&self.notification_store, &scope.owner, &updated) {
            scope.session.last_todo_query = None;
            scope
                .session
                .remember_last_todo_action(&scope.owner.key, &updated, "merged_partial");
            scope.save()?;
            return Ok(ToolOutput::json(json!({
                "ok": false,
                "partial_failure": true,
                "error_code": "todo_merge_reminder_sync_failed",
                "message": message,
                "target": todo_plain_item_json(&updated),
                "source": todo_plain_item_json(&source),
            })));
        }

        let delete_outcome = self
            .todo_store
            .delete_pending_by_ids(&scope.owner, std::slice::from_ref(&source.id))
            .map_err(todo_tool_error)?;
        if delete_outcome.deleted_count == 0 {
            scope.session.last_todo_query = None;
            scope
                .session
                .remember_last_todo_action(&scope.owner.key, &updated, "merged");
            scope.save()?;
            return Ok(ToolOutput::json(json!({
                "ok": false,
                "partial_failure": true,
                "error_code": "todo_merge_source_delete_failed",
                "message": "target updated but source was not deleted",
                "target": todo_plain_item_json(&updated),
                "source": todo_plain_item_json(&source),
            })));
        }

        scope.session.last_todo_query = None;
        scope
            .session
            .remember_last_todo_action(&scope.owner.key, &updated, "merged");
        scope.clear_clarification_if_scoped();
        scope.save()?;
        Ok(ToolOutput::json(json!({
            "ok": true,
            "merged": {
                "target": todo_plain_item_json(&updated),
                "source_deleted": todo_plain_item_json(&source),
            },
            "message": "已合并待办；源项已物理删除。",
        })))
    }
}

fn required_number(arguments: &Value, field: &str) -> Result<usize, LlmError> {
    let value = arguments
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| bad_tool_arguments(format!("{field} must be a positive integer")))?;
    usize::try_from(value).map_err(|_| bad_tool_arguments(format!("{field} is too large")))
}

fn label_visible_number(label: &super::common::TodoSelectionLabel) -> Option<usize> {
    match label {
        super::common::TodoSelectionLabel::Number(number) => Some(*number),
        super::common::TodoSelectionLabel::Reference(_) => None,
    }
}

fn prepared_id_or_resolve(
    scope: &mut TodoToolScope,
    todo_store: &crate::runtime::tools::todo::TodoStore,
    arguments: &Value,
    role: &str,
) -> Result<PreparedMergeId, LlmError> {
    let id_key = format!("_{role}_id");
    if let Some(id) = arguments.get(&id_key).and_then(Value::as_str) {
        return Ok(PreparedMergeId::Resolved(id.to_owned()));
    }
    let number = required_number(arguments, &format!("{role}_number"))?;
    let resolved =
        match scope.resolve_selection(&TodoSelectionRequest::Numbers(vec![number]), todo_store)? {
            TodoToolSelectionResolution::Resolved(resolved) => resolved,
            TodoToolSelectionResolution::Output(output) => {
                return Ok(PreparedMergeId::Output(output));
            }
        };
    if let Some((_, id)) = resolved.matched.first() {
        return Ok(PreparedMergeId::Resolved(id.clone()));
    }
    Ok(PreparedMergeId::Output(todo_tool_error_output(
        TODO_SELECTION_NOT_FOUND_CODE,
        "visible number not found",
    )))
}

enum PreparedMergeId {
    Resolved(String),
    Output(ToolOutput),
}

fn merged_detail(target: &TodoItem, source: &TodoItem) -> String {
    let mut parts = Vec::new();
    if let Some(detail) = target
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(detail.to_owned());
    }
    parts.push(format!("合并来源：{}", source.title.trim()));
    if let Some(detail) = source
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(detail.to_owned());
    }
    parts.join("\n")
}

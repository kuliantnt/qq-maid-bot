//! `delete_todos` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::{pending::PendingOperation, session::now_iso_cn, todo::TodoStatus},
};

use super::common::{
    DELETE_TODOS_TOOL_NAME, TODO_DELETE_INVALID_STATE_CODE, TODO_DELETE_MIXED_STATUS_CODE,
    TODO_REFERENCE_UNAVAILABLE_CODE, TODO_SELECTION_NOT_FOUND_CODE,
    number_list_or_reference_schema, todo_tool_error, todo_tool_error_output,
};
use super::json::{status_label, todo_items_json};
use super::scope::TodoToolScope;
use super::selection::{
    prepare_selection_arguments, prepared_selection_ids, resolved_selection_from_arguments,
    todo_selection_label_text,
};

pub struct DeleteTodoTool {
    todo_store: crate::runtime::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
}

impl DeleteTodoTool {
    pub fn new(
        todo_store: crate::runtime::todo::TodoStore,
        session_store: crate::runtime::session::SessionStore,
    ) -> Self {
        Self {
            todo_store,
            session_store,
        }
    }
}

#[async_trait]
impl Tool for DeleteTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: DELETE_TODOS_TOOL_NAME.to_owned(),
            description: "发起永久删除已完成或已取消待办。用户明确说“第 N 个”时只能传 numbers 并依赖最近一次 list_todos 的 visible_number；用户说“刚才那个 / 它 / 恢复的那个 / 刚完成的”时传 reference=\"last\"。未完成待办不能用本工具永久删除；用户说“不做了/取消/算了”时必须调用 cancel_todo。需要用户确认后才执行。".to_owned(),
            parameters: number_list_or_reference_schema("要永久删除的 visible_number 列表"),
        }
    }

    fn prepare(
        &self,
        context: &ToolContext,
        arguments: serde_json::Value,
    ) -> Result<qq_maid_llm::tool::ToolPreparation, LlmError> {
        prepare_selection_arguments(
            &self.session_store,
            &self.todo_store,
            context,
            arguments,
            true,
        )
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope = TodoToolScope::load(&self.session_store, &context)?;
        if let Some(output) = scope.take_dedup_output(&context, &arguments)? {
            return Ok(output);
        }
        let resolved =
            resolved_selection_from_arguments(&mut scope, &self.todo_store, &arguments, true)?;
        let ids = prepared_selection_ids(&resolved);
        if ids.is_empty() {
            return Ok(todo_tool_error_output(
                TODO_SELECTION_NOT_FOUND_CODE,
                "no visible numbers matched",
            ));
        }

        let mut items = Vec::new();
        for id in &ids {
            let Some(item) = self
                .todo_store
                .get_by_id(&scope.owner, id)
                .map_err(todo_tool_error)?
            else {
                continue;
            };
            items.push(item);
        }
        if items.is_empty() {
            return Ok(todo_tool_error_output(
                TODO_REFERENCE_UNAVAILABLE_CODE,
                "selected todos no longer exist",
            ));
        }
        if items.iter().any(|item| item.status == TodoStatus::Pending) {
            return Ok(todo_tool_error_output(
                TODO_DELETE_INVALID_STATE_CODE,
                "pending todos cannot be permanently deleted; use cancel_todo to mark them cancelled",
            ));
        }
        let status = items[0].status.clone();
        if items.iter().any(|item| item.status != status) {
            return Ok(todo_tool_error_output(
                TODO_DELETE_MIXED_STATUS_CODE,
                "delete_todos requires all selected todos to have the same terminal status",
            ));
        }

        scope.ensure_no_pending()?;
        let source_condition = format!(
            "{}编号 {}",
            status_label(&status),
            resolved
                .labels
                .iter()
                .map(todo_selection_label_text)
                .collect::<Vec<_>>()
                .join("、")
        );
        scope.session.pending_operation = Some(PendingOperation::TodoBulkDelete {
            initiator_user_id: scope.owner.user_id.clone(),
            owner_key: scope.owner.key.clone(),
            item_ids: items.iter().map(|item| item.id.clone()).collect(),
            matched_count: items.len(),
            status: status.clone(),
            summary: items
                .iter()
                .take(5)
                .map(|item| format!("- {}", item.title))
                .collect::<Vec<_>>()
                .join("\n"),
            source_condition: source_condition.clone(),
            created_at: now_iso_cn(),
        });
        scope.save()?;

        let output = ToolOutput::json(json!({
            "ok": true,
            "requires_confirmation": true,
            "pending_action": "delete",
            "message": "已发起永久删除确认；只针对已完成或已取消待办，用户确认后才会删除记录。",
            "source_condition": source_condition,
            "items": todo_items_json(&items),
        }));
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

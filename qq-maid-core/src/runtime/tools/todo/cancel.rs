//! `cancel_todo` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{error::LlmError, runtime::todo::TodoStatus};

use super::common::{
    CANCEL_TODO_TOOL_NAME, TODO_REFERENCE_INVALID_STATE_CODE, TODO_SELECTION_NOT_FOUND_CODE,
    number_list_or_reference_schema, todo_tool_error, todo_tool_error_output,
};
use super::json::todo_selected_items_json;
use super::scope::{SelectionScope, TodoToolScope, clarification_error_fields};
use super::selection::{
    missing_numbers_json, missing_selection_labels_for_result, prepare_selection_arguments,
    prepared_selection_ids, resolved_selection_from_arguments, selected_items_for_result,
};

pub struct CancelTodoTool {
    todo_store: crate::runtime::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
    /// 受限 Tool Loop 注入的请求级选择作用域；普通调用为 `None`。
    selection_scope: Option<SelectionScope>,
}

impl CancelTodoTool {
    pub fn new(
        todo_store: crate::runtime::todo::TodoStore,
        session_store: crate::runtime::session::SessionStore,
    ) -> Self {
        Self {
            todo_store,
            session_store,
            selection_scope: None,
        }
    }

    /// 注入受限 Tool Loop 专属的请求级选择作用域，返回新实例。
    pub fn with_selection_scope(mut self, scope: SelectionScope) -> Self {
        self.selection_scope = Some(scope);
        self
    }
}

#[async_trait]
impl Tool for CancelTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: CANCEL_TODO_TOOL_NAME.to_owned(),
            description: "取消一个或多个未完成待办，直接把状态变更为已取消，不需要二次确认。用户明确说“第 N 个/1-5/1,3,5”时传 numbers 或 selection_text 并依赖最近一次 list_todos 的 visible_number；用户说“刚才那个 / 它 / 恢复的那个”时传 reference=\"last\"。取消不是永久删除。".to_owned(),
            parameters: number_list_or_reference_schema("要取消的 visible_number 列表"),
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
            self.selection_scope.clone(),
        )
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope =
            TodoToolScope::load(&self.session_store, &context, self.selection_scope.clone())?;
        if let Some(output) = scope.take_dedup_output(&context, &arguments)? {
            return Ok(output);
        }
        let resolved =
            resolved_selection_from_arguments(&mut scope, &self.todo_store, &arguments, true)?;
        if let Some(output) = resolved.error_output.as_ref() {
            let (error_code, message) = clarification_error_fields(output);
            return scope.save_clarification(
                &self.todo_store,
                CANCEL_TODO_TOOL_NAME,
                &arguments,
                true,
                error_code,
                message,
            );
        }
        let ids = prepared_selection_ids(&resolved);
        if ids.is_empty() {
            return scope.save_clarification(
                &self.todo_store,
                CANCEL_TODO_TOOL_NAME,
                &arguments,
                true,
                TODO_SELECTION_NOT_FOUND_CODE,
                "no visible numbers matched",
            );
        }
        let selected_items = ids
            .iter()
            .filter_map(|id| self.todo_store.get_by_id(&scope.owner, id).ok().flatten())
            .collect::<Vec<_>>();
        if selected_items
            .iter()
            .any(|item| item.status != TodoStatus::Pending)
        {
            return Ok(todo_tool_error_output(
                TODO_REFERENCE_INVALID_STATE_CODE,
                "cancel_todo only accepts pending todos; use restore_todos or delete_todos for terminal states",
            ));
        }

        scope.ensure_no_pending()?;
        let outcome = crate::runtime::todo::ops::cancel_many(
            &self.todo_store,
            &mut scope.session,
            &scope.owner,
            &ids,
        )
        .map_err(todo_tool_error)?;
        let cancelled = selected_items_for_result(&resolved, &outcome.cancelled);
        let missing = missing_selection_labels_for_result(&resolved, &outcome.skipped_ids);
        if !cancelled.is_empty() {
            scope.clear_clarification_if_scoped();
        }
        scope.save()?;

        let output = ToolOutput::json(json!({
            "ok": true,
            "cancelled": todo_selected_items_json(&cancelled),
            "missing_numbers": missing_numbers_json(&missing),
            "message": "已取消的条目已变更为 cancelled；missing_numbers 表示编号不存在、状态不是未完成或条目已变化。",
        }));
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

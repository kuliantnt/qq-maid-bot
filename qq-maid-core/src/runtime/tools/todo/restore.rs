//! `restore_todos` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::error::LlmError;

use super::common::{
    RESTORE_TODOS_TOOL_NAME, TODO_SELECTION_NOT_FOUND_CODE, number_list_or_reference_schema,
    todo_tool_error,
};
use super::json::todo_selected_items_json;
use super::scope::{SelectionScope, TodoToolScope, clarification_error_fields};
use super::selection::{
    missing_numbers_json, missing_selection_labels_excluding_items, prepare_selection_arguments,
    prepared_selection_ids, resolved_selection_from_arguments, selected_items_for_result,
};

pub struct RestoreTodoTool {
    todo_store: crate::runtime::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
    /// 受限 Tool Loop 注入的请求级选择作用域；普通调用为 `None`。
    selection_scope: Option<SelectionScope>,
}

impl RestoreTodoTool {
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
impl Tool for RestoreTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: RESTORE_TODOS_TOOL_NAME.to_owned(),
            description: "将已完成或已取消待办恢复为未完成。用户明确说“第 N 个”时只能传 numbers 并依赖最近一次 list_todos 的 visible_number；用户说“刚才那个 / 它 / 恢复的那个”时传 reference=\"last\"。不会接受数据库内部 ID。".to_owned(),
            parameters: number_list_or_reference_schema("要恢复的 visible_number 列表"),
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
                RESTORE_TODOS_TOOL_NAME,
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
                RESTORE_TODOS_TOOL_NAME,
                &arguments,
                true,
                TODO_SELECTION_NOT_FOUND_CODE,
                "no visible numbers matched",
            );
        }
        // 同时恢复已完成/已取消两类待办 + 清空 last_todo_query / 更新 last_todo_action
        // 统一由 ops 门面维护，避免与指令侧重写同一套时序。
        let outcome = crate::runtime::todo::ops::restore_both(
            &self.todo_store,
            &mut scope.session,
            &scope.owner,
            &ids,
        )
        .map_err(todo_tool_error)?;
        let mut restored = selected_items_for_result(&resolved, &outcome.completed.restored);
        restored.extend(selected_items_for_result(
            &resolved,
            &outcome.cancelled.restored,
        ));
        let missing = missing_selection_labels_excluding_items(&resolved, &restored);
        if !restored.is_empty() {
            // 状态变化后清空旧编号快照，避免模型继续沿用已变更的列表；
            // 快照清空和最近对象记忆已由 ops::restore_both 统一维护。
            scope.clear_clarification_if_scoped();
            scope.save()?;
        }

        let output = ToolOutput::json(json!({
            "ok": true,
            "restored": todo_selected_items_json(&restored),
            "missing_numbers": missing_numbers_json(&missing),
            "message": "已恢复的条目已变更为 pending；missing_numbers 表示编号不存在、状态不是已完成/已取消或条目已变化。"
        }));
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

//! `complete_todos` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{error::LlmError, storage::notification::NotificationOutboxStore};

use super::{cancel_reminder_task, sync_reminder_task};

use super::common::{
    COMPLETE_TODOS_TOOL_NAME, TODO_SELECTION_NOT_FOUND_CODE, number_list_or_reference_schema,
    todo_tool_error,
};
use super::json::todo_selected_items_json;
use super::scope::{SelectionScope, TodoToolScope, clarification_error_fields};
use super::selection::{
    missing_numbers_json, missing_selection_labels_excluding_items, prepare_selection_arguments,
    prepared_selection_ids, resolved_selection_from_arguments, selected_items_for_result,
};

pub struct CompleteTodoTool {
    todo_store: crate::runtime::tools::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
    notification_store: NotificationOutboxStore,
    /// 受限 Tool Loop 注入的请求级选择作用域；普通调用为 `None`。
    selection_scope: Option<SelectionScope>,
}

impl CompleteTodoTool {
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

    /// 注入受限 Tool Loop 专属的请求级选择作用域，返回新实例。
    ///
    /// 该作用域不写入 Session / 数据库，仅用于本次受限 Loop 把模型给的可见编号映射到
    /// 本次澄清候选。
    pub(crate) fn with_selection_scope(mut self, scope: SelectionScope) -> Self {
        self.selection_scope = Some(scope);
        self
    }
}

#[async_trait]
impl Tool for CompleteTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: COMPLETE_TODOS_TOOL_NAME.to_owned(),
            description: "将未完成待办标记为已完成。用户明确说“第 N 个”时只能传 numbers 并依赖最近一次 list_todos 的 visible_number；用户说“刚才那个 / 它 / 刚恢复的那个 / 刚完成的”时传 reference=\"last\"。不会接受数据库内部 ID。".to_owned(),
            parameters: number_list_or_reference_schema("要完成的 visible_number 列表"),
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
                COMPLETE_TODOS_TOOL_NAME,
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
                COMPLETE_TODOS_TOOL_NAME,
                &arguments,
                true,
                TODO_SELECTION_NOT_FOUND_CODE,
                "no visible numbers matched",
            );
        }
        // 重复待办在这里按“完成本次后推进下一次”处理；一次性待办仍进入 completed。
        let outcome = crate::runtime::tools::todo::ops::complete_many_with_recurrence(
            &self.todo_store,
            &mut scope.session,
            &scope.owner,
            &ids,
        )
        .map_err(todo_tool_error)?;
        for item in &outcome.completed {
            cancel_reminder_task(&self.notification_store, item).map_err(|message| {
                LlmError::new("todo_reminder_cancel_failed", message, "todo_tool")
            })?;
        }
        for item in &outcome.advanced {
            sync_reminder_task(&self.notification_store, &scope.owner, item).map_err(
                |message| LlmError::new("todo_reminder_sync_failed", message, "todo_tool"),
            )?;
        }
        let completed = selected_items_for_result(&resolved, &outcome.completed);
        let advanced = selected_items_for_result(&resolved, &outcome.advanced);
        let mut changed = completed.clone();
        changed.extend(advanced.clone());
        let missing = missing_selection_labels_excluding_items(&resolved, &changed);
        if !changed.is_empty() {
            // 状态变化后清空旧编号快照，避免模型继续沿用已变更的列表。
            scope.clear_clarification_if_scoped();
            scope.save()?;
        }

        let output = ToolOutput::json(json!({
            "ok": true,
            "completed": todo_selected_items_json(&completed),
            "advanced": todo_selected_items_json(&advanced),
            "missing_numbers": missing_numbers_json(&missing),
            "message": "一次性待办会变更为 completed；重复待办会完成本次并推进到下一次。missing_numbers 表示编号不存在、状态不是未完成或条目已变化。"
        }));
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

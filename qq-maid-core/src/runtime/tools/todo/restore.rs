//! `restore_todos` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{error::LlmError, storage::notification::NotificationOutboxStore};

use super::sync_reminder_task;

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
    todo_store: crate::runtime::tools::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
    notification_store: NotificationOutboxStore,
    /// 受限 Tool Loop 注入的请求级选择作用域；普通调用为 `None`。
    selection_scope: Option<SelectionScope>,
}

impl RestoreTodoTool {
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
    pub(crate) fn with_selection_scope(mut self, scope: SelectionScope) -> Self {
        self.selection_scope = Some(scope);
        self
    }
}

#[async_trait]
impl Tool for RestoreTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: RESTORE_TODOS_TOOL_NAME.to_owned(),
            description: "撤销待办的完成状态，将已完成待办恢复为未完成。用户说“撤销完成 / 刚才那条还没做完 / 取消刚才的完成操作”时传 reference=\"last\"，定位最近一次完成的待办；用户明确说“第 N 个 / 第一条改回未完成”时只能传 numbers，并依赖最近一次用户可见列表或本轮 list_todos 的 visible_number；用户引用一条已完成待办时按引用快照传 numbers。用户明确给出标题但没有可用编号时，先调用 list_todos(status=\"completed\") 匹配标题，再传对应 visible_number；无法唯一确定目标时必须追问。不会接受数据库内部 ID。".to_owned(),
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
        // 只恢复已完成待办；“取消”已收敛为删除语义，不再提供软取消恢复入口。
        let outcome = crate::runtime::tools::todo::ops::restore_completed_many(
            &self.todo_store,
            &mut scope.session,
            &scope.owner,
            &ids,
        )
        .map_err(todo_tool_error)?;
        for item in &outcome.restored {
            sync_reminder_task(&self.notification_store, &scope.owner, item).map_err(
                |message| LlmError::new("todo_reminder_sync_failed", message, "todo_tool"),
            )?;
        }
        let restored = selected_items_for_result(&resolved, &outcome.restored);
        let missing = missing_selection_labels_excluding_items(&resolved, &restored);
        if !restored.is_empty() {
            // 状态变化后清空旧编号快照，避免模型继续沿用已变更的列表；
            // 快照清空和最近对象记忆已由 ops::restore_completed_many 统一维护。
            scope.clear_clarification_if_scoped();
            scope.save()?;
        }

        let output = ToolOutput::json(json!({
            "ok": true,
            "restored": todo_selected_items_json(&restored),
            "missing_numbers": missing_numbers_json(&missing),
            "message": "已恢复的条目已变更为 pending；missing_numbers 表示编号不存在、状态不是已完成或条目已变化。"
        }));
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

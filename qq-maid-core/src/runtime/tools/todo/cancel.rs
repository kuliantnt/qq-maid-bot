//! `cancel_todo` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::{pending::PendingOperation, session::now_iso_cn, todo::TodoStatus},
};

use super::common::{
    CANCEL_TODO_TOOL_NAME, TODO_REFERENCE_INVALID_STATE_CODE, single_number_or_reference_schema,
    todo_tool_error_output,
};
use super::json::todo_selected_item_json;
use super::scope::{TodoToolScope, TodoToolSingleItemResolution};
use super::selection::{prepare_selection_arguments, resolved_selection_from_arguments};

pub struct CancelTodoTool {
    todo_store: crate::runtime::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
}

impl CancelTodoTool {
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
impl Tool for CancelTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: CANCEL_TODO_TOOL_NAME.to_owned(),
            description: "发起取消未完成待办。用户明确说“第 N 个”时只能传 number 并依赖最近一次 list_todos 的 visible_number；用户说“刚才那个 / 它 / 恢复的那个”时传 reference=\"last\"。取消只是状态变更为已取消，不是永久删除；需要用户确认后才执行。".to_owned(),
            parameters: single_number_or_reference_schema("要取消的 visible_number"),
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
            false,
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
            resolved_selection_from_arguments(&mut scope, &self.todo_store, &arguments, false)?;
        let item = match resolved.single_item(&self.todo_store, &scope.owner)? {
            TodoToolSingleItemResolution::Item(item) => *item,
            TodoToolSingleItemResolution::Output(output) => return Ok(output),
        };
        if item.status != TodoStatus::Pending {
            return Ok(todo_tool_error_output(
                TODO_REFERENCE_INVALID_STATE_CODE,
                "cancel_todo only accepts pending todos; use restore_todos or delete_todos for terminal states",
            ));
        }

        scope.ensure_no_pending()?;
        scope.session.pending_operation = Some(PendingOperation::TodoDelete {
            initiator_user_id: scope.owner.user_id.clone(),
            owner_key: scope.owner.key.clone(),
            item: item.clone(),
            created_at: now_iso_cn(),
        });
        scope.save()?;

        let output = ToolOutput::json(json!({
            "ok": true,
            "requires_confirmation": true,
            "pending_action": "cancel",
            "message": "已发起取消待办确认；用户确认后只会标记为已取消，不会永久删除。",
            "item": todo_selected_item_json(resolved.single_label(), &item),
        }));
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

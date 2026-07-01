//! `list_todos` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::error::LlmError;

use super::common::{LIST_TODOS_TOOL_NAME, todo_status_argument, todo_tool_error};
use super::json::todo_items_json;
use super::scope::TodoToolScope;

pub struct ListTodoTool {
    todo_store: crate::runtime::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
}

impl ListTodoTool {
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
impl Tool for ListTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: LIST_TODOS_TOOL_NAME.to_owned(),
            description: "查询当前私聊用户的待办列表。不会返回数据库内部 ID；visible_number 只供本轮 Tool Loop 内部推理和后续工具调用使用，不会覆盖用户跨轮次真正看到的列表编号。status=pending 查询未完成，completed 查询已完成，cancelled 查询已取消，all 查询全部。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["pending", "completed", "cancelled", "all"],
                        "description": "要查询的待办状态"
                    }
                },
                "required": ["status"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, LlmError> {
        use super::common::TodoToolListStatus;

        let mut scope = TodoToolScope::load(&self.session_store, &context, None)?;
        let status = todo_status_argument(&arguments, "status")?;
        let items = match status {
            TodoToolListStatus::Pending => self.todo_store.list_pending(&scope.owner),
            TodoToolListStatus::Completed => self.todo_store.list_completed(&scope.owner),
            TodoToolListStatus::Cancelled => self.todo_store.list_cancelled(&scope.owner),
            // Tool 可见编号也必须和 `/todo all` 看板一致，否则模型随后按“第 N 个”
            // 调用 complete/restore/delete 时会绑定到用户没有按该顺序看到的条目。
            TodoToolListStatus::All => self.todo_store.list_all_for_board(&scope.owner),
        }
        .map_err(todo_tool_error)?;
        scope.remember_internal_query(status.query_type(), status.condition(), &items)?;

        Ok(ToolOutput::json(json!({
            "status": status.as_str(),
            "items": todo_items_json(&items),
            "count": items.len(),
            "numbering": "visible_number 是本轮工具查询编号，仅在当前 Tool Loop 内有效；用户跨轮次的第 N 条仍以最近实际展示给用户的 /todo 列表为准；未暴露数据库内部 ID。"
        })))
    }
}

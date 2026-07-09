//! `get_todo` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::error::LlmError;

use super::common::{GET_TODO_TOOL_NAME, single_number_or_reference_schema};
use super::json::todo_selected_item_json;
use super::scope::{SelectionScope, TodoToolScope, TodoToolSingleItemResolution};
use super::selection::{prepare_selection_arguments, resolved_selection_from_arguments};

pub struct GetTodoTool {
    todo_store: crate::runtime::tools::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
    selection_scope: Option<SelectionScope>,
}

impl GetTodoTool {
    pub fn new(
        todo_store: crate::runtime::tools::todo::TodoStore,
        session_store: crate::runtime::session::SessionStore,
    ) -> Self {
        Self {
            todo_store,
            session_store,
            selection_scope: None,
        }
    }

    pub(crate) fn with_selection_scope(mut self, scope: SelectionScope) -> Self {
        self.selection_scope = Some(scope);
        self
    }
}

#[async_trait]
impl Tool for GetTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: GET_TODO_TOOL_NAME.to_owned(),
            description: "查询单个待办详情。用户明确说“第 N 个”时传 number 或 numbers=[N]，依赖最近一次用户实际看到的 Todo 列表或本轮 list_todos 的 visible_number；用户说“刚才那个 / 它 / 刚恢复的那个 / 刚完成的”时传 reference=\"last\"。不会接受数据库内部 ID，不会修改待办或刷新用户可见编号快照。".to_owned(),
            parameters: single_number_or_reference_schema("要查询的 visible_number"),
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
        let resolved =
            resolved_selection_from_arguments(&mut scope, &self.todo_store, &arguments, false)?;
        let label = resolved.single_label();
        match resolved.single_item(&self.todo_store, &scope.owner)? {
            TodoToolSingleItemResolution::Item(item) => Ok(ToolOutput::json(json!({
                "ok": true,
                "item": todo_selected_item_json(label, &item),
                "message": "已查询到单个待办；未暴露数据库内部 ID，未修改待办状态或用户可见编号快照。"
            }))),
            TodoToolSingleItemResolution::Output(output) => Ok(output),
        }
    }
}

//! `list_todos` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::error::LlmError;

use super::common::{
    LIST_TODOS_TOOL_NAME, bad_tool_arguments, optional_text, todo_status_argument, todo_tool_error,
};
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
            description: "查询当前私聊用户的待办列表。不会返回数据库内部 ID；visible_number 只供本轮 Tool Loop 内部推理和后续工具调用使用，不会覆盖用户跨轮次真正看到的列表编号。status=pending 查询未完成，completed 查询已完成，cancelled 查询已取消，all 查询全部。查询今天、明天或明确日期内的待办时传 due_date=YYYY-MM-DD；日期条件按本地自然日匹配计划时间/到期时间，无时间待办不会命中。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["pending", "completed", "cancelled", "all"],
                        "description": "要查询的待办状态"
                    },
                    "due_date": {
                        "type": ["string", "null"],
                        "description": "按计划日期筛选，格式 YYYY-MM-DD；例如今天/明天查询时由模型先换算为具体本地日期。无日期筛选时必须传 null。"
                    }
                },
                "required": ["status", "due_date"],
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
        let due_date = optional_text(&arguments, "due_date")?
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(|value| {
                let ctx = crate::util::time_context::request_time_context();
                crate::util::time_context::parse_single_date_expression(&value, &ctx)
                    .map(|date| date.date)
                    .ok_or_else(|| bad_tool_arguments("due_date must be a valid YYYY-MM-DD date"))
            })
            .transpose()?;
        let items = match (status, due_date) {
            (TodoToolListStatus::Pending, Some(date)) => self.todo_store.list_by_due_date(
                &scope.owner,
                crate::runtime::todo::TodoStatus::Pending,
                date,
            ),
            (TodoToolListStatus::Completed, Some(date)) => self.todo_store.list_by_due_date(
                &scope.owner,
                crate::runtime::todo::TodoStatus::Completed,
                date,
            ),
            (TodoToolListStatus::Cancelled, Some(date)) => self.todo_store.list_by_due_date(
                &scope.owner,
                crate::runtime::todo::TodoStatus::Cancelled,
                date,
            ),
            (TodoToolListStatus::All, Some(date)) => self
                .todo_store
                .list_all_by_due_date_for_board(&scope.owner, date),
            (TodoToolListStatus::Pending, None) => self.todo_store.list_pending(&scope.owner),
            (TodoToolListStatus::Completed, None) => self.todo_store.list_completed(&scope.owner),
            (TodoToolListStatus::Cancelled, None) => self.todo_store.list_cancelled(&scope.owner),
            // Tool 可见编号也必须和 `/todo all` 看板一致，否则模型随后按“第 N 个”
            // 调用 complete/restore/delete 时会绑定到用户没有按该顺序看到的条目。
            (TodoToolListStatus::All, None) => self.todo_store.list_all_for_board(&scope.owner),
        }
        .map_err(todo_tool_error)?;
        let due_date_text = due_date.map(|date| date.format("%Y-%m-%d").to_string());
        let query_type = if due_date_text.is_some() && matches!(status, TodoToolListStatus::Pending)
        {
            "due-date"
        } else {
            status.query_type()
        };
        let condition = due_date_text
            .as_deref()
            .unwrap_or_else(|| status.condition());
        scope.remember_internal_query(query_type, condition, &items)?;

        Ok(ToolOutput::json(json!({
            "status": status.as_str(),
            "due_date": due_date_text,
            "items": todo_items_json(&items),
            "count": items.len(),
            "numbering": "visible_number 是本轮工具查询编号，仅在当前 Tool Loop 内有效；用户跨轮次的第 N 条仍以最近实际展示给用户的 /todo 列表为准；未暴露数据库内部 ID。"
        })))
    }
}

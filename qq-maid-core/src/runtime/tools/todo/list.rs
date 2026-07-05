//! `list_todos` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use chrono::NaiveDate;

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
            description: "查询当前私聊用户的待办列表。不会返回数据库内部 ID；visible_number 只供本轮 Tool Loop 内部推理和后续工具调用使用，不会覆盖用户跨轮次真正看到的列表编号。status=pending 查询未完成，completed 查询已完成，cancelled 查询已取消，all 查询全部。查询今天、昨天、本周、上周、本月、最近 N 天等时间范围时，把用户原始中文范围传给 date_range_text；Rust 会按请求时间和时区归一化，模型不要自行换算绝对日期。".to_owned(),
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
                        "description": "旧兼容字段：按单日计划日期筛选。新调用应优先把用户原始时间表达传给 date_range_text，不要让模型自行换算绝对日期。无日期筛选时传 null。"
                    },
                    "date_range_text": {
                        "type": ["string", "null"],
                        "description": "用户原始中文时间范围，例如“今天”“昨天”“前天”“本周”“上周”“下周”“本月”“上月”“最近 7 天”“这几天”“这两天”“明后天”。无范围筛选时传 null。"
                    }
                },
                "required": ["status", "due_date", "date_range_text"],
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
        let date_range = optional_text(&arguments, "date_range_text")?
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(|value| {
                let ctx = crate::util::time_context::request_time_context();
                crate::util::time_context::parse_date_range_expression(&value, &ctx)
                    .map(|range| (range.start, range.end, range.raw))
                    .ok_or_else(|| {
                        bad_tool_arguments(
                            "date_range_text must be one of 今天/昨天/前天/本周/上周/下周/本月/上月/最近N天/这几天/这两天/明后天",
                        )
                    })
            })
            .transpose()?;
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
        if date_range.is_some() && due_date.is_some() {
            return Err(bad_tool_arguments(
                "date_range_text and due_date are mutually exclusive",
            ));
        }
        let range_bounds = date_range
            .as_ref()
            .map(|(start, end, _)| (*start, *end))
            .or_else(|| due_date.map(|date| (date, date)));
        let items = match (status, range_bounds) {
            (TodoToolListStatus::Pending, Some((start, end))) if start == end => {
                self.todo_store.list_by_due_date(
                    &scope.owner,
                    crate::runtime::todo::TodoStatus::Pending,
                    start,
                )
            }
            (TodoToolListStatus::Completed, Some((start, end))) if start == end => {
                self.todo_store.list_by_due_date(
                    &scope.owner,
                    crate::runtime::todo::TodoStatus::Completed,
                    start,
                )
            }
            (TodoToolListStatus::Cancelled, Some((start, end))) if start == end => {
                self.todo_store.list_by_due_date(
                    &scope.owner,
                    crate::runtime::todo::TodoStatus::Cancelled,
                    start,
                )
            }
            (TodoToolListStatus::All, Some((start, end))) if start == end => self
                .todo_store
                .list_all_by_due_date_for_board(&scope.owner, start),
            (TodoToolListStatus::Pending, Some((start, end))) => {
                self.todo_store.list_by_due_date_range(
                    &scope.owner,
                    crate::runtime::todo::TodoStatus::Pending,
                    start,
                    end,
                )
            }
            (TodoToolListStatus::Completed, Some((start, end))) => {
                self.todo_store.list_by_due_date_range(
                    &scope.owner,
                    crate::runtime::todo::TodoStatus::Completed,
                    start,
                    end,
                )
            }
            (TodoToolListStatus::Cancelled, Some((start, end))) => {
                self.todo_store.list_by_due_date_range(
                    &scope.owner,
                    crate::runtime::todo::TodoStatus::Cancelled,
                    start,
                    end,
                )
            }
            (TodoToolListStatus::All, Some((start, end))) => self
                .todo_store
                .list_all_by_due_date_range_for_board(&scope.owner, start, end),
            (TodoToolListStatus::Pending, None) => self.todo_store.list_pending(&scope.owner),
            (TodoToolListStatus::Completed, None) => self.todo_store.list_completed(&scope.owner),
            (TodoToolListStatus::Cancelled, None) => self.todo_store.list_cancelled(&scope.owner),
            // Tool 可见编号也必须和 `/todo all` 看板一致，否则模型随后按“第 N 个”
            // 调用 complete/restore/delete 时会绑定到用户没有按该顺序看到的条目。
            (TodoToolListStatus::All, None) => self.todo_store.list_all_for_board(&scope.owner),
        }
        .map_err(todo_tool_error)?;
        let due_date_text = range_bounds
            .and_then(|(start, end)| (start == end).then(|| start.format("%Y-%m-%d").to_string()));
        let due_start = range_bounds.map(|(start, _)| format_date(start));
        let due_end = range_bounds.map(|(_, end)| format_date(end));
        let date_range_label = date_range.as_ref().map(|(_, _, raw)| raw.clone());
        let query_type = if range_bounds.is_some() && matches!(status, TodoToolListStatus::Pending)
        {
            "due-date"
        } else {
            status.query_type()
        };
        let condition = date_range_label
            .as_deref()
            .or(due_date_text.as_deref())
            .unwrap_or_else(|| status.condition());
        scope.remember_internal_query(query_type, condition, &items)?;

        Ok(ToolOutput::json(json!({
            "status": status.as_str(),
            "due_date": due_date_text,
            "due_start": due_start,
            "due_end": due_end,
            "date_range_text": date_range_label,
            "items": todo_items_json(&items),
            "count": items.len(),
            "numbering": "visible_number 是本轮工具查询编号，仅在当前 Tool Loop 内有效；用户跨轮次的第 N 条仍以最近实际展示给用户的 /todo 列表为准；未暴露数据库内部 ID。"
        })))
    }
}

fn format_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

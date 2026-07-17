//! `list_todos` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolEffect, ToolMetadata, ToolOutput};

use chrono::NaiveDate;

use crate::error::LlmError;
use crate::runtime::tools::todo::{
    TodoQuery, TodoQueryStatus, TodoQueryTimeFilter, resolve_todo_list_date_filter,
};

use super::common::{
    LIST_TODOS_TOOL_NAME, bad_tool_arguments, optional_text, todo_status_argument, todo_tool_error,
};
use super::json::todo_items_json;
use super::scope::TodoToolScope;

pub struct ListTodoTool {
    todo_store: crate::runtime::tools::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
}

impl ListTodoTool {
    pub fn new(
        todo_store: crate::runtime::tools::todo::TodoStore,
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
            description: "查询当前私聊用户的待办列表。不会返回数据库内部 ID；visible_number 只供本轮 Tool Loop 内部推理和后续工具调用使用，不会覆盖用户跨轮次真正看到的列表编号。status=pending 查询未完成，completed 查询已完成，all 查询全部。查询今天、昨天、本周、上周、本月、最近 N 天等时间范围时，把用户原始中文范围传给 date_range_text；Rust 会按请求时间和时区归一化，模型不要自行换算绝对日期。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "status": {
                        "type": "string",
                        "enum": ["pending", "completed", "all"],
                        "description": "要查询的待办状态"
                    },
                    "due_date": {
                        "type": ["string", "null"],
                        "description": "旧兼容字段：按单日计划日期筛选。新调用应优先把用户原始时间表达传给 date_range_text，不要让模型自行换算绝对日期。无日期筛选时传 null。"
                    },
                    "date_range_text": {
                        "type": ["string", "null"],
                        "description": "用户原始中文时间范围，例如“今天”“昨天”“前天”“本周”“上周”“下周”“本月”“上月”“最近 7 天”“这几天”“这两天”“明后天”。无范围筛选时传 null。"
                    },
                    "time_filter": {
                        "type": ["string", "null"],
                        "enum": ["overdue", "no_due_date", null],
                        "description": "特殊时间条件：overdue 查询已逾期且未完成，no_due_date 查询无截止时间；与 due_date/date_range_text 互斥。"
                    },
                    "keyword": {
                        "type": ["string", "null"],
                        "description": "在待办标题、详情和原文中模糊搜索；多个空格分隔词使用 AND 匹配。项目名称当前也按关键词搜索。"
                    }
                },
                "required": ["status", "due_date", "date_range_text", "time_filter", "keyword"],
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
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
                let ctx = qq_maid_common::time_context::request_time_context();
                qq_maid_common::time_context::parse_date_range_expression(&value, &ctx)
                    .map(|range| (range.start, range.end, range.raw))
                    .ok_or_else(|| {
                        bad_tool_arguments(
                            "date_range_text must be one of 今天/明天/昨天/前天/本周/上周/下周/本月/上月/最近N天/这几天/这两天/明后天",
                        )
                    })
            })
            .transpose()?;
        let due_date = optional_text(&arguments, "due_date")?
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(|value| {
                let ctx = qq_maid_common::time_context::request_time_context();
                qq_maid_common::time_context::parse_single_date_expression(&value, &ctx)
                    .map(|date| date.date)
                    .ok_or_else(|| bad_tool_arguments("due_date must be a valid YYYY-MM-DD date"))
            })
            .transpose()?;
        let special_time_filter = optional_text(&arguments, "time_filter")?;
        if special_time_filter.is_some() && (due_date.is_some() || date_range.is_some()) {
            return Err(bad_tool_arguments(
                "time_filter 与 due_date/date_range_text 不能同时传入",
            ));
        }
        let date_filter = resolve_todo_list_date_filter(
            status.storage_status(),
            due_date,
            date_range.as_ref().map(|(start, end, _)| (*start, *end)),
        )
        .map_err(todo_tool_error)?;
        let ctx = qq_maid_common::time_context::request_time_context();
        let time = match special_time_filter.as_deref() {
            Some("overdue") => Some(TodoQueryTimeFilter::Overdue {
                now: qq_maid_common::time_context::parse_local_datetime_for_comparison(
                    ctx.current_time(),
                )
                .expect("request time context must contain a valid local datetime"),
            }),
            Some("no_due_date") => Some(TodoQueryTimeFilter::NoDueDate),
            Some(_) => {
                return Err(bad_tool_arguments(
                    "time_filter must be overdue/no_due_date/null",
                ));
            }
            None => date_filter.map(|filter| TodoQueryTimeFilter::DateRange {
                start: filter.start,
                end: filter.end,
                field: filter.field,
            }),
        };
        let query = TodoQuery {
            status: match status {
                TodoToolListStatus::Pending => TodoQueryStatus::Pending,
                TodoToolListStatus::Completed => TodoQueryStatus::Completed,
                TodoToolListStatus::All => TodoQueryStatus::All,
            },
            time,
            keyword: optional_text(&arguments, "keyword")?,
            ..TodoQuery::default()
        };
        let page = self
            .todo_store
            .query_todos(&scope.owner, &query)
            .map_err(todo_tool_error)?;
        let due_date_text = date_filter.and_then(|filter| {
            (filter.start == filter.end).then(|| filter.start.format("%Y-%m-%d").to_string())
        });
        let due_start = date_filter.map(|filter| format_date(filter.start));
        let due_end = date_filter.map(|filter| format_date(filter.end));
        let date_range_field = date_filter.map(|filter| filter.field.as_str());
        let date_range_label = date_range.as_ref().map(|(_, _, raw)| raw.clone());
        let mut conditions = Vec::new();
        if let Some(value) = date_range_label.as_deref().or(due_date_text.as_deref()) {
            conditions.push(value.to_owned());
        }
        if let Some(value) = special_time_filter.as_deref() {
            conditions.push(match value {
                "overdue" => "逾期".to_owned(),
                "no_due_date" => "无截止时间".to_owned(),
                _ => value.to_owned(),
            });
        }
        if let Some(value) = query.keyword.as_deref() {
            conditions.push(format!("关键词“{value}”"));
        }
        let condition = if conditions.is_empty() {
            status.condition().to_owned()
        } else {
            conditions.join("、")
        };
        scope.remember_internal_query("list-query", &condition, &page.items)?;

        Ok(ToolOutput::json(json!({
            "status": status.as_str(),
            "due_date": due_date_text,
            "due_start": due_start,
            "due_end": due_end,
            "date_range_start": due_start,
            "date_range_end": due_end,
            "date_range_text": date_range_label,
            "date_range_field": date_range_field,
            "time_filter": special_time_filter,
            "condition": condition,
            "items": todo_items_json(&page.items),
            "count": page.items.len(),
            "total_count": page.total_count,
            "displayed_count": page.items.len(),
            "truncated": page.offset + page.items.len() < page.total_count,
            "keyword": query.keyword,
            "numbering": "visible_number 是本轮工具查询编号，仅在当前 Tool Loop 内有效；用户跨轮次的第 N 条仍以最近实际展示给用户的 /todo 列表为准；未暴露数据库内部 ID。"
        })))
    }
}

fn format_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

impl super::common::TodoToolListStatus {
    fn storage_status(self) -> Option<crate::runtime::tools::todo::TodoStatus> {
        match self {
            Self::Pending => Some(crate::runtime::tools::todo::TodoStatus::Pending),
            Self::Completed => Some(crate::runtime::tools::todo::TodoStatus::Completed),
            Self::All => None,
        }
    }
}

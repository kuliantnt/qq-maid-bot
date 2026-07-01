//! `create_todo` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::todo::{TodoItemDraft, TodoTimePrecision, enrich_draft_time_from_text},
    util::time_context::request_time_context,
};

use super::common::{
    CREATE_TODO_TOOL_NAME, optional_text, optional_time_precision, todo_tool_error,
};
use super::json::todo_plain_item_json;
use super::scope::TodoToolScope;

pub struct CreateTodoTool {
    todo_store: crate::runtime::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
}

impl CreateTodoTool {
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
impl Tool for CreateTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: CREATE_TODO_TOOL_NAME.to_owned(),
            description: "为当前私聊用户直接创建待办。成功后立即写入数据库，并记录为最近操作对象；新增不需要二次确认。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "用户原始待办内容，例如“今晚检查机器人日志”"
                    },
                    "title": {
                        "type": ["string", "null"],
                        "description": "模型整理出的待办标题；不确定时传 null，系统使用 content"
                    },
                    "detail": {
                        "type": ["string", "null"],
                        "description": "补充详情；没有则传 null"
                    },
                    "due_date": {
                        "type": ["string", "null"],
                        "description": "YYYY-MM-DD 截止日期；没有则传 null"
                    },
                    "due_at": {
                        "type": ["string", "null"],
                        "description": "YYYY-MM-DD HH:MM:SS 或 RFC3339 截止时间；没有则传 null"
                    },
                    "time_precision": {
                        "type": ["string", "null"],
                        "enum": ["none", "date", "date_time", "inferred", null],
                        "description": "时间精度；不确定时传 null"
                    }
                },
                "required": ["content", "title", "detail", "due_date", "due_at", "time_precision"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, LlmError> {
        use super::common::required_non_empty_text;

        let mut scope = TodoToolScope::load(&self.session_store, &context, None)?;
        if let Some(output) = scope.take_dedup_output(&context, &arguments)? {
            return Ok(output);
        }
        let content = required_non_empty_text(&arguments, "content")?;
        let title = optional_text(&arguments, "title")?.unwrap_or_else(|| content.clone());
        let detail = optional_text(&arguments, "detail")?;
        let due_date = optional_text(&arguments, "due_date")?;
        let due_at = optional_text(&arguments, "due_at")?;
        let time_precision: TodoTimePrecision =
            optional_time_precision(&arguments, "time_precision")?;
        let mut draft = TodoItemDraft {
            title,
            detail,
            raw_text: Some(content.clone()),
            due_date,
            due_at,
            time_precision,
        };
        // Tool 创建仍复用本地时间推断；模型未传结构化时间时，保持普通待办创建的保守体验。
        enrich_draft_time_from_text(&mut draft, &content, &request_time_context());

        scope.ensure_no_pending()?;
        let created = crate::runtime::todo::ops::create_one(
            &self.todo_store,
            &mut scope.session,
            &scope.owner,
            draft,
        )
        .map_err(todo_tool_error)?;
        scope.clear_clarification_if_scoped();
        scope.save()?;

        let output = ToolOutput::json(json!({
            "ok": true,
            "created": todo_plain_item_json(&created),
            "message": "待办已新增并写入数据库；后续“刚才那个/刚刚那条”可用 reference=\"last\" 指向这条待办。",
        }));
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

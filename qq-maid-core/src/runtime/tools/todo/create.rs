//! `create_todo` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::{
        pending::PendingOperation,
        session::now_iso_cn,
        todo::{TodoItemDraft, TodoTimePrecision, enrich_draft_time_from_text},
    },
    util::time_context::request_time_context,
};

use super::common::{CREATE_TODO_TOOL_NAME, optional_text, optional_time_precision};
use super::json::todo_draft_json;
use super::scope::TodoToolScope;

pub struct CreateTodoTool {
    session_store: crate::runtime::session::SessionStore,
}

impl CreateTodoTool {
    pub fn new(session_store: crate::runtime::session::SessionStore) -> Self {
        Self { session_store }
    }
}

#[async_trait]
impl Tool for CreateTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: CREATE_TODO_TOOL_NAME.to_owned(),
            description: "为当前私聊用户创建待办草稿。该工具只会生成待确认 pending，不会直接写入；用户确认后才保存。".to_owned(),
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

        let mut scope = TodoToolScope::load(&self.session_store, &context)?;
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
        // Tool 创建仍复用本地时间推断；模型未传结构化时间时，保持 `/todo add` 的保守体验。
        enrich_draft_time_from_text(&mut draft, &content, &request_time_context());

        scope.ensure_no_pending()?;
        scope.session.last_todo_query = None;
        scope.session.pending_operation = Some(PendingOperation::TodoAdd {
            initiator_user_id: scope.owner.user_id.clone(),
            owner_key: scope.owner.key.clone(),
            draft: draft.clone(),
            allow_revision: true,
            created_at: now_iso_cn(),
        });
        scope.save()?;

        let output = ToolOutput::json(json!({
            "requires_confirmation": true,
            "pending_action": "create",
            "message": "已生成待确认待办草稿；必须等待用户确认后才会写入。",
            "draft": todo_draft_json(&draft),
        }));
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

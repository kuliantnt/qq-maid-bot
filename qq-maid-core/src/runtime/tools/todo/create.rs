//! `create_todo` Tool。

use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::todo::{TodoItemDraft, TodoTimePrecision, enrich_draft_time_from_text},
    util::time_context::request_time_context,
};

use super::common::{
    CREATE_TODO_TOOL_NAME, optional_text, optional_time_precision, required_non_empty_text,
    todo_tool_error,
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
            description: "为当前私聊用户直接创建一个或多个待办。成功后立即写入数据库；新增不需要二次确认。优先使用 items 批量表达同一轮拆解出的多个待办，旧单项字段仍兼容。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": ["array", "null"],
                        "description": "同一用户意图下要创建的待办列表；创建多项时必须使用此字段。",
                        "minItems": 1,
                        "maxItems": 20,
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "title": {"type": ["string", "null"]},
                                "detail": {"type": ["string", "null"]},
                                "due_date": {"type": ["string", "null"]},
                                "due_at": {"type": ["string", "null"]},
                                "time_precision": {
                                    "type": ["string", "null"],
                                    "enum": ["none", "date", "date_time", "inferred", null]
                                }
                            },
                            "required": ["content", "title", "detail", "due_date", "due_at", "time_precision"],
                            "additionalProperties": false
                        }
                    },
                    "content": {
                        "type": ["string", "null"],
                        "description": "旧单项兼容字段：用户原始待办内容，例如“今晚检查机器人日志”。items 非空时传 null。"
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
                "required": ["items", "content", "title", "detail", "due_date", "due_at", "time_precision"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope = TodoToolScope::load(&self.session_store, &context, None)?;
        if let Some(output) = scope.take_dedup_output(&context, &arguments)? {
            return Ok(output);
        }
        let drafts = create_drafts_from_arguments(&arguments)?;

        scope.ensure_no_pending()?;
        let created = crate::runtime::todo::ops::create_many(
            &self.todo_store,
            &mut scope.session,
            &scope.owner,
            drafts,
        )
        .map_err(todo_tool_error)?;
        scope.clear_clarification_if_scoped();
        scope.save()?;

        let output = ToolOutput::json(json!({
            "ok": true,
            "created": created.first().map(todo_plain_item_json),
            "created_items": created.iter().map(todo_plain_item_json).collect::<Vec<_>>(),
            "message": if created.len() == 1 {
                "待办已新增并写入数据库；后续“刚才那个/刚刚那条”可用 reference=\"last\" 指向这条待办。"
            } else {
                "多条待办已作为同一批创建并写入数据库；批量创建后不会把“刚才那个”绑定到任意单条。"
            },
        }));
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

fn create_drafts_from_arguments(arguments: &Value) -> Result<Vec<TodoItemDraft>, LlmError> {
    if let Some(items) = arguments.get("items").and_then(Value::as_array)
        && !items.is_empty()
    {
        return items.iter().map(create_draft_from_value).collect();
    }
    Ok(vec![create_draft_from_value(arguments)?])
}

fn create_draft_from_value(value: &Value) -> Result<TodoItemDraft, LlmError> {
    let content = required_non_empty_text(value, "content")?;
    let title = optional_text(value, "title")?.unwrap_or_else(|| content.clone());
    let detail = optional_text(value, "detail")?;
    let due_date = optional_text(value, "due_date")?;
    let due_at = optional_text(value, "due_at")?;
    let time_precision: TodoTimePrecision = optional_time_precision(value, "time_precision")?;
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
    Ok(draft)
}

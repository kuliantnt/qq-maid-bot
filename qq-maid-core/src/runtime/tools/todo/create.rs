//! `create_todo` Tool。

use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::todo::{
        TodoItemDraft, TodoRecurrenceUnit, TodoTimePrecision, enrich_draft_time_from_text,
        reminder_task::{sync_reminder_task, validate_draft_reminder},
    },
    storage::notification::NotificationOutboxStore,
    util::time_context::request_time_context,
};

use super::common::{
    CREATE_TODO_TOOL_NAME, TODO_TOOL_MAX_BATCH_CREATE_ITEMS, bad_tool_arguments,
    has_explicit_no_recurrence, optional_positive_u32, optional_recurrence_kind,
    optional_recurrence_unit, optional_text, optional_time_precision, required_non_empty_text,
    todo_tool_error,
};
use super::json::todo_plain_item_json;
use super::scope::TodoToolScope;

pub struct CreateTodoTool {
    todo_store: crate::runtime::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
    notification_store: NotificationOutboxStore,
}

impl CreateTodoTool {
    pub fn new(
        todo_store: crate::runtime::todo::TodoStore,
        session_store: crate::runtime::session::SessionStore,
        notification_store: NotificationOutboxStore,
    ) -> Self {
        Self {
            todo_store,
            session_store,
            notification_store,
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
                        "maxItems": TODO_TOOL_MAX_BATCH_CREATE_ITEMS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "title": {"type": ["string", "null"]},
                                "detail": {"type": ["string", "null"]},
                                "due_date": {"type": ["string", "null"]},
                                "due_at": {"type": ["string", "null"]},
                                "reminder_at": {
                                    "type": ["string", "null"],
                                    "description": "明确提醒时间，必须是 YYYY-MM-DD HH:MM[:SS] 或 RFC3339；没有单次提醒则传 null。不要用截止时间代替提醒时间。"
                                },
                                "time_precision": {
                                    "type": ["string", "null"],
                                    "enum": ["none", "date", "date_time", "inferred", null]
                                },
                                "recurrence_kind": {
                                    "type": ["string", "null"],
                                    "enum": ["none", "daily", "every_n_days", "weekly", "every_n_weeks", "monthly", "every_n_months", "yearly", "every_n_years", null],
                                    "description": "重复规则；优先配合 recurrence_interval + recurrence_unit 表达 day/week/month/year。未显式解析时传 null，系统会结合用户原文做保守补充。"
                                },
                                "recurrence_interval": {
                                    "type": ["integer", "null"],
                                    "minimum": 1,
                                    "maximum": 1827,
                                    "description": "重复间隔数值；day 最大 1827，week 最大 261，month 最大 60，year 最大 5。无重复则传 null。"
                                },
                                "recurrence_unit": {
                                    "type": ["string", "null"],
                                    "enum": ["day", "week", "month", "year", null],
                                    "description": "重复间隔单位；无重复或不确定时传 null。"
                                },
                                "recurrence_interval_days": {
                                    "type": ["integer", "null"],
                                    "minimum": 1,
                                    "maximum": 1827,
                                    "description": "旧兼容字段：仅用于 day 单位；daily 固定为 1，隔天为 2，每隔 N 天为 N。新调用优先使用 recurrence_interval + recurrence_unit。"
                                }
                            },
                            "required": ["content", "title", "detail", "due_date", "due_at", "reminder_at", "time_precision", "recurrence_kind", "recurrence_interval", "recurrence_unit", "recurrence_interval_days"],
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
                    "reminder_at": {
                        "type": ["string", "null"],
                        "description": "明确单次提醒时间，必须是 YYYY-MM-DD HH:MM[:SS] 或 RFC3339；没有提醒则传 null。"
                    },
                    "time_precision": {
                        "type": ["string", "null"],
                        "enum": ["none", "date", "date_time", "inferred", null],
                        "description": "时间精度；不确定时传 null"
                    },
                    "recurrence_kind": {
                        "type": ["string", "null"],
                        "enum": ["none", "daily", "every_n_days", "weekly", "every_n_weeks", "monthly", "every_n_months", "yearly", "every_n_years", null],
                        "description": "重复规则；优先配合 recurrence_interval + recurrence_unit 表达 day/week/month/year，不重复则传 null。"
                    },
                    "recurrence_interval": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "maximum": 1827,
                        "description": "重复间隔数值；day 最大 1827，week 最大 261，month 最大 60，year 最大 5。无重复则传 null。"
                    },
                    "recurrence_unit": {
                        "type": ["string", "null"],
                        "enum": ["day", "week", "month", "year", null],
                        "description": "重复间隔单位；无重复或不确定时传 null。"
                    },
                    "recurrence_interval_days": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "maximum": 1827,
                        "description": "旧兼容字段：仅用于 day 单位；新调用优先使用 recurrence_interval + recurrence_unit。"
                    }
                },
                "required": ["items", "content", "title", "detail", "due_date", "due_at", "reminder_at", "time_precision", "recurrence_kind", "recurrence_interval", "recurrence_unit", "recurrence_interval_days"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, LlmError> {
        let drafts = create_drafts_from_arguments(&arguments)?;
        for draft in &drafts {
            validate_draft_reminder(draft)
                .map_err(|message| LlmError::new("bad_todo_reminder", message, "todo_tool"))?;
        }
        let mut scope = TodoToolScope::load(&self.session_store, &context, None)?;
        if let Some(output) = scope.take_dedup_output(&context, &arguments)? {
            return Ok(output);
        }

        scope.ensure_no_pending()?;
        let created = crate::runtime::todo::ops::create_many(
            &self.todo_store,
            &mut scope.session,
            &scope.owner,
            drafts,
        )
        .map_err(todo_tool_error)?;
        for item in &created {
            sync_reminder_task(&self.notification_store, &scope.owner, item).map_err(
                |message| LlmError::new("todo_reminder_sync_failed", message, "todo_tool"),
            )?;
        }
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
    match arguments.get("items") {
        Some(Value::Array(items)) => {
            validate_batch_create_item_count(items.len())?;
            return items.iter().map(create_draft_from_value).collect();
        }
        Some(Value::Null) | None => {}
        Some(_) => return Err(bad_tool_arguments("items must be an array or null")),
    }
    Ok(vec![create_draft_from_value(arguments)?])
}

fn validate_batch_create_item_count(count: usize) -> Result<(), LlmError> {
    if count == 0 {
        return Err(bad_tool_arguments("items must contain at least one todo"));
    }
    if count > TODO_TOOL_MAX_BATCH_CREATE_ITEMS {
        return Err(bad_tool_arguments(format!(
            "单次最多创建 {TODO_TOOL_MAX_BATCH_CREATE_ITEMS} 项待办，请减少本次项目数量后重试。"
        )));
    }
    Ok(())
}

fn create_draft_from_value(value: &Value) -> Result<TodoItemDraft, LlmError> {
    let content = required_non_empty_text(value, "content")?;
    let title = optional_text(value, "title")?.unwrap_or_else(|| content.clone());
    let detail = optional_text(value, "detail")?;
    let due_date = optional_text(value, "due_date")?;
    let due_at = optional_text(value, "due_at")?;
    let reminder_at = optional_text(value, "reminder_at")?;
    let time_precision: TodoTimePrecision = optional_time_precision(value, "time_precision")?;
    let recurrence_kind = optional_recurrence_kind(value, "recurrence_kind")?;
    let recurrence_interval = optional_positive_u32(value, "recurrence_interval")?.unwrap_or(0);
    let recurrence_unit =
        optional_recurrence_unit(value, "recurrence_unit")?.unwrap_or(TodoRecurrenceUnit::Day);
    let recurrence_interval_days =
        optional_positive_u32(value, "recurrence_interval_days")?.unwrap_or(0);
    let explicit_no_recurrence = has_explicit_no_recurrence(value, "recurrence_kind");
    if explicit_no_recurrence && (recurrence_interval_days > 0 || recurrence_interval > 0) {
        return Err(bad_tool_arguments(
            "recurrence interval must be null when recurrence_kind is none",
        ));
    }
    let mut draft = TodoItemDraft {
        title,
        detail,
        raw_text: Some(content.clone()),
        due_date,
        due_at,
        reminder_at,
        time_precision,
        recurrence_kind,
        recurrence_interval_days,
        recurrence_interval,
        recurrence_unit,
    };
    if explicit_no_recurrence {
        // 模型显式判断“不重复”时，不再让正文里的“每天”等词触发保守推断。
        draft.mark_explicit_no_recurrence();
    }
    // Tool 创建仍复用本地时间推断；模型未传结构化时间时，保持普通待办创建的保守体验。
    enrich_draft_time_from_text(&mut draft, &content, &request_time_context());
    Ok(draft)
}

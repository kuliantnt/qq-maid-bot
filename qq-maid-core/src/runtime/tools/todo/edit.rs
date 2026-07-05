//! `edit_todo` Tool。

use async_trait::async_trait;
use serde_json::json;

use qq_maid_llm::tool::{
    Tool, ToolCallDependency, ToolContext, ToolMetadata, ToolOutput, ToolPreparation,
};

use crate::{
    error::LlmError,
    runtime::todo::{
        TodoItemDraft, TodoStatus,
        reminder_task::{sync_reminder_task, validate_draft_reminder},
    },
    storage::notification::NotificationOutboxStore,
};

use super::common::{
    EDIT_TODO_TOOL_NAME, TODO_REFERENCE_INVALID_STATE_CODE, TODO_REFERENCE_LAST,
    bad_tool_arguments, required_non_empty_text, single_todo_selection_request, todo_edit_patch,
    todo_tool_error, todo_tool_error_output,
};
use super::json::todo_selected_item_json;
use super::scope::clarification_error_fields;
use super::scope::{SelectionScope, TodoToolScope};
use super::selection::{TodoToolSingleItemResolutionWithDraft, prepared_edit_target};

pub struct EditTodoTool {
    todo_store: crate::runtime::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
    notification_store: NotificationOutboxStore,
    /// 受限 Tool Loop 注入的请求级选择作用域；普通调用为 `None`。
    selection_scope: Option<SelectionScope>,
}

impl EditTodoTool {
    pub fn new(
        todo_store: crate::runtime::todo::TodoStore,
        session_store: crate::runtime::session::SessionStore,
        notification_store: NotificationOutboxStore,
    ) -> Self {
        Self {
            todo_store,
            session_store,
            notification_store,
            selection_scope: None,
        }
    }

    /// 注入受限 Tool Loop 专属的请求级选择作用域，返回新实例。
    pub fn with_selection_scope(mut self, scope: SelectionScope) -> Self {
        self.selection_scope = Some(scope);
        self
    }
}

#[async_trait]
impl Tool for EditTodoTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: EDIT_TODO_TOOL_NAME.to_owned(),
            description: "编辑未完成待办的标题、详情和时间。用户明确说“第 N 个”时只能传 number 并依赖最近一次 list_todos 的 visible_number；用户说“刚才那个 / 它”时传 reference=\"last\"。不会接受数据库内部 ID，也不会修改已完成/已取消待办。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "number": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "要编辑的 visible_number"
                    },
                    "reference": {
                        "type": ["string", "null"],
                        "enum": [TODO_REFERENCE_LAST, null],
                        "description": "当用户说“刚才那个 / 它 / 刚恢复的那个 / 刚创建的那个”时传 \"last\"；与 number 二选一。"
                    },
                    "raw_text": {
                        "type": "string",
                        "description": "用户原始修改内容，例如“改为除了搬家还有宽带要迁移”"
                    },
                    "title": {
                        "type": ["string", "null"],
                        "description": "新的标题；仅在用户明确修改标题时传值"
                    },
                    "detail": {
                        "type": ["string", "null"],
                        "description": "新的详情/内容/备注；仅在用户明确修改详情时传值"
                    },
                    "due_date": {
                        "type": ["string", "null"],
                        "description": "新的 YYYY-MM-DD 截止日期；没有则传 null"
                    },
                    "due_at": {
                        "type": ["string", "null"],
                        "description": "新的 YYYY-MM-DD HH:MM:SS 截止时间；没有则传 null"
                    },
                    "reminder_at": {
                        "type": ["string", "null"],
                        "description": "新的明确单次提醒时间，必须是 YYYY-MM-DD HH:MM[:SS] 或 RFC3339；不修改提醒传 null；清除提醒传空字符串。"
                    },
                    "time_precision": {
                        "type": ["string", "null"],
                        "enum": ["none", "date", "date_time", "inferred", null],
                        "description": "新的时间精度；未明确修改时传 null"
                    },
                    "recurrence_kind": {
                        "type": ["string", "null"],
                        "enum": ["none", "daily", "every_n_days", null],
                        "description": "新的重复规则；不修改传 null；清除重复规则时传 \"none\"。"
                    },
                    "recurrence_interval_days": {
                        "type": ["integer", "null"],
                        "minimum": 1,
                        "description": "新的重复间隔天数；daily=1，隔天=2，每隔 3 天=3；不修改传 null。"
                    }
                },
                "required": ["number", "reference", "raw_text", "title", "detail", "due_date", "due_at", "reminder_at", "time_precision", "recurrence_kind", "recurrence_interval_days"],
                "additionalProperties": false
            }),
        }
    }

    fn prepare(
        &self,
        context: &ToolContext,
        arguments: serde_json::Value,
    ) -> Result<ToolPreparation, LlmError> {
        use super::common::TodoSelectionRequest;
        use super::common::{
            PREBOUND_EDIT_DRAFT_KEY, PREBOUND_ERROR_OUTPUT_KEY, PREBOUND_SINGLE_ID_KEY,
            PREBOUND_SINGLE_LABEL_KEY,
        };
        use super::selection::resolve_prepared_selection;

        let mut scope =
            TodoToolScope::load(&self.session_store, context, self.selection_scope.clone())?;
        let selection = single_todo_selection_request(&arguments)?;
        let patch = todo_edit_patch(&arguments)?;
        let raw_text = required_non_empty_text(&arguments, "raw_text")?;
        let prepared = match selection {
            TodoSelectionRequest::Numbers(_) => {
                let resolved =
                    resolve_prepared_selection(&mut scope, &selection, &self.todo_store)?;
                if let Some(error_output) = resolved.error_output {
                    let mut prepared = arguments.clone();
                    let object = prepared.as_object_mut().ok_or_else(|| {
                        bad_tool_arguments("tool arguments must be a JSON object")
                    })?;
                    object.insert(PREBOUND_ERROR_OUTPUT_KEY.to_owned(), error_output);
                    object.insert("raw_text".to_owned(), json!(raw_text));
                    return Ok(ToolPreparation::ready(prepared));
                }
                let id = resolved
                    .matched
                    .first()
                    .map(|item| item.id.clone())
                    .ok_or_else(|| bad_tool_arguments("number did not match any visible todo"))?;
                let label = serde_json::to_value(
                    resolved
                        .labels
                        .first()
                        .cloned()
                        .unwrap_or(super::common::TodoSelectionLabel::Number(1)),
                )
                .map_err(|err| {
                    LlmError::new(
                        "bad_tool_arguments",
                        format!("failed to encode selection label: {err}"),
                        "tool",
                    )
                })?;
                json!({
                    PREBOUND_SINGLE_ID_KEY: id,
                    PREBOUND_SINGLE_LABEL_KEY: label,
                    PREBOUND_EDIT_DRAFT_KEY: serde_json::to_value(patch).map_err(|err| LlmError::new("bad_tool_arguments", format!("failed to encode edit patch: {err}"), "tool"))?,
                    "raw_text": raw_text,
                })
            }
            TodoSelectionRequest::Reference(_) => {
                let mut prepared = arguments.clone();
                if let Some(object) = prepared.as_object_mut() {
                    object.insert("raw_text".to_owned(), json!(raw_text));
                }
                prepared
            }
        };
        let dependency = match selection {
            TodoSelectionRequest::Reference(_) => ToolCallDependency::PreviousCallSuccess,
            TodoSelectionRequest::Numbers(_) => ToolCallDependency::None,
        };
        Ok(ToolPreparation::ready(prepared).with_dependency(dependency))
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: serde_json::Value,
    ) -> Result<ToolOutput, LlmError> {
        let mut scope =
            TodoToolScope::load(&self.session_store, &context, self.selection_scope.clone())?;
        if let Some(output) = scope.take_dedup_output(&context, &arguments)? {
            return Ok(output);
        }
        let (item, label, patch, raw_text) =
            match prepared_edit_target(&mut scope, &self.todo_store, &arguments)? {
                TodoToolSingleItemResolutionWithDraft::Item {
                    item,
                    label,
                    patch,
                    raw_text,
                } => (item, label, patch, raw_text),
                TodoToolSingleItemResolutionWithDraft::Output(output) => {
                    let (error_code, message) = clarification_error_fields(&output);
                    return scope.save_clarification(
                        &self.todo_store,
                        EDIT_TODO_TOOL_NAME,
                        &arguments,
                        false,
                        error_code,
                        message,
                    );
                }
            };
        if item.status != TodoStatus::Pending {
            return Ok(todo_tool_error_output(
                TODO_REFERENCE_INVALID_STATE_CODE,
                "edit_todo only accepts pending todos",
            ));
        }
        // 补丁应用逻辑已统一到 `runtime::todo::edit_patch::apply_to_draft`，
        // 与指令侧 `/todo edit` 保持同一套规则。
        let draft = crate::runtime::todo::edit_patch::apply_to_draft(
            TodoItemDraft::from_item(&item, raw_text.clone()),
            &patch,
            &raw_text,
        );
        if patch.reminder_at.is_some() {
            validate_draft_reminder(&draft)
                .map_err(|message| LlmError::new("bad_todo_reminder", message, "todo_tool"))?;
        }
        let updated = self
            .todo_store
            .edit(&scope.owner, &item.id, draft)
            .map_err(todo_tool_error)?;
        sync_reminder_task(&self.notification_store, &scope.owner, &updated)
            .map_err(|message| LlmError::new("todo_reminder_sync_failed", message, "todo_tool"))?;
        scope.session.last_todo_query = None;
        scope
            .session
            .remember_last_todo_action(&scope.owner.key, &updated, "edited");
        scope.clear_clarification_if_scoped();
        let output = ToolOutput::json(json!({
            "ok": true,
            "updated": todo_selected_item_json(label, &updated),
            "message": "待办已更新；执行前已把用户可见编号绑定到稳定内部 ID。"
        }));
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

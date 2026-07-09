//! `manage_recurring_reminder` Tool。
//!
//! 只处理重复提醒的周期控制：跳过当前周期、关闭后续重复提醒，以及明确告知
//! 独立暂停状态暂不支持。删除/取消待办仍交给 `delete_todos` 的二次确认链路。

use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{error::LlmError, storage::notification::NotificationOutboxStore};

use super::{
    cancel_reminder_task,
    common::{
        MANAGE_RECURRING_REMINDER_TOOL_NAME, TODO_SELECTION_NOT_FOUND_CODE, bad_tool_arguments,
        number_list_or_reference_schema, todo_tool_error,
    },
    json::todo_selected_items_json,
    ops::{disable_recurrence_many, skip_recurring_current_period},
    scope::{SelectionScope, TodoToolScope, clarification_error_fields},
    selection::{
        missing_numbers_json, missing_selection_labels_excluding_items,
        prepare_selection_arguments, prepared_selection_ids, resolved_selection_from_arguments,
        selected_items_for_result,
    },
    sync_reminder_task,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecurringReminderAction {
    SkipNext,
    DisableRecurrence,
    Pause,
}

impl RecurringReminderAction {
    fn parse(arguments: &Value) -> Result<Self, LlmError> {
        match arguments.get("action").and_then(Value::as_str) {
            Some("skip_next") => Ok(Self::SkipNext),
            Some("disable_recurrence") => Ok(Self::DisableRecurrence),
            Some("pause") => Ok(Self::Pause),
            _ => Err(bad_tool_arguments(
                "action must be skip_next/disable_recurrence/pause",
            )),
        }
    }
}

pub struct ManageRecurringReminderTool {
    todo_store: crate::runtime::tools::todo::TodoStore,
    session_store: crate::runtime::session::SessionStore,
    notification_store: NotificationOutboxStore,
    /// 受限 Tool Loop 注入的请求级选择作用域；普通调用为 `None`。
    selection_scope: Option<SelectionScope>,
}

impl ManageRecurringReminderTool {
    pub fn new(
        todo_store: crate::runtime::tools::todo::TodoStore,
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
    pub(crate) fn with_selection_scope(mut self, scope: SelectionScope) -> Self {
        self.selection_scope = Some(scope);
        self
    }
}

#[async_trait]
impl Tool for ManageRecurringReminderTool {
    fn metadata(&self) -> ToolMetadata {
        let mut parameters =
            number_list_or_reference_schema("要管理的重复提醒 visible_number 列表");
        if let Some(properties) = parameters
            .get_mut("properties")
            .and_then(Value::as_object_mut)
        {
            properties.insert(
                "action".to_owned(),
                json!({
                    "type": "string",
                    "enum": ["skip_next", "disable_recurrence", "pause"],
                    "description": "skip_next=跳过本次/今天别提醒了，仅推进重复项到下一周期；disable_recurrence=以后别提醒了/关掉重复提醒，关闭后续重复并取消提醒；pause=用户要求暂停但未给出新的提醒时间，当前只返回不支持。取消或删除待办/提醒必须使用 delete_todos，不要调用本工具。"
                }),
            );
        }
        if let Some(required) = parameters.get_mut("required").and_then(Value::as_array_mut) {
            required.push(json!("action"));
        }
        ToolMetadata {
            name: MANAGE_RECURRING_REMINDER_TOOL_NAME.to_owned(),
            description: "管理重复提醒的当前周期或后续重复。用户说“今天已完成/完成这个”时应使用 complete_todos；说“跳过这次/今天别提醒了”时使用 action=skip_next；说“以后别提醒了/关掉重复提醒”时使用 action=disable_recurrence；说“暂停提醒”但没有明确新时间时使用 action=pause 并返回不支持。用户说“取消这个提醒/待办/不做了/删除”时必须使用 delete_todos 进入删除确认。不会接受数据库内部 ID。".to_owned(),
            parameters,
        }
    }

    fn prepare(
        &self,
        context: &ToolContext,
        arguments: Value,
    ) -> Result<qq_maid_llm::tool::ToolPreparation, LlmError> {
        RecurringReminderAction::parse(&arguments)?;
        prepare_selection_arguments(
            &self.session_store,
            &self.todo_store,
            context,
            arguments,
            true,
            self.selection_scope.clone(),
        )
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let action = RecurringReminderAction::parse(&arguments)?;
        let mut scope =
            TodoToolScope::load(&self.session_store, &context, self.selection_scope.clone())?;
        if let Some(output) = scope.take_dedup_output(&context, &arguments)? {
            return Ok(output);
        }
        if action == RecurringReminderAction::Pause {
            return Ok(ToolOutput::json(json!({
                "ok": false,
                "requires_clarification": true,
                "error_code": "todo_pause_unsupported",
                "question": "当前不保存独立暂停状态。请说清楚要改到哪个提醒时间，或说“跳过这次”“以后别提醒了”。",
                "message": "pause is unsupported without a concrete replacement reminder time"
            })));
        }

        let resolved =
            resolved_selection_from_arguments(&mut scope, &self.todo_store, &arguments, true)?;
        if let Some(output) = resolved.error_output.as_ref() {
            let (error_code, message) = clarification_error_fields(output);
            return scope.save_clarification(
                &self.todo_store,
                MANAGE_RECURRING_REMINDER_TOOL_NAME,
                &arguments,
                true,
                error_code,
                message,
            );
        }
        let ids = prepared_selection_ids(&resolved);
        if ids.is_empty() {
            return scope.save_clarification(
                &self.todo_store,
                MANAGE_RECURRING_REMINDER_TOOL_NAME,
                &arguments,
                true,
                TODO_SELECTION_NOT_FOUND_CODE,
                "no visible numbers matched",
            );
        }

        let output = match action {
            RecurringReminderAction::SkipNext => {
                let outcome = skip_recurring_current_period(
                    &self.todo_store,
                    &mut scope.session,
                    &scope.owner,
                    &ids,
                )
                .map_err(todo_tool_error)?;
                for item in &outcome.advanced {
                    sync_reminder_task(&self.notification_store, &scope.owner, item).map_err(
                        |message| LlmError::new("todo_reminder_sync_failed", message, "todo_tool"),
                    )?;
                }
                let advanced = selected_items_for_result(&resolved, &outcome.advanced);
                let missing = missing_selection_labels_excluding_items(&resolved, &advanced);
                if !advanced.is_empty() {
                    scope.clear_clarification_if_scoped();
                    scope.save()?;
                }
                ToolOutput::json(json!({
                    "ok": true,
                    "advanced": todo_selected_items_json(&advanced),
                    "missing_numbers": missing_numbers_json(&missing),
                    "message": "已跳过重复提醒的当前周期；系统只推进到下一次，不删除待办，也不关闭重复规则。"
                }))
            }
            RecurringReminderAction::DisableRecurrence => {
                let disabled = disable_recurrence_many(
                    &self.todo_store,
                    &mut scope.session,
                    &scope.owner,
                    &ids,
                )
                .map_err(todo_tool_error)?;
                for item in &disabled {
                    cancel_reminder_task(&self.notification_store, item).map_err(|message| {
                        LlmError::new("todo_reminder_cancel_failed", message, "todo_tool")
                    })?;
                }
                let disabled = selected_items_for_result(&resolved, &disabled);
                let missing = missing_selection_labels_excluding_items(&resolved, &disabled);
                if !disabled.is_empty() {
                    scope.clear_clarification_if_scoped();
                    scope.save()?;
                }
                ToolOutput::json(json!({
                    "ok": true,
                    "disabled": todo_selected_items_json(&disabled),
                    "missing_numbers": missing_numbers_json(&missing),
                    "message": "已关闭后续重复提醒并取消未发送提醒任务；待办本身保留为未完成。missing_numbers 表示目标不存在、不是未完成重复提醒或状态已变化。"
                }))
            }
            RecurringReminderAction::Pause => unreachable!("pause is returned before mutation"),
        };
        scope.remember_dedup_output(&context, &arguments, &output)?;
        Ok(output)
    }
}

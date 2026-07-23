//! Todo 领域模块。
//!
//! 这里是 Todo 的唯一业务边界：命令、Tool、业务编排、展示、查询模型和 SQLite
//! Repository 都在本模块内协作。外层只依赖下面明确列出的领域入口，不直接依赖
//! `storage` 的子模块，避免数据库字段和 SQL 细节向 Runtime 泄漏。
//!
//! 模块拆分（保持公共导出与历史不变）：
//! - `common`：常量、选择/引用类型与参数解析 helper、错误转换。
//! - `scope`：`TodoToolScope` 与可见编号 / 最近对象解析。
//! - `selection`：prepare/execute 共用的预解析与结果映射helper。
//! - `json`：面向模型的 JSON 序列化与状态文案。
//! - `recurrence`/`reminder`/`reminder_worker`/`template`：Todo 领域的重复规则意图、提醒 outbox、每日提醒调度和展示模板。
//! - `list`/`create`/`complete`/`edit`/`restore`/`delete`：各 Tool 实现。

pub(crate) mod agent_turn;
mod common;
pub(crate) mod edit_patch;
pub(crate) mod flow;
pub(crate) mod format;
mod freshness;
pub(crate) mod group_admin;
pub(crate) mod interaction_state;
mod json;
pub(crate) mod ops;
pub(crate) mod pending;
pub(crate) mod query_filter;
mod query_snapshot;
pub(crate) mod receipt;
pub(crate) mod recurrence;
pub(crate) mod reminder;
pub(crate) mod reminder_worker;
pub(crate) mod route;
mod scope;
mod selection;
pub(crate) mod status;
pub(crate) mod storage;
pub(crate) mod success_guard;
pub(crate) mod template;
pub(crate) mod tool_policy;
pub(crate) mod visible_entity;

mod complete;
mod create;
mod delete;
mod edit;
mod get;
mod list;
mod merge;
mod recurring;
mod restore;

use std::sync::Arc;

use qq_maid_llm::tool::DynTool;

use crate::{runtime::session::SessionStore, storage::notification::NotificationOutboxStore};

pub(crate) use complete::CompleteTodoTool;
pub(crate) use create::CreateTodoTool;
pub(crate) use delete::DeleteTodoTool;
pub(crate) use edit::EditTodoTool;
pub use edit_patch::TodoEditPatch;
pub(crate) use freshness::valid_last_visible_todo_query;
pub(crate) use get::GetTodoTool;
pub(crate) use list::ListTodoTool;
pub(crate) use merge::MergeTodoTool;
pub(crate) use pending::{
    ClarificationCandidate, PendingTodoClarification, TODO_PENDING_DOMAIN, TodoPendingPayload,
    todo_lexicon,
};
pub(crate) use query_snapshot::{remember_todo_query_snapshot, replay_todo_query, todo_query_type};
pub(crate) use recurring::ManageRecurringReminderTool;
pub(crate) use reminder::{
    TodoReminderSentHook, cancel_reminder_task, cancel_reminder_task_by_id, sync_reminder_task,
    validate_draft_reminder,
};
pub use reminder_worker::{TodoReminderScheduler, TodoReminderSchedulerConfig};
pub(crate) use restore::RestoreTodoTool;

// 这是 Todo 对外的领域 API。不要改回 `storage::*`：Repository 新增内部类型时，
// 必须明确决定它是否属于业务边界，避免 SQL/行映射类型意外被 Runtime 使用。
pub(crate) use storage::{
    TODO_DAILY_REMINDER_PREF_SCHEMA_V5, TODO_QUERY_DEFAULT_LIMIT, TODO_QUERY_MAX_LIMIT,
    TODO_RECURRENCE_RULE_SCHEMA_V4, TODO_RECURRENCE_SCHEMA_V3, TODO_REMINDER_SCHEMA_V2,
    TODO_SCHEMA_V1, TodoBulkDeleteOutcome, TodoBulkRestoreOutcome, TodoCompleteProgressOutcome,
    TodoEditRecurrencePatch, TodoError, TodoItem, TodoItemDraft, TodoListDateField,
    TodoListDateFilter, TodoOwner, TodoQuery, TodoQueryPage, TodoQueryStatus, TodoQueryTimeFilter,
    TodoRecurrenceKind, TodoRecurrenceRule, TodoRecurrenceUnit, TodoReminderOwnerQueryResult,
    TodoReminderOwnerSkipReason, TodoStatus, TodoStore, TodoTimePrecision,
    apply_recurrence_patch_to_draft, display_todo_time, enrich_draft_time_from_text,
    preview_next_reminder_at, recurrence_kind_for_rule, recurrence_label,
    recurrence_rule_from_interval_unit, resolve_todo_list_date_filter,
};
pub(crate) use template::{ReminderFieldMode, TodoCardOptions, TodoRenderItem, format_todo_cards};
pub(crate) use visible_entity::{
    TodoScopedToolInputs, replace_scoped_todo_tools_from_visible_snapshot,
    todo_item_visible_entity_snapshot, todo_last_action_visible_entity_snapshot,
    todo_visible_entity_snapshot, visible_snapshot_has_todo_items,
};

/// 构造 Todo 领域完整 Tool 集合。外层 Registry 只注册该集合，不需要知道具体
/// Tool 类型、数量或各 Tool 的存储依赖。
pub(crate) fn registered_tools(
    todo_store: TodoStore,
    session_store: SessionStore,
    notification_store: NotificationOutboxStore,
) -> Vec<DynTool> {
    vec![
        Arc::new(ListTodoTool::new(todo_store.clone(), session_store.clone())),
        Arc::new(GetTodoTool::new(todo_store.clone(), session_store.clone())),
        Arc::new(CreateTodoTool::new(
            todo_store.clone(),
            session_store.clone(),
            notification_store.clone(),
        )),
        Arc::new(CompleteTodoTool::new(
            todo_store.clone(),
            session_store.clone(),
            notification_store.clone(),
        )),
        Arc::new(EditTodoTool::new(
            todo_store.clone(),
            session_store.clone(),
            notification_store.clone(),
        )),
        Arc::new(RestoreTodoTool::new(
            todo_store.clone(),
            session_store.clone(),
            notification_store.clone(),
        )),
        Arc::new(DeleteTodoTool::new(
            todo_store.clone(),
            session_store.clone(),
            notification_store.clone(),
        )),
        Arc::new(MergeTodoTool::new(
            todo_store.clone(),
            session_store.clone(),
            notification_store.clone(),
        )),
        Arc::new(ManageRecurringReminderTool::new(
            todo_store,
            session_store,
            notification_store,
        )),
    ]
}

#[cfg(test)]
mod pending_tests;
#[cfg(test)]
mod tests;

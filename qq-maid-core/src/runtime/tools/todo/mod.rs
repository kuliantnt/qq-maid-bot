//! Todo Tool。
//!
//! 这些 Tool 只把模型参数适配到现有 TodoStore、Session 快照和 pending 机制。
//! 内部 ID 不返回给模型；跨轮次编号只来自用户实际看到的列表，Tool Loop 内部
//! `list_todos` 查询只保留在当前 task 的临时选择上下文中。
//!
//! 模块拆分（保持公共导出与历史不变）：
//! - `common`：常量、选择/引用类型与参数解析 helper、错误转换。
//! - `scope`：`TodoToolScope` 与可见编号 / 最近对象解析。
//! - `selection`：prepare/execute 共用的预解析与结果映射helper。
//! - `json`：面向模型的 JSON 序列化与状态文案。
//! - `recurrence`/`reminder`/`reminder_worker`/`template`：Todo 领域的重复规则意图、提醒 outbox、每日提醒调度和展示模板。
//! - `list`/`create`/`complete`/`edit`/`restore`/`delete`：各 Tool 实现。

mod common;
pub(crate) mod edit_patch;
pub(crate) mod flow;
pub(crate) mod format;
mod json;
pub(crate) mod ops;
pub(crate) mod receipt;
pub(crate) mod recurrence;
pub(crate) mod reminder;
pub(crate) mod reminder_worker;
mod scope;
mod selection;
pub(crate) mod status;
pub(crate) mod storage;
pub(crate) mod template;
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

pub use complete::CompleteTodoTool;
pub use create::CreateTodoTool;
pub use delete::DeleteTodoTool;
pub use edit::EditTodoTool;
pub use edit_patch::TodoEditPatch;
pub use get::GetTodoTool;
pub use list::ListTodoTool;
pub use merge::MergeTodoTool;
pub use recurring::ManageRecurringReminderTool;
pub(crate) use reminder::{
    TodoReminderSentHook, cancel_reminder_task, cancel_reminder_task_by_id, sync_reminder_task,
    validate_draft_reminder,
};
pub use reminder_worker::{TodoReminderScheduler, TodoReminderSchedulerConfig};
pub use restore::RestoreTodoTool;
pub use storage::*;
pub(crate) use template::{ReminderFieldMode, TodoCardOptions, TodoRenderItem, format_todo_cards};
pub(crate) use visible_entity::{
    TodoScopedToolInputs, replace_scoped_todo_tools_from_visible_snapshot,
    todo_item_visible_entity_snapshot, todo_last_action_visible_entity_snapshot,
    todo_visible_entity_snapshot, visible_snapshot_has_todo_items,
};

#[cfg(test)]
mod tests;

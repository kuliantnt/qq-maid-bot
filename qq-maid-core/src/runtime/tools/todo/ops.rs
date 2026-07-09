//! Todo 应用操作门面。
//!
//! 统一“执行存储层状态变更 -> 维护 session 快照”的不变量，避免指令侧
//! (`/todo` flow) 和工具调用侧 (`*_todo` Tool) 各自重写同一套
//! “新增/完成/恢复后清空 `last_todo_query`、更新 `last_todo_action`”的时序。
//!
//! 约束（详见 AGENTS.md “Core / Todo / Memory / Session 注意事项”）：
//! - 本层只做存储层变更和 session 副作用，不调用 LLM、不构造 pending、
//!   不持久化 session；持久化仍由调用方（Tool 的 `scope.save()` 或指令侧的
//!   append/save）负责。
//! - 内部 ID 由调用方完成“可见编号 -> ID”解析后传入；本层不接受可见编号。
//! - 快照清空/记忆规则与历史实现严格一致：批量操作只在成功变更非空时清空
//!   `last_todo_query`，避免全部 skipped 时把用户仍可复用的列表快照误清掉；
//!   单条新增或确认链路成功后无条件清空并记录最近对象。
//! - pending 类型定义和总分发仍在 `runtime/pending` 与 `respond/pending.rs`。

use crate::runtime::{
    session::SessionRecord,
    tools::todo::{
        TodoBulkRestoreOutcome, TodoCompleteProgressOutcome, TodoItem, TodoItemDraft, TodoOwner,
        TodoStore,
    },
};

/// 新增单条待办，并维护 session 最近对象快照。
///
/// 新增待办已改为直接写入；成功后必须立即清空旧列表编号快照，
/// 并把新条目记录为 `created` 最近对象，供后续“刚刚那个”续指使用。
pub fn create_one(
    store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    draft: TodoItemDraft,
) -> Result<TodoItem, crate::runtime::tools::todo::TodoError> {
    let created = store.create(owner, draft)?;
    session.last_todo_query = None;
    session.remember_last_todo_action(&owner.key, &created, "created");
    Ok(created)
}

/// 批量新增待办，并维护 session 最近对象快照。
///
/// Tool Loop 一轮可表达多个待办创建意图时，必须把它们作为同一用户操作处理；
/// 成功后只清空一次旧列表快照。多条创建不会记录单一 `last_todo_action`，
/// 避免“刚刚那个”在批量上下文里错误指向任意一条。
pub fn create_many(
    store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    drafts: Vec<TodoItemDraft>,
) -> Result<Vec<TodoItem>, crate::runtime::tools::todo::TodoError> {
    let created = store.create_many(owner, drafts)?;
    if !created.is_empty() {
        session.last_todo_query = None;
        session.update_last_todo_action_from_items(&owner.key, "created", &created);
    }
    Ok(created)
}

/// 批量“完成本次待办”，兼容一次性待办与重复待办。
///
/// 一次性待办沿用原 Completed 终态；重复待办则保留 Pending，
/// 仅把时间推进到下一次，避免预生成无限未来实例。
pub fn complete_many_with_recurrence(
    store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    ids: &[String],
) -> Result<TodoCompleteProgressOutcome, crate::runtime::tools::todo::TodoError> {
    let outcome = store.complete_by_ids_with_recurrence(owner, ids)?;
    let changed = outcome.all_changed();
    if !changed.is_empty() {
        session.last_todo_query = None;
        session.update_last_todo_action_from_items(&owner.key, "completed", &changed);
    }
    Ok(outcome)
}

/// 批量恢复已完成待办（仅 completed），并维护 session 快照。
///
/// 用于指令侧 `/todo undo` 从已完成列表恢复；与 `complete_many` 同样的
/// “非空才清空”守卫，避免全部 skipped 时清掉用户仍可复用的已完成列表快照。
pub fn restore_completed_many(
    store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    ids: &[String],
) -> Result<TodoBulkRestoreOutcome, crate::runtime::tools::todo::TodoError> {
    let outcome = store.restore_completed_by_ids(owner, ids)?;
    if !outcome.restored.is_empty() {
        session.last_todo_query = None;
        session.update_last_todo_action_from_items(&owner.key, "restored", &outcome.restored);
    }
    Ok(outcome)
}

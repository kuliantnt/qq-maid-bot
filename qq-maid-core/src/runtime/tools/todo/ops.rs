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
//!   新增或确认链路成功后无条件清空并记录最近对象。
//! - Todo pending payload 与状态机在 `tools/todo/pending.rs` 和
//!   `tools/todo/flow/pending.rs`；本层不构造 pending。

use crate::runtime::{
    session::SessionRecord,
    tools::todo::{
        TodoBulkRestoreOutcome, TodoCompleteProgressOutcome, TodoItem, TodoItemDraft, TodoOwner,
        TodoRecurrenceKind, TodoStatus, TodoStore,
    },
};

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

/// 批量跳过重复提醒当前周期，并维护 session 快照。
///
/// 新增周期控制 Tool 时优先在本层扩展“业务操作门面”，Tool 本身只负责
/// 选择解析、通知同步和 JSON 输出，避免每个 Tool 直接拼存储读写细节。
pub fn skip_recurring_current_period(
    store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    ids: &[String],
) -> Result<TodoCompleteProgressOutcome, crate::runtime::tools::todo::TodoError> {
    let outcome = store.advance_recurring_by_ids(owner, ids)?;
    if !outcome.advanced.is_empty() {
        session.last_todo_query = None;
        session.update_last_todo_action_from_items(&owner.key, "skipped", &outcome.advanced);
    }
    Ok(outcome)
}

/// 关闭后续重复提醒但保留待办本身为未完成。
///
/// 当前 schema 没有独立暂停状态；关闭重复通过清空 recurrence + reminder_at 表达。
pub fn disable_recurrence_many(
    store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    ids: &[String],
) -> Result<Vec<TodoItem>, crate::runtime::tools::todo::TodoError> {
    let pending = store.list_by_ids_with_status(owner, ids, TodoStatus::Pending)?;
    let mut disabled = Vec::new();
    for item in pending.into_iter().filter(is_recurring_todo) {
        let mut draft = TodoItemDraft::from_item(&item, "关闭重复提醒");
        draft.mark_explicit_no_recurrence();
        draft.reminder_at = None;
        disabled.push(store.edit(owner, &item.id, draft)?);
    }
    if !disabled.is_empty() {
        session.last_todo_query = None;
        session.update_last_todo_action_from_items(
            owner.key.as_str(),
            "recurrence_disabled",
            &disabled,
        );
    }
    Ok(disabled)
}

fn is_recurring_todo(item: &TodoItem) -> bool {
    !matches!(item.recurrence_kind, TodoRecurrenceKind::None)
        || item.recurrence_interval > 0
        || item.recurrence_interval_days > 0
}

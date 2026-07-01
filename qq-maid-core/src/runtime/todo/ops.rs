//! Todo 应用操作门面。
//!
//! 统一“执行存储层状态变更 -> 维护 session 快照”的不变量，避免指令侧
//! (`/todo` flow) 和工具调用侧 (`*_todo` Tool) 各自重写同一套
//! “完成/恢复/软取消后清空 `last_todo_query`、更新 `last_todo_action`”的时序。
//!
//! 约束（详见 AGENTS.md “Core / Todo / Memory / Session 注意事项”）：
//! - 本层只做存储层变更和 session 副作用，不调用 LLM、不构造 pending、
//!   不持久化 session；持久化仍由调用方（Tool 的 `scope.save()` 或指令侧的
//!   append/save）负责。
//! - 内部 ID 由调用方完成“可见编号 -> ID”解析后传入；本层不接受可见编号。
//! - 快照清空/记忆规则与历史实现严格一致：批量操作只在成功变更非空时清空
//!   `last_todo_query`，避免全部 skipped 时把用户仍可复用的列表快照误清掉；
//!   单条操作（pending 确认链路）必然成功，因此无条件清空并记录最近对象。
//! - pending 类型定义和总分发仍在 `runtime/pending` 与 `respond/pending.rs`。

use crate::runtime::{
    session::SessionRecord,
    todo::{TodoBulkCompleteOutcome, TodoBulkRestoreOutcome, TodoItem, TodoOwner, TodoStore},
};

/// 完成单条待办，并维护 session 最近对象快照。
///
/// 单条完成只在 pending `TodoDone` 确认链路中调用：待办必然存在且为未完成，
/// 因此无论结果如何都清空最近列表快照并记录最近对象，与历史 `TodoDone`
/// 确认分支保持一致。
pub fn complete_one(
    store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    id: &str,
) -> Result<TodoItem, crate::runtime::todo::TodoError> {
    let completed = store.complete(owner, id)?;
    session.last_todo_query = None;
    session.remember_last_todo_action(&owner.key, &completed, "completed");
    Ok(completed)
}

/// 批量完成待办，并维护 session 快照。
///
/// 与 `complete_one` 不同：批量接口允许部分编号命中失败（skipped），只有至少
/// 成功完成一条时才清空 `last_todo_query` 并更新 `last_todo_action`，避免用户
/// 还能继续按原列表重试时被提前清掉快照。
pub fn complete_many(
    store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    ids: &[String],
) -> Result<TodoBulkCompleteOutcome, crate::runtime::todo::TodoError> {
    let outcome = store.complete_by_ids(owner, ids)?;
    if !outcome.completed.is_empty() {
        session.last_todo_query = None;
        session.update_last_todo_action_from_items(&owner.key, "completed", &outcome.completed);
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
) -> Result<TodoBulkRestoreOutcome, crate::runtime::todo::TodoError> {
    let outcome = store.restore_completed_by_ids(owner, ids)?;
    if !outcome.restored.is_empty() {
        session.last_todo_query = None;
        session.update_last_todo_action_from_items(&owner.key, "restored", &outcome.restored);
    }
    Ok(outcome)
}

/// 同时恢复已完成和已取消待办的批量结果。
///
/// 工具调用侧 `restore_todos` 给出的可见编号可能同时包含两类条目，必须分别
/// 调用两个存储接口尝试恢复；两类结果各自保留，由调用方再按 resolved 编号
/// 映射成面向模型的输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoRestoreBothOutcome {
    /// `restore_completed_by_ids` 的结果。
    pub completed: TodoBulkRestoreOutcome,
    /// `restore_cancelled_by_ids` 的结果。
    pub cancelled: TodoBulkRestoreOutcome,
}

impl TodoRestoreBothOutcome {
    /// 全部成功恢复的条目（completed + cancelled），用于最近对象快照维护。
    pub fn all_restored(&self) -> Vec<TodoItem> {
        let mut combined = self.completed.restored.clone();
        combined.extend(self.cancelled.restored.clone());
        combined
    }
}

/// 批量恢复已完成与已取消待办，并维护 session 快照。
///
/// 只有任一类命中时才清空 `last_todo_query`，并以“两类并集”更新
/// `last_todo_action`，与历史 Tool 实现保持一致。
pub fn restore_both(
    store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    ids: &[String],
) -> Result<TodoRestoreBothOutcome, crate::runtime::todo::TodoError> {
    let completed = store.restore_completed_by_ids(owner, ids)?;
    let cancelled = store.restore_cancelled_by_ids(owner, ids)?;
    let combined = completed.restored.clone(); // 暂存，避免与 cancelled 借用冲突
    let mut combined_all = combined;
    combined_all.extend(cancelled.restored.clone());
    if !combined_all.is_empty() {
        session.last_todo_query = None;
        session.update_last_todo_action_from_items(&owner.key, "restored", &combined_all);
    }
    Ok(TodoRestoreBothOutcome {
        completed,
        cancelled,
    })
}

/// 软取消单条待办（仅状态变更为已取消），并维护 session 快照。
///
/// 用于 pending `TodoDelete` 确认分支中“未完成待办”的软删除语义：历史实现会清空
/// `last_todo_query` 并记录 “cancelled” 最近对象，这里保持完全一致。
/// 物理删除已完成/已取消待办不经过这里，仍由调用方直接走带状态校验的存储接口。
pub fn cancel_one(
    store: &TodoStore,
    session: &mut SessionRecord,
    owner: &TodoOwner,
    id: &str,
) -> Result<TodoItem, crate::runtime::todo::TodoError> {
    let deleted = store.cancel(owner, id)?;
    session.last_todo_query = None;
    session.remember_last_todo_action(&owner.key, &deleted, "cancelled");
    Ok(deleted)
}

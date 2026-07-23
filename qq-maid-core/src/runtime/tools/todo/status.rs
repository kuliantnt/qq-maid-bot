//! Todo 状态与时间精度的统一字串映射。
//!
//! 指令侧文案（`/todo` flow）和工具调用侧 JSON（`*_todo` Tool）各自维护过
//! 状态/精度的字串转换，存在多份重复且口径易漂移。这里把机器格式串和短中文
//! 文案收敛到一处；长中文文案（如“已完成待办”“未完成待办”）因拼接语义不同
//! 仍各自保留，避免合并后误改 QQ 侧文案。
//!
//! 不变量：
//! - 机器格式串必须与 `TodoStatus` 的
//!   `#[serde(rename_all = "snake_case")]` 保持一致，避免与存储层序列化漂移。
//! - 短中文文案只用于行内状态点缀（`format_todo_*`），不得替换为带“待办”后缀的
//!   长文案，否则会改变 QQ 侧确认/删除提示文案。

use crate::runtime::tools::{
    status::{StatusAction, StatusHint, StatusSubject},
    todo::{TodoStatus, route::TodoIntentAction},
};

/// 根据 Todo Tool 名称生成通用状态提示；具体工具集合由 Todo 模块维护。
pub(crate) fn status_hint_for_tool_name(tool_name: &str) -> Option<StatusHint> {
    let action = match tool_name {
        "list_todos" | "get_todo" => StatusAction::Query,
        "create_todo" | "edit_todo" | "merge_todos" | "manage_recurring_reminder" => {
            StatusAction::Write
        }
        "complete_todos" | "restore_todos" | "delete_todos" => StatusAction::Confirm,
        "clarification_control" => StatusAction::Process,
        _ => return None,
    };
    Some(StatusHint::new(StatusSubject::Todo, action))
}

/// Todo 自然语言意图只在领域内分类，外层状态调度器不维护 Todo 词表和动作映射。
pub(crate) fn classify_status_hint(text: &str, has_recent_context: bool) -> Option<StatusHint> {
    let lower = text.to_ascii_lowercase();
    let intent = super::route::classify_todo_intent(text, &lower, has_recent_context);
    if !intent.is_confident() {
        return None;
    }
    let action = match super::route::todo_intent_action(text) {
        TodoIntentAction::Confirm => StatusAction::Confirm,
        TodoIntentAction::Write => StatusAction::Write,
        TodoIntentAction::Query => StatusAction::Query,
        TodoIntentAction::Process => StatusAction::Process,
    };
    Some(StatusHint::new(StatusSubject::Todo, action))
}

/// 面向模型的机器格式状态串，与 `TodoStatus` 的 serde snake_case 口径一致。
pub fn status_machine_str(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::Completed => "completed",
    }
}

/// 短中文状态文案（不带“待办”后缀），用于行内状态点缀与批量删除提示拼接。
pub fn status_cn_short(status: &TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "未完成",
        TodoStatus::Completed => "已完成",
    }
}

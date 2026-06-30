//! 记忆模块面向用户可见的回复格式化与文案常量。
//!
//! 列表、详情、创建/更新/删除确认、等待确认提示集中在本模块维护，避免文案散落
//! 在主流程中。文案改动需保持现有 QQ 侧体验稳定；不在这里处理 scope 或写入逻辑。
//!
//! 这里不改变 `/memory`、`/记忆`、`/记` 的创建/查看语义，普通聊天也不会经由此处
//! 自动写长期记忆。

use crate::runtime::{
    memory::MemoryRecord,
    pending::PendingMemoryUpdate,
    respond::{common::truncate_chars, session_flow::datetime_for_display},
};

use super::scope::MemoryCommandScope;

// 旧版 /zy 指令的迁移提示
pub(super) const MEMORY_DRAFT_LEGACY_USAGE_REPLY: &str =
    "/zy 仍可使用，但推荐改用：/memory 要保存的记忆内容
也可以使用：/记忆、/记";
// 非斜杠开头的“记一下”等旧版语法的提示
pub(super) const MEMORY_LEGACY_HINT_REPLY: &str = "长期记忆请使用：/memory 要保存的内容
也可以使用：/记忆 要保存的内容";
pub(super) const MEMORY_GROUP_PRIVATE_REJECT_REPLY: &str = "群记忆只能在群聊中查看或管理。";
pub(super) const MEMORY_SCOPE_MISMATCH_REPLY: &str = "这条记忆不在当前可管理范围内。";

pub(super) fn format_memory_list_reply(
    records: &[MemoryRecord],
    query: &str,
    command_scope: &MemoryCommandScope,
) -> String {
    if records.is_empty() {
        if query.trim().is_empty() {
            return format!("当前没有{}长期记忆。", command_scope.label);
        }
        return format!("没有找到匹配的{}长期记忆。", command_scope.label);
    }
    let mut rows = vec![format!("{}长期记忆：", command_scope.label)];
    for (index, record) in records.iter().enumerate() {
        rows.push(format!(
            "{}. {} [{}/{}] {}",
            index + 1,
            short_memory_id(&record.id),
            record.memory_type,
            record.scope,
            truncate_chars(&record.content, 80)
        ));
    }
    let prefix = if command_scope.group_command {
        "/memory group"
    } else {
        "/memory"
    };
    rows.push(format!(
        "操作：{prefix} show 1；{prefix} edit 1 新内容；{prefix} delete 1"
    ));
    rows.join("\n")
}

pub(super) fn format_memory_detail_reply(record: &MemoryRecord) -> String {
    let created_at = if record.created_at.trim().is_empty() {
        &record.ts
    } else {
        &record.created_at
    };
    let mut rows = vec![
        format!("记忆 {}：", short_memory_id(&record.id)),
        format!("- 类型：{}", record.memory_type),
        format!("- 范围：{}", record.scope),
        format!("- 时间：{}", datetime_for_display(created_at)),
    ];
    if let Some(updated_at) = &record.updated_at {
        rows.push(format!("- 更新：{}", datetime_for_display(updated_at)));
    }
    rows.push(format!("- 内容：{}", record.content));
    rows.join("\n")
}

pub(super) fn format_memory_create_confirm(content: &str) -> String {
    format!(
        "整理成这条记忆草稿：{}\n\n{}",
        content.trim(),
        build_memory_confirm_hint()
    )
}

pub(super) fn format_memory_pending_create_waiting_reply() -> String {
    "这条记忆草稿还在等待确认。要写入请回复“确认 / 可以 / 记吧”；要调整请直接继续补充修改意见；要放弃请回复“取消 / 不记 / 算了”。"
        .to_owned()
}

pub(super) fn format_memory_pending_update_waiting_reply() -> String {
    "这次记忆修改还在等待确认。要执行请回复“确认 / 可以 / 好”；要调整请直接继续补充修改意见；要放弃请回复“取消 / 不记 / 算了”。"
        .to_owned()
}

pub(super) fn format_memory_pending_delete_waiting_reply() -> String {
    "这次记忆删除还在等待确认。要删除请回复“确认 / 可以 / 好”；要放弃请回复“取消 / 不记 / 算了”。"
        .to_owned()
}

pub(super) fn format_memory_update_confirm(
    record: &MemoryRecord,
    update: &PendingMemoryUpdate,
) -> String {
    format_pending_memory_update_confirm_with_id(&short_memory_id(&record.id), update)
}

pub(super) fn format_pending_memory_update_confirm(update: &PendingMemoryUpdate) -> String {
    format_pending_memory_update_confirm_with_id(&short_memory_id(&update.id), update)
}

fn format_pending_memory_update_confirm_with_id(
    memory_id: &str,
    update: &PendingMemoryUpdate,
) -> String {
    [
        format!("待确认修改记忆 {}：", memory_id),
        format!("- 原内容：{}", truncate_chars(&update.before_content, 120)),
        format!("- 新内容：{}", update.content),
        format!("- 新类型：{}", update.memory_type),
        format!("- 新范围：{}", update.scope),
        build_memory_operation_confirm_hint(),
    ]
    .join("\n")
}

pub(super) fn format_memory_delete_confirm(record: &MemoryRecord) -> String {
    [
        format!("确认删除这条记忆 {}？", short_memory_id(&record.id)),
        format!("- 类型：{}", record.memory_type),
        format!("- 范围：{}", record.scope),
        format!("- 内容：{}", truncate_chars(&record.content, 120)),
        build_memory_operation_confirm_hint(),
    ]
    .join("\n")
}

pub(super) fn format_memory_no_list_index_reply(
    target: &str,
    command_scope: &MemoryCommandScope,
) -> String {
    let list_command = if command_scope.group_command {
        "/memory group"
    } else {
        "/memory"
    };
    format!(
        "最近的{}记忆列表里没有第 {} 条。请先发送 {list_command} 查看列表，再使用列表序号。",
        command_scope.label,
        target.trim()
    )
}

fn build_memory_confirm_hint() -> String {
    "回复“确认 / 可以 / 记吧”写入长期记忆。\n回复“取消 / 不记 / 算了”放弃。".to_owned()
}

fn build_memory_operation_confirm_hint() -> String {
    "回复“确认 / 可以 / 好”执行。\n回复“取消 / 不记 / 算了”放弃。".to_owned()
}

/// 截取记忆 ID 前 8 个字符用于展示，避免在回复里暴露完整 UUID。
/// 需要在 `respond` 层被外部测试引用，故可见范围放宽到整个 `respond` 模块树。
pub(in crate::runtime::respond) fn short_memory_id(memory_id: &str) -> String {
    memory_id.chars().take(8).collect()
}

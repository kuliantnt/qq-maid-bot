//! 记忆模块面向用户可见的回复格式化与文案常量。
//!
//! 列表、详情、创建/更新/删除确认、等待确认提示集中在本模块维护，避免文案散落
//! 在主流程中。文案改动需保持现有 QQ 侧体验稳定；不在这里处理 scope 或写入逻辑。
//!
//! 这里不改变 `/memory`、`/记忆`、`/记` 的创建/查看语义，普通聊天也不会经由此处
//! 自动写长期记忆。

use crate::runtime::{
    memory::MemoryRecord,
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

/// 截取记忆 ID 前 8 个字符用于展示，避免在回复里暴露完整 UUID。
/// 需要在 `respond` 层被外部测试引用，故可见范围放宽到整个 `respond` 模块树。
pub(in crate::runtime::respond) fn short_memory_id(memory_id: &str) -> String {
    memory_id.chars().take(8).collect()
}

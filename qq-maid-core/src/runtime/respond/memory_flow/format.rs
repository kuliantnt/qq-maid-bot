//! 记忆模块面向用户可见的回复格式化与文案常量。
//!
//! 列表、详情、创建/更新/删除确认、等待确认提示集中在本模块维护，避免文案散落
//! 在主流程中。文案改动需保持现有 QQ 侧体验稳定；不在这里处理 scope 或写入逻辑。
//!
//! 普通聊天不会经由此处自动写长期记忆。

use crate::runtime::{
    respond::{common::truncate_chars, session_flow::datetime_for_display},
    tools::memory::{MemoryKind, MemoryRecord},
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
    let scope_title = memory_kind_label(command_scope.kind());
    if records.is_empty() {
        if query.trim().is_empty() {
            return format!(
                "🧠 {scope_title}\n\n当前还没有内容。\n\n可直接发送：{} 要记住的内容",
                command_scope.command_prefix
            );
        }
        return format!("🧠 {scope_title}\n\n没有找到匹配“{}”的内容。", query);
    }
    let mut rows = vec![
        format!("🧠 {scope_title}（共 {} 条）", records.len()),
        String::new(),
    ];
    for (index, record) in records.iter().enumerate() {
        rows.push(format!(
            "{}. {}",
            index + 1,
            truncate_chars(&record.content, 100)
        ));
    }
    let prefix = command_scope.command_prefix;
    rows.extend([
        String::new(),
        "可回复：".to_owned(),
        format!("- `{prefix} show 1`"),
        format!("- `{prefix} edit 1 新内容`"),
        format!("- `{prefix} delete 1`"),
    ]);
    rows.join("\n")
}

pub(super) fn format_memory_detail_reply(record: &MemoryRecord) -> String {
    let created_at = if record.created_at.trim().is_empty() {
        &record.ts
    } else {
        &record.created_at
    };
    let mut rows = vec![
        "🧠 记忆详情".to_owned(),
        String::new(),
        format!("范围：{}", memory_kind_label(record.memory_kind)),
        format!("内容：{}", record.content),
        format!("创建：{}", datetime_for_display(created_at)),
    ];
    if let Some(updated_at) = &record.updated_at {
        rows.push(format!("更新：{}", datetime_for_display(updated_at)));
    }
    rows.join("\n")
}

pub(super) fn format_memory_no_list_index_reply(
    target: &str,
    command_scope: &MemoryCommandScope,
) -> String {
    let list_command = command_scope.command_prefix;
    format!(
        "最近的{}列表里没有第 {} 条。请先发送 {list_command} 查看列表，再使用列表序号。",
        memory_kind_label(command_scope.kind()),
        target.trim()
    )
}

pub(super) fn memory_kind_label(kind: MemoryKind) -> &'static str {
    crate::runtime::tools::memory::memory_kind_label(kind)
}

/// 旧回归测试用于构造短 ID 输入的辅助函数；用户回复不再展示任何记忆 ID。
/// 需要在 `respond` 层被外部测试引用，故可见范围放宽到整个 `respond` 模块树。
#[cfg(test)]
pub(in crate::runtime::respond) fn short_memory_id(memory_id: &str) -> String {
    memory_id.chars().take(8).collect()
}

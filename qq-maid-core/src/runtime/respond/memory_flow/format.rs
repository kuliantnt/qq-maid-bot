//! 记忆模块面向用户可见的回复格式化与文案常量。
//!
//! 列表、详情、创建/更新/删除确认、等待确认提示集中在本模块维护，避免文案散落
//! 在主流程中。文案改动需保持现有 QQ 侧体验稳定；不在这里处理 scope 或写入逻辑。
//!
//! 普通聊天不会经由此处自动写长期记忆。

use qq_maid_common::markdown::escape_inline;

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
    let command_prefix = localized_memory_command_prefix(command_scope.kind());
    if records.is_empty() {
        if query.trim().is_empty() {
            let add_command = localized_memory_add_command(command_scope.kind());
            let add_hint = match command_scope.kind() {
                MemoryKind::Group => {
                    format!("群主或管理员添加：`{add_command}`")
                }
                _ => format!("添加内容：`{add_command}`"),
            };
            return [
                format!("# 🧠 {scope_title}"),
                String::new(),
                "当前还没有保存内容。".to_owned(),
                String::new(),
                "## 快速操作".to_owned(),
                String::new(),
                format!("- {add_hint}"),
                format!("- 查看列表：`{command_prefix}`"),
            ]
            .join("\n");
        }
        return [
            format!("# 🧠 {scope_title}"),
            String::new(),
            format!("没有找到匹配“{}”的内容。", escape_inline(query)),
            String::new(),
            "## 可以试试".to_owned(),
            String::new(),
            format!("- 查看全部：`{command_prefix}`"),
            format!("- 更换关键词：`{command_prefix} 列表 关键词`"),
        ]
        .join("\n");
    }
    let mut rows = vec![
        format!("# 🧠 {scope_title}"),
        String::new(),
        format!("共 {} 条", records.len()),
        String::new(),
    ];
    for (index, record) in records.iter().enumerate() {
        rows.push(format!(
            "{}. {}",
            index + 1,
            escape_inline(&truncate_chars(&record.content, 100))
        ));
    }
    rows.extend([
        String::new(),
        "## 可用操作".to_owned(),
        String::new(),
        format!("- 查看详情：`{command_prefix} 查看 1`"),
        format!("- 修改内容：`{command_prefix} 修改 1 新内容`"),
        format!("- 删除内容：`{command_prefix} 删除 1`"),
        format!("- 搜索内容：`{command_prefix} 列表 关键词`"),
    ]);
    rows.join("\n")
}

fn localized_memory_command_prefix(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Personal | MemoryKind::LegacyUnassigned => "/记忆",
        MemoryKind::GroupProfile => "/记忆 画像",
        MemoryKind::Group => "/记忆 群",
    }
}

fn localized_memory_add_command(kind: MemoryKind) -> &'static str {
    match kind {
        MemoryKind::Personal | MemoryKind::LegacyUnassigned => "/记忆 要记住的内容",
        MemoryKind::GroupProfile => "/记忆 画像 添加 要记住的内容",
        MemoryKind::Group => "/记忆 群 添加 要记住的内容",
    }
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

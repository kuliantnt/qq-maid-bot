//! 记忆指令解析与旧版语法兼容入口。
//!
//! 这里只负责把 `/memory` 系列输入拆解成 `ParsedCommand`，以及识别旧版
//! “记一下……”等非斜杠语法。群聊/私聊 scope 判定、最近列表序号解析都不在本模块：
//! 指令解析完成后交给 `scope` 与主流程 `mod` 进一步处理。
//!
//! 边界：旧版语法只做迁移提示，不会直接写入长期记忆；草稿仍需走确认流程。

use crate::runtime::command::{ParsedCommand, parse_slash_command};

/// 解析 `/memory` 草稿指令（无子命令的情况）。
pub(super) fn parse_memory_draft_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_slash_command(text)?;
    (command.action == "memory").then_some(command)
}

/// 解析 `/memory` 管理子命令（list / show / edit / delete 等）。
pub(super) fn parse_memory_management_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_memory_draft_command(text)?;
    let mut parts = command.argument.splitn(2, char::is_whitespace);
    let subcommand = parts.next()?.trim().to_ascii_lowercase();
    let (action, argument) = match subcommand.as_str() {
        // group/群 是显式群记忆命名空间；其后的空参数按群列表处理。
        "group" | "群" => {
            let rest = parts.next().unwrap_or("").trim();
            let mut group_parts = rest.splitn(2, char::is_whitespace);
            let group_subcommand = group_parts.next().unwrap_or("").trim().to_ascii_lowercase();
            match group_subcommand.as_str() {
                "" | "list" | "ls" | "列表" | "search" | "find" | "搜索" => (
                    "memory_list",
                    group_argument(group_parts.next().unwrap_or("").trim()),
                ),
                "add" | "新增" | "添加" => {
                    return None;
                }
                "show" | "get" | "查看" | "详情" => (
                    "memory_show",
                    group_argument(group_parts.next().unwrap_or("").trim()),
                ),
                "edit" | "set" | "修改" | "改" => (
                    "memory_edit",
                    group_argument(group_parts.next().unwrap_or("").trim()),
                ),
                "update" | "更新" => ("memory_update_hint", group_argument("")),
                "delete" | "del" | "rm" | "删除" => (
                    "memory_delete",
                    group_argument(group_parts.next().unwrap_or("").trim()),
                ),
                _ => ("memory_list", group_argument(rest)),
            }
        }
        "list" | "ls" | "列表" | "search" | "find" | "搜索" => {
            ("memory_list", parts.next().unwrap_or("").trim().to_owned())
        }
        "show" | "get" | "查看" | "详情" => {
            ("memory_show", parts.next().unwrap_or("").trim().to_owned())
        }
        "edit" | "set" | "修改" | "改" => {
            ("memory_edit", parts.next().unwrap_or("").trim().to_owned())
        }
        "update" | "更新" => (
            "memory_update_hint",
            parts.next().unwrap_or("").trim().to_owned(),
        ),
        "delete" | "del" | "rm" | "删除" => (
            "memory_delete",
            parts.next().unwrap_or("").trim().to_owned(),
        ),
        _ => return None,
    };
    Some(ParsedCommand {
        action: action.to_owned(),
        argument,
        raw_command: command.raw_command,
    })
}

/// 统一的记忆指令解析入口：依次尝试管理子命令、草稿指令与旧版语法。
/// 需要在 `respond` 层被引用，故可见范围放宽到整个 `respond` 模块树。
pub(in crate::runtime::respond) fn parse_memory_command(text: &str) -> Option<ParsedCommand> {
    parse_memory_management_command(text)
        .or_else(|| parse_memory_draft_command(text))
        .or_else(|| {
            is_legacy_memory_request(text).then(|| ParsedCommand {
                action: "memory".to_owned(),
                argument: text.trim().to_owned(),
                raw_command: "legacy_memory".to_owned(),
            })
        })
}

/// `/memory <内容>` 草稿指令实际需要写入记忆的内容，剥掉 `group add` 等前缀。
pub(super) fn memory_draft_argument(command: &ParsedCommand) -> String {
    let argument = command.argument.trim();
    for prefix in [
        "group add",
        "group 新增",
        "group 添加",
        "群 add",
        "群 新增",
        "群 添加",
    ] {
        if let Some(rest) = argument.strip_prefix(prefix) {
            return rest.trim().to_owned();
        }
    }
    argument.to_owned()
}

/// 管理子命令去掉 `group / 群` 命名空间前缀后真正需要操作的参数。
pub(super) fn memory_scoped_argument(command: &ParsedCommand) -> String {
    let argument = command.argument.trim();
    for prefix in ["group", "群"] {
        if argument == prefix {
            return String::new();
        }
        if let Some(rest) = argument.strip_prefix(&format!("{prefix} ")) {
            return rest.trim().to_owned();
        }
    }
    argument.to_owned()
}

/// 把群记忆子命令的参数统一拼回带 `group` 前缀的形式，方便后续 scope 解析。
fn group_argument(argument: &str) -> String {
    if argument.trim().is_empty() {
        "group".to_owned()
    } else {
        format!("group {}", argument.trim())
    }
}

/// `/memory edit 列表序号 新内容` 中序号与正文的切分。
pub(super) fn parse_memory_edit_argument(argument: &str) -> Option<(String, String)> {
    let mut parts = argument.splitn(2, char::is_whitespace);
    let memory_id = parts.next()?.trim().to_owned();
    let content = parts.next()?.trim().to_owned();
    if memory_id.is_empty() || content.is_empty() {
        None
    } else {
        Some((memory_id, content))
    }
}

/// 旧版“记一下……”“写入记忆”等非斜杠语法的识别，仅用于引导改用 `/memory`。
pub(super) fn is_legacy_memory_request(text: &str) -> bool {
    let text = text.trim();
    !text.starts_with('/') && (text.starts_with("记一下") || text.contains("写入记忆"))
}

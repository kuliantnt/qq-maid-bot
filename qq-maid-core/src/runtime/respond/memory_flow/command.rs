//! `/memory` 分域命令解析与中英文别名兼容。
//!
//! 命名空间后空参查看列表；`list/列表` 搜索；其余自由文本（含 `add/添加`）走新增草稿。

use crate::runtime::command::{ParsedCommand, parse_slash_command};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MemoryNamespace {
    Personal,
    GroupProfile,
    Group,
}

impl MemoryNamespace {
    pub(super) fn prefix(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::GroupProfile => "profile",
            Self::Group => "group",
        }
    }
}

pub(super) fn parse_memory_draft_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_slash_command(text)?;
    (command.action == "memory").then_some(command)
}

/// 解析列表、详情、纠正、删除、清空和画像授权命令。
pub(super) fn parse_memory_management_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_memory_draft_command(text)?;
    let (namespace, rest) = split_namespace(&command.argument);
    let mut parts = rest.splitn(2, char::is_whitespace);
    let subcommand = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let tail = parts.next().unwrap_or("").trim();
    let (action, argument) = match subcommand.as_str() {
        "" => ("memory_list", String::new()),
        "list" | "ls" | "列表" | "search" | "find" | "搜索" => ("memory_list", tail.to_owned()),
        "show" | "get" | "查看" | "详情" => ("memory_show", tail.to_owned()),
        "edit" | "set" | "修改" | "纠正" | "改" => ("memory_edit", tail.to_owned()),
        "update" | "更新" => ("memory_update_hint", tail.to_owned()),
        "delete" | "del" | "rm" | "删除" => ("memory_delete", tail.to_owned()),
        "clear" | "清空" | "清除" => ("memory_clear", tail.to_owned()),
        "stop" | "disable" | "停用" | "停止保存" => {
            if namespace == Some(MemoryNamespace::GroupProfile) {
                ("memory_profile_disable", tail.to_owned())
            } else {
                return None;
            }
        }
        "enable" | "resume" | "启用" | "恢复保存" | "重新授权" => {
            if namespace == Some(MemoryNamespace::GroupProfile) {
                ("memory_profile_enable", tail.to_owned())
            } else {
                return None;
            }
        }
        // 空参查看列表；显式 list/列表 搜索；其余自由文本（含 add/添加前缀）统一走新增草稿。
        "add" | "新增" | "添加" | "记住" => return None,
        _ => return None,
    };
    Some(ParsedCommand {
        action: action.to_owned(),
        argument: with_namespace(namespace, &argument),
        raw_command: command.raw_command,
    })
}

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

pub(super) fn memory_draft_argument(command: &ParsedCommand) -> String {
    let (_, rest) = split_namespace(&command.argument);
    let rest = rest.trim();
    for prefix in ["add", "新增", "添加", "记住"] {
        if rest == prefix {
            return String::new();
        }
        if let Some(value) = rest.strip_prefix(&format!("{prefix} ")) {
            return value.trim().to_owned();
        }
    }
    rest.to_owned()
}

pub(super) fn memory_scoped_argument(command: &ParsedCommand) -> String {
    split_namespace(&command.argument).1.trim().to_owned()
}

pub(super) fn memory_namespace(command: &ParsedCommand) -> Option<MemoryNamespace> {
    split_namespace(&command.argument).0
}

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

pub(super) fn is_legacy_memory_request(text: &str) -> bool {
    let text = text.trim();
    !text.starts_with('/') && (text.starts_with("记一下") || text.contains("写入记忆"))
}

fn split_namespace(argument: &str) -> (Option<MemoryNamespace>, &str) {
    let argument = argument.trim();
    let mut parts = argument.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let namespace = match first.as_str() {
        "personal" | "个人" | "私聊" => Some(MemoryNamespace::Personal),
        "profile" | "画像" | "群画像" | "本群画像" => Some(MemoryNamespace::GroupProfile),
        "group" | "群" | "群组" | "公共" => Some(MemoryNamespace::Group),
        _ => None,
    };
    if namespace.is_some() {
        (namespace, parts.next().unwrap_or(""))
    } else {
        (None, argument)
    }
}

fn with_namespace(namespace: Option<MemoryNamespace>, argument: &str) -> String {
    match namespace {
        Some(namespace) if argument.trim().is_empty() => namespace.prefix().to_owned(),
        Some(namespace) => format!("{} {}", namespace.prefix(), argument.trim()),
        None => argument.trim().to_owned(),
    }
}

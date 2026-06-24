//! Todo 指令解析。
//!
//! 只负责把 `/todo` 及其中文别名后的子命令归一化为内部 action，
//! 不在这里触发新增、搜索或持久化，避免解析层改变用户可见语义。

use crate::runtime::command::{ParsedCommand, parse_slash_command};

/// 解析 `/todo` 指令，识别子命令和参数。
pub(super) fn parse_todo_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_slash_command(text)?;
    if command.action != "todo" {
        return None;
    }

    let argument = command.argument.trim();
    if argument.is_empty() {
        return Some(ParsedCommand {
            action: "todo_list".to_owned(),
            argument: String::new(),
            raw_command: command.raw_command,
        });
    }

    let mut parts = argument.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim();
    let action = match normalize_todo_action(first) {
        Some("todo_list") => "todo_list",
        Some("todo_all") => "todo_all",
        Some("todo_search") => "todo_search",
        Some("todo_add") => "todo_add",
        Some("todo_done") => "todo_done",
        Some("todo_undo") => "todo_undo",
        Some("todo_edit") => "todo_edit",
        Some("todo_delete") => "todo_delete",
        Some(_) => "todo_search",
        None => "todo_search",
    };
    let argument = if normalize_todo_action(first).is_some() {
        rest.to_owned()
    } else {
        argument.to_owned()
    };
    Some(ParsedCommand {
        action: action.to_owned(),
        argument,
        raw_command: command.raw_command,
    })
}

/// 将用户输入的待办子命令别名归一化为标准动作名称。
fn normalize_todo_action(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "list" | "ls" | "列表" | "查看" | "显示" | "看看" => Some("todo_list"),
        "all" | "全部" => Some("todo_all"),
        "search" | "find" | "查" | "搜" | "搜索" | "查询" | "筛选" => Some("todo_search"),
        "add" | "new" | "create" | "新增" | "添加" | "增加" | "加" | "创建" | "记下" => {
            Some("todo_add")
        }
        "done" | "finish" | "complete" | "完成" | "做完" | "结束" | "已完成" | "搞定" | "打勾" => {
            Some("todo_done")
        }
        "undo" | "restore" | "恢复" | "恢复完成" | "取消完成" | "设为未完成" | "未完成" => {
            Some("todo_undo")
        }
        "edit" | "update" | "modify" | "set" | "修改" | "更新" | "改" | "调整" => {
            Some("todo_edit")
        }
        "delete" | "del" | "rm" | "remove" | "删除" | "删" | "移除" | "作废" => {
            Some("todo_delete")
        }
        _ => None,
    }
}

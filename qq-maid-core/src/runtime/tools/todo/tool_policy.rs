//! Todo Tool 的请求级暴露策略。
//!
//! 模型只能在用户本轮明确表达对应意图时看到高风险逆向工具，避免模型在完成流程中
//! 自行“纠错”或回滚已经发生的持久化修改。

const RESTORE_INTENT_MARKERS: &[&str] = &[
    "恢复",
    "还原",
    "改回未完成",
    "设回未完成",
    "重新设为未完成",
    "重新打开",
    "重新开启",
    "撤销完成",
    "撤回完成",
    "取消完成",
    "undo",
];

const NEGATED_RESTORE_MARKERS: &[&str] = &["不恢复", "不要恢复", "别恢复", "无需恢复"];

const UNFINISHED_CORRECTION_MARKERS: &[&str] = &[
    "还没做完",
    "还没有做完",
    "没做完",
    "没有做完",
    "还没完成",
    "还没有完成",
    "没完成",
    "没有完成",
    "并未完成",
];

const EXPLICIT_TODO_REFERENCE_MARKERS: &[&str] = &["刚才", "刚刚", "这条", "那条"];

const UNFINISHED_QUERY_MARKERS: &[&str] = &["查看", "看看", "查询", "哪些", "是否", "是不是", "吗"];

pub(crate) fn restore_tool_allowed(user_text: &str) -> bool {
    let normalized = user_text.trim().to_ascii_lowercase();
    let negated = NEGATED_RESTORE_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker));
    let explicit_marker = RESTORE_INTENT_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker));
    let undo_completion = ["撤销", "撤回", "取消"]
        .iter()
        .any(|marker| normalized.contains(marker))
        && normalized.contains("完成");
    // “刚才那条还没做完”没有显式的“恢复”动词，但在已完成对象上下文中表达的是
    // 状态纠正。必须同时出现明确指代，避免“看看还有哪些没做完”之类查询开放逆向工具。
    let unfinished_correction = UNFINISHED_CORRECTION_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
        && EXPLICIT_TODO_REFERENCE_MARKERS
            .iter()
            .any(|marker| normalized.contains(marker))
        && !UNFINISHED_QUERY_MARKERS
            .iter()
            .any(|marker| normalized.contains(marker));
    !negated && (explicit_marker || undo_completion || unfinished_correction)
}

pub(crate) fn enabled_tool_names_for_request<'a>(
    enabled_tools: &'a [String],
    user_text: &str,
) -> Vec<&'a str> {
    let restore_allowed = restore_tool_allowed(user_text);
    enabled_tools
        .iter()
        .filter(|name| name.as_str() != "restore_todos" || restore_allowed)
        .map(String::as_str)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::restore_tool_allowed;

    #[test]
    fn restore_tool_requires_explicit_restore_intent() {
        for text in ["完成待办", "把第一条标记完成", "完成它，然后列出待办"] {
            assert!(!restore_tool_allowed(text), "{text}");
        }
        for text in [
            "恢复第一条待办",
            "撤销完成",
            "撤销刚才的完成",
            "取消刚才的完成操作",
            "把它改回未完成",
            "刚才那条还没做完",
            "undo last todo",
        ] {
            assert!(restore_tool_allowed(text), "{text}");
        }
        for text in [
            "看看没做完的任务",
            "查看还没做完的任务",
            "哪些任务还没有完成",
            "第一条是不是还没完成？",
            "第一个版本还没完成",
        ] {
            assert!(!restore_tool_allowed(text), "{text}");
        }
        assert!(!restore_tool_allowed("不要恢复，继续完成第一条"));
    }

    #[test]
    fn completion_request_excludes_restore_tool_from_whitelist() {
        let enabled = vec!["complete_todos".to_owned(), "restore_todos".to_owned()];
        assert_eq!(
            super::enabled_tool_names_for_request(&enabled, "完成待办"),
            ["complete_todos"]
        );
        assert_eq!(
            super::enabled_tool_names_for_request(&enabled, "撤销刚才的完成"),
            ["complete_todos", "restore_todos"]
        );
    }
}

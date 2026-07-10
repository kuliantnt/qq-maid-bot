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
    !negated && (explicit_marker || undo_completion)
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
            "撤销刚才的完成",
            "把它改回未完成",
            "undo last todo",
        ] {
            assert!(restore_tool_allowed(text), "{text}");
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

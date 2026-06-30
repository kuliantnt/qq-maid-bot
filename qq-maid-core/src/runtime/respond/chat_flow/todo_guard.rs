//! 普通 Chat flow 的 `required Todo Tool` 守卫。
//!
//! 集中判断本轮普通聊天是否需要强制调用某个 Todo 写操作 Tool，避免模型不调 Tool
//! 却发"已生成草稿/已完成"等假成功文案；判定边界与 session 状态依赖在此封装。

use std::sync::OnceLock;

use regex::Regex;

use super::contains_any;
use crate::runtime::{respond::todo_flow::is_natural_todo_query_text, session::SessionRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TodoMutationToolKind {
    Create,
    Complete,
    Edit,
    Cancel,
    Restore,
    Delete,
}

impl TodoMutationToolKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Complete => "complete",
            Self::Edit => "edit",
            Self::Cancel => "cancel",
            Self::Restore => "restore",
            Self::Delete => "delete",
        }
    }

    fn required_tool_name(self) -> &'static str {
        match self {
            Self::Create => "create_todo",
            Self::Complete => "complete_todos",
            Self::Edit => "edit_todo",
            Self::Cancel => "cancel_todo",
            Self::Restore => "restore_todos",
            Self::Delete => "delete_todos",
        }
    }

    pub(super) fn matches_executed_tools(self, executed_tools: &[String]) -> bool {
        executed_tools
            .iter()
            .any(|tool| tool == self.required_tool_name())
    }
}

/// 判断本轮是否需要强制调用某个 Todo 写操作 Tool，防止模型不调 Tool 却发假成功文案。
///
/// 设计约束：
/// - 查询意图始终最优先排除，永返回 `None`。
/// - 创建意图单独识别，不依赖已有 session 状态，但必须同时存在明确创建动词和创建目标。
/// - 完成/取消/恢复/删除只在存在 Todo 目标上下文时才强制：
///   明确提到待办/任务/todo，或有编号引用（依赖 `last_todo_query`），
///   或有最近对象引用（依赖 `last_todo_action`）。
/// - 编号引用仅在 session 存在 `last_todo_query` 时才算目标；最近对象引用仅在
///   `last_todo_action` 时才算目标。这样可避免“完成这个项目/取消明天会议/删除服务器上的旧日志”
///   等普通聊天被误判成 Todo 写操作。
pub(super) fn required_todo_tool_kind(
    user_text: &str,
    session: &SessionRecord,
) -> Option<TodoMutationToolKind> {
    let text = user_text.trim();
    if text.is_empty() {
        return None;
    }
    // 查询意图始终最优先排除，不得进入 Todo 写操作受控重试。
    if looks_like_todo_query_text(text) {
        return None;
    }

    // 创建意图单独识别：不依赖已有 session 状态，但必须同时存在明确创建动词和创建目标，
    // 避免仅出现“待办”就误判为创建（如“待办功能怎么用”）。
    if let Some(kind) = detect_create_todo_kind(text) {
        return Some(kind);
    }

    // 完成/取消/恢复/删除必须同时存在 Todo 目标上下文，否则普通聊天里的
    // “完成项目/取消会议/删除日志”会被误强制成 Todo Tool，甚至真的修改用户待办。
    let has_todo_noun = contains_any(text, &["待办", "任务", "todo"]);
    let has_visible_reference =
        contains_visible_number_reference(text) && session.last_todo_query.is_some();
    let has_last_reference =
        contains_last_todo_reference(text) && session.last_todo_action.is_some();
    if !(has_todo_noun || has_visible_reference || has_last_reference) {
        return None;
    }

    // 检测写操作动词时屏蔽参照/状态子串，避免“已完成/刚恢复的那个”里的动词误被当成主操作。
    let operative = strip_todo_reference_and_status(text);
    // 顺序很关键：删除/恢复优先检测，避免“永久删除已完成待办”被“完成”抢先、“取消完成”（=恢复）被“取消”抢先。
    if contains_any(&operative, &["删除", "删掉", "移除", "永久删除"]) {
        return Some(TodoMutationToolKind::Delete);
    }
    if contains_any(&operative, &["恢复", "撤销完成", "恢复完成", "取消完成"]) {
        return Some(TodoMutationToolKind::Restore);
    }
    if contains_any(
        &operative,
        &["修改", "改成", "改为", "更新", "改一下", "改下"],
    ) {
        return Some(TodoMutationToolKind::Edit);
    }
    if contains_any(&operative, &["完成", "做完", "标记完成", "搞定"]) {
        return Some(TodoMutationToolKind::Complete);
    }
    if contains_any(&operative, &["取消", "不做了", "算了", "作废"]) {
        return Some(TodoMutationToolKind::Cancel);
    }
    None
}

/// 识别明确的待办创建意图。
///
/// 必须同时满足创建动词和创建目标：
/// - 创建动词：记一个 / 记个 / 帮我记 / 新增 / 添加 / 提醒我
/// - 创建目标：明确出现待办 / 任务，或“提醒我 + 具体事项”。
///
/// 例如“待办功能怎么用 / 这个待办是不是有 bug”只出现待办名词、没有创建动词，不会被误判为创建。
fn detect_create_todo_kind(text: &str) -> Option<TodoMutationToolKind> {
    let has_create_verb = contains_any(
        text,
        &["记一个", "记个", "帮我记", "新增", "添加", "提醒我"],
    );
    if !has_create_verb {
        return None;
    }
    let has_explicit_target = contains_any(text, &["待办", "任务"]);
    let has_reminder_target = text.contains("提醒我") && has_text_after_reminder(text);
    if has_explicit_target || has_reminder_target {
        Some(TodoMutationToolKind::Create)
    } else {
        None
    }
}

/// “提醒我 + 具体事项”判定：“提醒我”之后还有非空内容才算创建目标。
fn has_text_after_reminder(text: &str) -> bool {
    let Some((_, rest)) = text.split_once("提醒我") else {
        return false;
    };
    rest.trim().chars().count() >= 2
}

/// 检测明确的待办编号引用：“第 1 个 / 第一个 / 编号 2 / 第 3 条”等。
///
/// 只检测是否存在编号引用句式本身，不检测孤立数字，避免普通文本里出现数字就误强制 Todo Tool。
fn contains_visible_number_reference(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"第\s*[0-9一二三四五六七八九十]+\s*[个条项]|编号\s*[0-9]+")
            .expect("todo number reference regex")
    });
    re.is_match(text)
}

/// 检测最近待办对象引用：“刚才那个 / 刚恢复的那个 / 它 / 把它…”等。
///
/// 不包含裸“那个”（会误命中“那个项目我完成了”这类非 Todo 语句）；“那个待办”已通过 `has_todo_noun` 覆盖。
/// 是否算作目标还需要 `session.last_todo_action` 存在，该约束在 `required_todo_tool_kind` 中处理。
fn contains_last_todo_reference(text: &str) -> bool {
    contains_any(
        text,
        &[
            "刚才那个",
            "刚恢复的那个",
            "刚完成的那个",
            "刚取消的那个",
            "刚删除的那个",
            "刚创建的那个",
            "刚新建的那个",
            "把它",
            "它",
        ],
    )
}

/// 屏蔽参照与状态子串（“已完成 / 已取消 / 刚恢复的那个 / 第 N 个”），返回只保留主操作动词的文本。
///
/// 这些子串里的“完成 / 取消 / 恢复”是状态/参照描述，不是本轮主操作，检测写操作动词时需先移除。
fn strip_todo_reference_and_status(text: &str) -> String {
    let mut out = text.to_owned();
    for phrase in [
        "刚恢复的那个",
        "刚完成的那个",
        "刚取消的那个",
        "刚删除的那个",
        "刚创建的那个",
        "刚新建的那个",
        "刚才那个",
        "已完成的",
        "已取消的",
        "已完成",
        "已取消",
        "把它",
        "它",
    ] {
        out = out.replace(phrase, " ");
    }
    // 移除编号引用，避免“第 N 个”干扰动词检测。
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"第\s*[0-9一二三四五六七八九十]+\s*[个条项]|编号\s*[0-9]+")
            .expect("todo number reference strip regex")
    });
    out = re.replace_all(&out, " ").to_string();
    out
}

fn looks_like_todo_query_text(text: &str) -> bool {
    is_natural_todo_query_text(text)
}

pub(super) fn todo_required_tool_not_called_reply(
    required_tool_kind: Option<TodoMutationToolKind>,
) -> String {
    let action = match required_tool_kind {
        Some(TodoMutationToolKind::Create) => "新增待办",
        Some(TodoMutationToolKind::Complete) => "完成待办",
        Some(TodoMutationToolKind::Edit) => "修改待办",
        Some(TodoMutationToolKind::Cancel) => "取消待办",
        Some(TodoMutationToolKind::Restore) => "恢复待办",
        Some(TodoMutationToolKind::Delete) => "删除待办",
        None => "处理待办",
    };
    format!("我这次没有真正执行到{action}操作。请再说一次，我会先调用待办工具，再告诉你结果。")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::runtime::session::{LastTodoAction, LastTodoQuery, SessionRecord};
    use crate::runtime::todo::TodoStatus;

    use super::{TodoMutationToolKind, required_todo_tool_kind};

    /// 通过反序列化构造一个全默认字段的 session，便于隔离测试 Todo 意图判定。
    fn empty_session() -> SessionRecord {
        serde_json::from_value(json!({})).unwrap()
    }

    fn session_with_last_query() -> SessionRecord {
        let mut session = empty_session();
        session.last_todo_query = Some(LastTodoQuery {
            owner_key: "private:u1".to_owned(),
            query_type: "list".to_owned(),
            condition: String::new(),
            result_ids: vec!["item-1".to_owned()],
            created_at: "2026-06-30T00:00:00+08:00".to_owned(),
        });
        session
    }

    fn session_with_last_action() -> SessionRecord {
        let mut session = empty_session();
        session.last_todo_action = Some(LastTodoAction {
            owner_key: "private:u1".to_owned(),
            item_id: "item-1".to_owned(),
            title: "示例待办".to_owned(),
            action: "completed".to_owned(),
            resulting_status: TodoStatus::Completed,
            created_at: "2026-06-30T00:00:00+08:00".to_owned(),
        });
        session
    }

    fn assert_kind(text: &str, session: &SessionRecord, expected: Option<TodoMutationToolKind>) {
        assert_eq!(
            required_todo_tool_kind(text, session),
            expected,
            "text = {text:?}"
        );
    }

    // ----- 创建意图：不依赖 session 状态，但必须有明确创建动词和创建目标 -----

    #[test]
    fn create_intent_recognized_without_session_state() {
        let session = empty_session();
        assert_kind(
            "帮我记一个待办，今晚检查日志",
            &session,
            Some(TodoMutationToolKind::Create),
        );
        assert_kind(
            "新增一个任务，明天交报告",
            &session,
            Some(TodoMutationToolKind::Create),
        );
        assert_kind(
            "提醒我明天下午三点开会",
            &session,
            Some(TodoMutationToolKind::Create),
        );
    }

    #[test]
    fn create_intent_not_forced_for_todo_mentions_without_create_verb() {
        let session = empty_session();
        // 有待办但没有创建动词，不应被误判为创建。
        assert_kind("待办功能怎么用", &session, None);
        assert_kind("我们聊聊待办设计", &session, None);
        assert_kind("这个待办是不是有 bug", &session, None);
        assert_kind("为什么我的待办没显示", &session, None);
    }

    // ----- 非 Todo 普通聊天：不应强制任何 mutation Tool -----
    #[test]
    fn non_todo_chat_is_not_forced() {
        let session = empty_session();
        assert_kind("我终于完成这个项目了", &session, None);
        assert_kind("取消明天的会议", &session, None);
        assert_kind("删除服务器上的旧日志", &session, None);
        assert_kind("这个方案算了，不做了", &session, None);
        assert_kind("帮我恢复刚才删除的文档", &session, None);
    }

    // ----- 状态修改依赖 Todo 目标上下文：名词 / 编号 / 最近对象引用 -----

    #[test]
    fn mutation_with_todo_noun_recognized_without_session_state() {
        let session = empty_session();
        assert_kind(
            "完成第 1 个待办",
            &session,
            Some(TodoMutationToolKind::Complete),
        );
        assert_kind(
            "修改第 2 个待办",
            &session,
            Some(TodoMutationToolKind::Edit),
        );
        assert_kind(
            "取消第 2 个任务",
            &session,
            Some(TodoMutationToolKind::Cancel),
        );
        // “已完成”是状态描述，不应被“完成”抢先；主操作是删除。
        assert_kind(
            "永久删除已完成待办第 3 个",
            &session,
            Some(TodoMutationToolKind::Delete),
        );
    }

    #[test]
    fn mutation_with_number_reference_requires_last_todo_query() {
        // last_todo_query 存在时，编号引用算 Todo 目标。
        let session = session_with_last_query();
        assert_kind(
            "完成第 1 个",
            &session,
            Some(TodoMutationToolKind::Complete),
        );
        assert_kind("取消第 1 个", &session, Some(TodoMutationToolKind::Cancel));

        // last_todo_query 缺失时，裸编号引用不应强制 Tool。
        let session = empty_session();
        assert_kind("完成第 1 个", &session, None);
        assert_kind("删除第 2 个", &session, None);
    }

    #[test]
    fn mutation_with_last_reference_requires_last_todo_action() {
        // last_todo_action 存在时，“刚才那个 / 刚恢复的那个 / 它”算 Todo 目标。
        let session = session_with_last_action();
        assert_kind(
            "把刚才那个完成",
            &session,
            Some(TodoMutationToolKind::Complete),
        );
        assert_kind(
            "取消刚恢复的那个",
            &session,
            Some(TodoMutationToolKind::Cancel),
        );
        assert_kind("删除它", &session, Some(TodoMutationToolKind::Delete));
        assert_kind("把它取消掉", &session, Some(TodoMutationToolKind::Cancel));
        assert_kind(
            "删除刚取消的那个",
            &session,
            Some(TodoMutationToolKind::Delete),
        );

        // last_todo_action 缺失时，最近对象引用不应强制 Tool。
        let session = empty_session();
        assert_kind("删除它", &session, None);
        assert_kind("取消刚才那个", &session, None);
    }

    #[test]
    fn mutation_restore_phrases_override_complete_and_cancel() {
        let session = empty_session();
        // “取消完成 / 恢复完成”表示撤销完成（=恢复），不应被“取消 / 完成”抢先。
        assert_kind(
            "取消完成第 1 个待办",
            &session,
            Some(TodoMutationToolKind::Restore),
        );
        assert_kind(
            "恢复完成第 2 个任务",
            &session,
            Some(TodoMutationToolKind::Restore),
        );
    }

    // ----- 查询意图最优先排除，永返回 None -----

    #[test]
    fn query_intent_is_always_excluded() {
        let session = session_with_last_query();
        assert_kind("看看我的待办", &session, None);
        assert_kind("有哪些任务", &session, None);
        assert_kind("列出已完成的待办", &session, None);
        assert_kind("看看已完成", &session, None);
        assert_kind("看看已取消", &session, None);
        assert_kind("我的待办", &session, None);
        assert_kind("待办列表", &session, None);
    }
}

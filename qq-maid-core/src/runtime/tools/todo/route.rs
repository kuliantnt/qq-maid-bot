//! Todo 普通消息的高置信领域信号。
//!
//! 状态提示与 Todo 成功文案验真可以各自消费这里的强信号，但它不参与 Agent Runtime
//! 路由、Tool 暴露或执行，也不解析具体日期、编号目标和状态变更。真正的 owner、
//! 可见编号、pending、快照和写入不变量仍由 Todo Tool 与 flow 模块处理。

use qq_maid_common::time_context;

const TODO_OBJECT_MARKERS: &[&str] = &["待办", "代办", "任务", "提醒", "事项"];
const TODO_WRITE_MARKERS: &[&str] = &[
    "新增",
    "添加",
    "加个",
    "加一",
    "创建",
    "帮我记",
    "记一下",
    "记录",
    "提醒我",
    "别忘",
    "编辑",
    "修改",
    "改成",
];
const TODO_CONFIRM_MARKERS: &[&str] = &["完成", "做完", "恢复", "取消", "删除", "删掉", "移除"];
const TODO_QUERY_MARKERS: &[&str] = &["查看", "看一下", "列出", "有哪些", "检查"];
const TODO_DETAIL_MARKERS: &[&str] = &["详情", "备注", "内容", "说明", "正文"];
const TODO_DETAIL_CLEAR_MARKERS: &[&str] = &[
    "清除",
    "清空",
    "去掉",
    "移除",
    "删除",
    "删掉",
    "不要",
    "不需要",
];
const REMINDER_ACTION_MARKERS: &[&str] = &[
    "提醒我",
    "提醒一下",
    "提醒下",
    "帮我提醒",
    "回头提醒",
    "别忘",
    "别忘了",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoIntentKind {
    None,
    DirectIntent,
    StrongReference,
    ContextReference,
    NumberContext,
    ContextReferenceMissing,
    NumberContextMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoIntentAction {
    Confirm,
    Write,
    Query,
    Process,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TodoIntent {
    pub kind: TodoIntentKind,
}

impl TodoIntent {
    pub(crate) fn is_confident(self) -> bool {
        matches!(
            self.kind,
            TodoIntentKind::DirectIntent
                | TodoIntentKind::StrongReference
                | TodoIntentKind::ContextReference
                | TodoIntentKind::NumberContext
        )
    }
}

pub(crate) fn classify_todo_intent(
    text: &str,
    lower: &str,
    has_recent_todo_context: bool,
) -> TodoIntent {
    if has_todo_intent(text, lower) || has_reminder_intent(text) {
        return intent(TodoIntentKind::DirectIntent);
    }
    if is_strong_todo_reference_operation(text) {
        return intent(TodoIntentKind::StrongReference);
    }
    if is_weak_todo_context_reference(text) && has_recent_todo_context {
        return intent(TodoIntentKind::ContextReference);
    }
    if is_bare_number_todo_operation(text) {
        if has_recent_todo_context {
            return intent(TodoIntentKind::NumberContext);
        }
        return intent(TodoIntentKind::NumberContextMissing);
    }
    if is_weak_todo_context_reference(text) {
        return intent(TodoIntentKind::ContextReferenceMissing);
    }
    intent(TodoIntentKind::None)
}

pub(crate) fn todo_intent_action(text: &str) -> TodoIntentAction {
    if is_detail_clear_edit(text) {
        return TodoIntentAction::Write;
    }
    if contains_any(text, TODO_CONFIRM_MARKERS) {
        return TodoIntentAction::Confirm;
    }
    if contains_any(text, TODO_WRITE_MARKERS) {
        return TodoIntentAction::Write;
    }
    if contains_any(text, TODO_QUERY_MARKERS) {
        return TodoIntentAction::Query;
    }
    TodoIntentAction::Process
}

fn intent(kind: TodoIntentKind) -> TodoIntent {
    TodoIntent { kind }
}

fn has_todo_intent(text: &str, lower: &str) -> bool {
    if has_reminder_action(text) && !has_reminder_intent(text) {
        return false;
    }

    let has_todo_object =
        contains_any(text, TODO_OBJECT_MARKERS) || contains_ascii_word(lower, "todo");
    let has_todo_action = contains_any(text, TODO_WRITE_MARKERS)
        || contains_any(text, TODO_CONFIRM_MARKERS)
        || contains_any(text, TODO_QUERY_MARKERS);
    if has_todo_object && has_todo_action {
        return true;
    }

    if is_detail_clear_edit(text) {
        return true;
    }

    (contains_any(text, TODO_CONFIRM_MARKERS) || contains_any(text, &["编辑", "修改", "改成"]))
        && (has_ordinal_reference(text) || contains_any(text, &["它", "这个", "那个", "刚才那条"]))
}

fn is_detail_clear_edit(text: &str) -> bool {
    contains_any(text, TODO_DETAIL_MARKERS)
        && contains_any(text, TODO_DETAIL_CLEAR_MARKERS)
        && (has_ordinal_reference(text) || has_context_pronoun_reference(text))
}

fn has_reminder_intent(text: &str) -> bool {
    has_reminder_action(text)
        && (looks_like_temporal_expression(text) || has_reminder_payload(text))
}

fn has_reminder_action(text: &str) -> bool {
    contains_any(text, REMINDER_ACTION_MARKERS)
}

pub(super) fn looks_like_temporal_expression(text: &str) -> bool {
    // 路由层只判断“是否存在时间线索”，不消费推断出的日期，也不改变 Todo Tool
    // 内部基于模型/时间上下文生成的最终 due_date/due_at。
    let ctx = time_context::request_time_context();
    let compact = text.split_whitespace().collect::<String>();
    if time_context::infer_due_date_from_text(text, &ctx).is_some()
        || compact != text && time_context::infer_due_date_from_text(&compact, &ctx).is_some()
    {
        return true;
    }
    contains_any(
        text,
        &[
            "今晚",
            "明早",
            "明晚",
            "早上",
            "上午",
            "中午",
            "下午",
            "晚上",
            "凌晨",
            "傍晚",
            "回头",
            "月末",
            "下个月",
        ],
    )
}

fn has_reminder_payload(text: &str) -> bool {
    let mut payload = text.to_owned();
    for marker in REMINDER_ACTION_MARKERS {
        payload = payload.replace(marker, "");
    }
    // 只清掉明显的提示/时间壳，剩余内容仍交给 Todo Tool 解析和澄清。
    for filler in [
        "帮我",
        "请",
        "麻烦",
        "一下",
        "一下子",
        "到时候",
        "记得",
        "记着",
        "回头",
        "今天",
        "明天",
        "后天",
        "今晚",
        "明早",
        "明晚",
        "早上",
        "上午",
        "中午",
        "下午",
        "晚上",
        "凌晨",
        "傍晚",
        "月底",
        "月末",
        "下个月初",
        "下个月",
        "周一",
        "周二",
        "周三",
        "周四",
        "周五",
        "周六",
        "周日",
        "星期一",
        "星期二",
        "星期三",
        "星期四",
        "星期五",
        "星期六",
        "星期日",
    ] {
        payload = payload.replace(filler, "");
    }
    let meaningful = payload.trim_matches(|ch: char| {
        ch.is_whitespace() || ch.is_ascii_punctuation() || is_cjk_punctuation(ch)
    });
    meaningful.chars().count() >= 2
}

fn is_strong_todo_reference_operation(text: &str) -> bool {
    let has_reference = has_ordinal_reference(text) || has_context_pronoun_reference(text);
    if !has_reference {
        return false;
    }

    let has_lifecycle_action = contains_any(text, TODO_CONFIRM_MARKERS);
    let has_numbered_edit_or_process =
        has_ordinal_reference(text) && contains_any(text, &["处理", "改一下", "修改", "编辑"]);
    has_lifecycle_action || has_numbered_edit_or_process
}

fn is_weak_todo_context_reference(text: &str) -> bool {
    (has_context_pronoun_reference(text)
        && contains_any(text, &["处理", "改一下", "修改", "编辑", "改成"]))
        || is_bulk_todo_context_reference(text)
}

fn is_bulk_todo_context_reference(text: &str) -> bool {
    contains_any(text, &["都", "全部", "全"]) && contains_any(text, TODO_CONFIRM_MARKERS)
}

fn is_bare_number_todo_operation(text: &str) -> bool {
    let compact = text.split_whitespace().collect::<String>();
    has_ascii_digit(&compact)
        && (contains_any(&compact, TODO_CONFIRM_MARKERS)
            || contains_any(&compact, &["删", "清掉", "作废", "合并"]))
}

fn has_ascii_digit(text: &str) -> bool {
    text.bytes().any(|byte| byte.is_ascii_digit())
}

fn contains_ascii_word(text: &str, expected: &str) -> bool {
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .any(|word| word == expected)
}

fn has_ordinal_reference(text: &str) -> bool {
    contains_any(
        text,
        &[
            "第一", "第二", "第三", "第四", "第五", "第六", "第七", "第八", "第九", "第十", "第 1",
            "第 2", "第 3", "第 4", "第 5", "第 6", "第 7", "第 8", "第 9", "第1", "第2", "第3",
            "第4", "第5", "第6", "第7", "第8", "第9",
        ],
    )
}

fn has_context_pronoun_reference(text: &str) -> bool {
    contains_any(
        text,
        &[
            "它",
            "这个",
            "那个",
            "这条",
            "那条",
            "这些",
            "它们",
            "刚才那条",
            "刚刚那条",
            "刚才那个",
            "刚刚那个",
        ],
    )
}

fn is_cjk_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。'
            | '、'
            | '：'
            | '；'
            | '？'
            | '！'
            | '（'
            | '）'
            | '【'
            | '】'
            | '《'
            | '》'
            | '“'
            | '”'
            | '‘'
            | '’'
    )
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(text: &str, has_recent_todo_context: bool) -> TodoIntent {
        classify_todo_intent(text, &text.to_ascii_lowercase(), has_recent_todo_context)
    }

    #[test]
    fn explicit_todo_operations_are_high_confidence() {
        for (input, action) in [
            ("新增待办，明天接人", TodoIntentAction::Write),
            ("帮我记一个待办，今晚检查日志", TodoIntentAction::Write),
            ("明天提醒我交水电费", TodoIntentAction::Write),
            ("查看待办", TodoIntentAction::Query),
            ("完成第一条", TodoIntentAction::Confirm),
            ("删除第 7 条", TodoIntentAction::Confirm),
        ] {
            assert!(classify(input, false).is_confident(), "{input}");
            assert_eq!(todo_intent_action(input), action, "{input}");
        }
    }

    #[test]
    fn weak_references_require_recent_todo_context() {
        for input in ["这个改一下", "删除7", "把7合并到6"] {
            assert!(classify(input, true).is_confident(), "{input}");
            assert!(!classify(input, false).is_confident(), "{input}");
        }
    }

    #[test]
    fn time_expressions_do_not_create_todo_status_without_todo_signal() {
        for input in [
            "明天陪我聊聊",
            "晚上分析一下架构",
            "下午写一段文案",
            "明天解释这段日志",
            "明天开会",
            "下午整理一下",
        ] {
            assert!(!classify(input, false).is_confident(), "{input}");
        }
    }

    #[test]
    fn todo_ascii_marker_matches_a_word_instead_of_a_substring() {
        assert!(classify("查看 TODO", false).is_confident());
        assert!(!classify("查看 TodoMVC 示例", false).is_confident());
    }
}

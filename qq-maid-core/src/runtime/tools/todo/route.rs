//! Todo 普通消息进入 Tool Loop 前的轻量路由判定。
//!
//! 这里只判断“是否明显是 Todo / Reminder 任务”，不解析具体日期、编号目标或执行
//! 状态变更。真正的 owner、可见编号、pending、快照和写入不变量仍由 Todo Tool 与
//! flow 模块处理。

use qq_maid_common::time_context;

const TODO_OBJECT_MARKERS: &[&str] = &["待办", "代办", "任务", "提醒", "事项"];
const TODO_WRITE_MARKERS: &[&str] = &[
    "新增",
    "添加",
    "加个",
    "加一",
    "创建",
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
pub(crate) enum TodoRouteKind {
    None,
    DirectIntent,
    StrongReference,
    ContextReference,
    NumberContext,
    ContextReferenceMissing,
    NumberContextMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoRouteAction {
    Confirm,
    Write,
    Query,
    Process,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TodoRouteIntent {
    pub kind: TodoRouteKind,
    pub reason: &'static str,
}

impl TodoRouteIntent {
    pub(crate) fn routes_to_tool_loop(self) -> bool {
        matches!(
            self.kind,
            TodoRouteKind::DirectIntent
                | TodoRouteKind::StrongReference
                | TodoRouteKind::ContextReference
                | TodoRouteKind::NumberContext
        )
    }
}

pub(crate) fn classify_todo_route(
    text: &str,
    lower: &str,
    has_recent_todo_context: bool,
    plain_chat_candidate: bool,
) -> TodoRouteIntent {
    if has_todo_intent(text, lower)
        || has_reminder_intent(text)
        || has_scheduled_task_intent(text, plain_chat_candidate)
    {
        return intent(TodoRouteKind::DirectIntent, "semantic_tool_intent");
    }
    if is_strong_todo_reference_operation(text) {
        return intent(TodoRouteKind::StrongReference, "todo_reference_strong");
    }
    if is_weak_todo_context_reference(text) && has_recent_todo_context {
        return intent(TodoRouteKind::ContextReference, "todo_reference_context");
    }
    if is_bare_number_todo_operation(text) {
        if has_recent_todo_context {
            return intent(TodoRouteKind::NumberContext, "todo_number_context");
        }
        return intent(
            TodoRouteKind::NumberContextMissing,
            "todo_number_context_missing",
        );
    }
    if is_weak_todo_context_reference(text) {
        return intent(
            TodoRouteKind::ContextReferenceMissing,
            "todo_reference_context_missing",
        );
    }
    intent(TodoRouteKind::None, "todo_no_intent")
}

pub(crate) fn todo_route_action(text: &str) -> TodoRouteAction {
    if contains_any(text, TODO_CONFIRM_MARKERS) {
        return TodoRouteAction::Confirm;
    }
    if contains_any(text, TODO_WRITE_MARKERS) {
        return TodoRouteAction::Write;
    }
    if contains_any(text, TODO_QUERY_MARKERS) {
        return TodoRouteAction::Query;
    }
    TodoRouteAction::Process
}

pub(crate) fn routes_as_todo_write_status(text: &str, plain_chat_candidate: bool) -> bool {
    has_reminder_intent(text) || has_scheduled_task_intent(text, plain_chat_candidate)
}

fn intent(kind: TodoRouteKind, reason: &'static str) -> TodoRouteIntent {
    TodoRouteIntent { kind, reason }
}

fn has_todo_intent(text: &str, lower: &str) -> bool {
    if has_reminder_action(text) && !has_reminder_intent(text) {
        return false;
    }

    let has_todo_object = contains_any(text, TODO_OBJECT_MARKERS) || lower.contains("todo");
    let has_todo_action = contains_any(text, TODO_WRITE_MARKERS)
        || contains_any(text, TODO_CONFIRM_MARKERS)
        || contains_any(text, TODO_QUERY_MARKERS);
    if has_todo_object && has_todo_action {
        return true;
    }

    (contains_any(text, TODO_CONFIRM_MARKERS) || contains_any(text, &["编辑", "修改", "改成"]))
        && (has_ordinal_reference(text) || contains_any(text, &["它", "这个", "那个", "刚才那条"]))
}

fn has_reminder_intent(text: &str) -> bool {
    has_reminder_action(text)
        && (looks_like_temporal_expression(text) || has_reminder_payload(text))
}

fn has_reminder_action(text: &str) -> bool {
    contains_any(text, REMINDER_ACTION_MARKERS)
}

fn has_scheduled_task_intent(text: &str, plain_chat_candidate: bool) -> bool {
    if plain_chat_candidate || !looks_like_temporal_expression(text) {
        return false;
    }
    let compact = text.split_whitespace().collect::<String>();
    let action_markers = [
        "盯一下",
        "盯下",
        "看一下",
        "看下",
        "检查",
        "跟进",
        "开会",
        "提交",
        "出一版",
        "复盘",
        "续费",
        "验收",
        "整理",
        "发送",
        "发给",
        "发一下",
        "发布",
        "发版",
        "交水电费",
        "交作业",
        "买菜",
        "买东西",
        "买药",
        "完成初稿",
        "完成草稿",
    ];
    contains_any(&compact, &action_markers)
}

fn looks_like_temporal_expression(text: &str) -> bool {
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

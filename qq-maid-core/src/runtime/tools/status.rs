//! 用户可见处理状态文案映射。
//!
//! 这里只消费已经确定的会话类型、工具类别和动作类别，不参与是否进入 Agent Chat
//! 的路由判断，避免状态提示反向影响业务执行边界。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusAudience {
    Private,
    Group,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusSubject {
    Model,
    Search,
    Weather,
    Todo,
    Rss,
    Train,
    Record,
}

impl StatusSubject {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Search => "search",
            Self::Weather => "weather",
            Self::Todo => "todo",
            Self::Rss => "rss",
            Self::Train => "train",
            Self::Record => "record",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusAction {
    Think,
    Process,
    Query,
    Write,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusPhase {
    Started,
    Running,
    Finalizing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StatusHint {
    pub subject: StatusSubject,
    pub action: StatusAction,
}

impl StatusHint {
    pub(crate) const fn new(subject: StatusSubject, action: StatusAction) -> Self {
        Self { subject, action }
    }

    pub(crate) const fn model() -> Self {
        Self::new(StatusSubject::Model, StatusAction::Think)
    }

    pub(crate) const fn processing() -> Self {
        Self::new(StatusSubject::Model, StatusAction::Process)
    }
}

/// 真实 Tool Call 已开始后，根据结构化工具名选择用户可见状态。
///
/// 这里不读取用户原文，也不参与 Tool Registry、权限或执行判断；未知工具统一回退到
/// 通用处理状态，避免为状态提示再维护一套自然语言意图分类器。
pub(crate) fn status_hint_for_tool_name(tool_name: &str) -> Option<StatusHint> {
    let hint = match tool_name {
        "web_search" | "knowledge_search" => {
            StatusHint::new(StatusSubject::Search, StatusAction::Query)
        }
        "get_weather" => StatusHint::new(StatusSubject::Weather, StatusAction::Query),
        "get_train_schedule" => StatusHint::new(StatusSubject::Train, StatusAction::Query),
        "get_rss_recent_items" => StatusHint::new(StatusSubject::Rss, StatusAction::Query),
        "manage_rss_subscriptions" => StatusHint::new(StatusSubject::Rss, StatusAction::Process),
        "list_todos" | "get_todo" => StatusHint::new(StatusSubject::Todo, StatusAction::Query),
        "create_todo" | "edit_todo" | "merge_todos" | "manage_recurring_reminder" => {
            StatusHint::new(StatusSubject::Todo, StatusAction::Write)
        }
        "complete_todos" | "restore_todos" | "delete_todos" => {
            StatusHint::new(StatusSubject::Todo, StatusAction::Confirm)
        }
        "clarification_control" => StatusHint::new(StatusSubject::Todo, StatusAction::Process),
        "save_memory" => StatusHint::new(StatusSubject::Record, StatusAction::Write),
        _ => return None,
    };
    Some(hint)
}

pub(crate) fn status_hint_text(
    audience: StatusAudience,
    hint: StatusHint,
    phase: StatusPhase,
    private_display_name: &str,
) -> String {
    match phase {
        StatusPhase::Started => started_status_text(audience, hint, private_display_name),
        StatusPhase::Running => running_status_text(audience, private_display_name),
        StatusPhase::Finalizing => finalizing_status_text(audience, private_display_name),
    }
}

fn started_status_text(
    audience: StatusAudience,
    hint: StatusHint,
    private_display_name: &str,
) -> String {
    match audience {
        StatusAudience::Private => private_started_status_text(hint, private_display_name),
        StatusAudience::Group => group_started_status_text(hint).to_owned(),
    }
}

fn private_started_status_text(hint: StatusHint, display_name: &str) -> String {
    let action = match (hint.subject, hint.action) {
        (StatusSubject::Model, StatusAction::Think) => "正在想一下…",
        (StatusSubject::Weather, StatusAction::Query) => "正在查天气…",
        (StatusSubject::Search, StatusAction::Query) => "正在查资料…",
        (StatusSubject::Todo, StatusAction::Query) => "正在翻待办…",
        (StatusSubject::Todo, StatusAction::Write) => "正在记下来…",
        (StatusSubject::Todo, StatusAction::Confirm) => "正在确认处理…",
        (StatusSubject::Rss, _) => "正在看订阅…",
        (StatusSubject::Train, StatusAction::Query) => "正在查车次…",
        (StatusSubject::Record, StatusAction::Write) => "正在记下来…",
        _ => "正在处理…",
    };
    format!("{display_name}{action}")
}

fn group_started_status_text(hint: StatusHint) -> &'static str {
    match hint.action {
        StatusAction::Think => "正在想…",
        StatusAction::Query => "正在查…",
        StatusAction::Write => "正在记…",
        StatusAction::Confirm => "正在确认…",
        StatusAction::Process => "处理中…",
    }
}

fn running_status_text(audience: StatusAudience, private_display_name: &str) -> String {
    match audience {
        StatusAudience::Private => format!("{private_display_name}正在处理…"),
        StatusAudience::Group => "处理中…".to_owned(),
    }
}

fn finalizing_status_text(audience: StatusAudience, private_display_name: &str) -> String {
    match audience {
        StatusAudience::Private => format!("{private_display_name}正在确认结果…"),
        StatusAudience::Group => "正在确认…".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_public_text(text: &str) {
        for internal in [
            "正在调用工具",
            "调用工具",
            "Tool Loop",
            "tool loop",
            "tool call",
            "function call",
            "agent loop",
            "routing",
            "route",
        ] {
            assert!(!text.contains(internal), "{text} contains {internal}");
        }
    }

    #[test]
    fn private_status_hint_maps_tool_subjects_to_natural_text() {
        let cases = [
            (
                StatusHint::model(),
                StatusPhase::Started,
                "小女仆正在想一下…",
            ),
            (
                StatusHint::new(StatusSubject::Search, StatusAction::Process),
                StatusPhase::Started,
                "小女仆正在处理…",
            ),
            (
                StatusHint::new(StatusSubject::Weather, StatusAction::Query),
                StatusPhase::Started,
                "小女仆正在查天气…",
            ),
            (
                StatusHint::new(StatusSubject::Todo, StatusAction::Query),
                StatusPhase::Started,
                "小女仆正在翻待办…",
            ),
            (
                StatusHint::new(StatusSubject::Todo, StatusAction::Write),
                StatusPhase::Started,
                "小女仆正在记下来…",
            ),
            (
                StatusHint::new(StatusSubject::Todo, StatusAction::Confirm),
                StatusPhase::Started,
                "小女仆正在确认处理…",
            ),
            (
                StatusHint::new(StatusSubject::Rss, StatusAction::Query),
                StatusPhase::Started,
                "小女仆正在看订阅…",
            ),
            (
                StatusHint::new(StatusSubject::Train, StatusAction::Query),
                StatusPhase::Started,
                "小女仆正在查车次…",
            ),
            (
                StatusHint::new(StatusSubject::Search, StatusAction::Process),
                StatusPhase::Finalizing,
                "小女仆正在确认结果…",
            ),
        ];

        for (hint, phase, expected) in cases {
            let text = status_hint_text(StatusAudience::Private, hint, phase, "小女仆");
            assert_eq!(text, expected);
            assert_public_text(&text);
        }
    }

    #[test]
    fn group_status_hint_uses_short_text_by_action() {
        let cases = [
            (StatusHint::model(), StatusPhase::Started, "正在想…"),
            (
                StatusHint::new(StatusSubject::Weather, StatusAction::Query),
                StatusPhase::Started,
                "正在查…",
            ),
            (
                StatusHint::new(StatusSubject::Todo, StatusAction::Write),
                StatusPhase::Started,
                "正在记…",
            ),
            (
                StatusHint::new(StatusSubject::Todo, StatusAction::Confirm),
                StatusPhase::Started,
                "正在确认…",
            ),
            (
                StatusHint::new(StatusSubject::Search, StatusAction::Process),
                StatusPhase::Running,
                "处理中…",
            ),
            (
                StatusHint::new(StatusSubject::Search, StatusAction::Process),
                StatusPhase::Finalizing,
                "正在确认…",
            ),
        ];

        for (hint, phase, expected) in cases {
            let text = status_hint_text(StatusAudience::Group, hint, phase, "不会显示");
            assert_eq!(text, expected);
            assert_public_text(&text);
        }
    }

    #[test]
    fn private_status_hint_uses_configured_display_name() {
        let text = status_hint_text(
            StatusAudience::Private,
            StatusHint::new(StatusSubject::Weather, StatusAction::Query),
            StatusPhase::Started,
            "助手",
        );

        assert_eq!(text, "助手正在查天气…");
        assert_public_text(&text);
    }

    #[test]
    fn structured_tool_names_map_to_domain_statuses() {
        let cases = [
            (
                "web_search",
                StatusHint::new(StatusSubject::Search, StatusAction::Query),
            ),
            (
                "get_weather",
                StatusHint::new(StatusSubject::Weather, StatusAction::Query),
            ),
            (
                "list_todos",
                StatusHint::new(StatusSubject::Todo, StatusAction::Query),
            ),
            (
                "create_todo",
                StatusHint::new(StatusSubject::Todo, StatusAction::Write),
            ),
            (
                "delete_todos",
                StatusHint::new(StatusSubject::Todo, StatusAction::Confirm),
            ),
            (
                "save_memory",
                StatusHint::new(StatusSubject::Record, StatusAction::Write),
            ),
        ];

        for (tool_name, expected) in cases {
            assert_eq!(status_hint_for_tool_name(tool_name), Some(expected));
        }
        assert_eq!(status_hint_for_tool_name("future_tool"), None);
    }
}

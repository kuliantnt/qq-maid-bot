//! 业务工具的状态提示分类。
//!
//! 这些轻量规则只用于用户可见状态，不参与工具暴露或执行决策。

use super::{status::StatusHint, todo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractionDomain {
    Todo,
    #[cfg(test)]
    NonTodoForTest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InteractionDomainState {
    pub domain: InteractionDomain,
    pub has_visible_snapshot: bool,
    pub has_recent_operation: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InteractionStateSnapshot {
    domains: Vec<InteractionDomainState>,
}

impl InteractionStateSnapshot {
    pub(crate) fn from_domains(domains: Vec<InteractionDomainState>) -> Self {
        Self { domains }
    }

    pub(crate) fn has_recent_context(&self, domain: InteractionDomain) -> bool {
        self.domains.iter().any(|state| {
            state.domain == domain && (state.has_visible_snapshot || state.has_recent_operation)
        })
    }

    #[cfg(test)]
    pub(crate) fn with_recent_todo_context_for_test() -> Self {
        Self::from_domains(vec![InteractionDomainState {
            domain: InteractionDomain::Todo,
            has_visible_snapshot: true,
            has_recent_operation: false,
        }])
    }

    #[cfg(test)]
    pub(crate) fn with_recent_non_todo_context_for_test() -> Self {
        Self::from_domains(vec![InteractionDomainState {
            domain: InteractionDomain::NonTodoForTest,
            has_visible_snapshot: true,
            has_recent_operation: false,
        }])
    }
}

pub(crate) fn classify_status_hint(
    text: &str,
    interaction_state: &InteractionStateSnapshot,
) -> Option<StatusHint> {
    let has_recent_todo_context = interaction_state.has_recent_context(InteractionDomain::Todo);
    if let Some(hint) = todo::status::classify_status_hint(text, has_recent_todo_context) {
        return Some(hint);
    }
    for classify in [
        super::memory::status::classify_status_hint,
        super::weather::status::classify_status_hint,
        super::train::status::classify_status_hint,
    ] {
        if let Some(hint) = classify(text) {
            return Some(hint);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::runtime::tools::status::{StatusAction, StatusSubject};

    use super::*;

    #[test]
    fn todo_weak_references_only_use_todo_domain_context() {
        let todo_context = InteractionStateSnapshot::with_recent_todo_context_for_test();
        let no_context = InteractionStateSnapshot::default();
        let non_todo_context = InteractionStateSnapshot::with_recent_non_todo_context_for_test();

        for input in ["这个改一下", "删除7", "把7合并到6"] {
            assert!(
                classify_status_hint(input, &todo_context)
                    .is_some_and(|hint| hint.subject == StatusSubject::Todo),
                "Todo 最近上下文应触发 Todo 状态：{input}"
            );
            assert_eq!(classify_status_hint(input, &no_context), None, "{input}");
            assert_eq!(
                classify_status_hint(input, &non_todo_context),
                None,
                "非 Todo domain 不得等同于 Todo 最近上下文：{input}"
            );
        }
    }

    #[test]
    fn explicit_status_semantics_do_not_depend_on_recent_context() {
        let context = InteractionStateSnapshot::default();
        let cases = [
            (
                "杭州明天要带伞吗",
                StatusHint::new(StatusSubject::Weather, StatusAction::Query),
            ),
            (
                "新增待办，明天接人",
                StatusHint::new(StatusSubject::Todo, StatusAction::Write),
            ),
            (
                "完成第一条",
                StatusHint::new(StatusSubject::Todo, StatusAction::Confirm),
            ),
            (
                "记住我喜欢简短回复",
                StatusHint::new(StatusSubject::Record, StatusAction::Write),
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(
                classify_status_hint(input, &context),
                Some(expected),
                "{input}"
            );
        }
    }

    #[test]
    fn search_and_rss_text_wait_for_structured_tool_events() {
        let context = InteractionStateSnapshot::default();

        for input in [
            "联网查一下今天 AI 新闻",
            "这个项目的最新进展",
            "GitHub Actions 怎么配置",
            "看看 RSS 最近更新",
        ] {
            assert_eq!(classify_status_hint(input, &context), None, "{input}");
        }
    }
}

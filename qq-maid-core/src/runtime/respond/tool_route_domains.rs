//! 非 Todo 工具路由聚合。
//!
//! respond 层只在这里按固定顺序调用各 domain route provider，并把 domain 判断映射成
//! 通用 `SemanticAssessment` / `StatusHint`。具体关键词和轻量意图规则放在对应业务模块。

use crate::runtime::{
    memory,
    tools::{rss, search, train, weather},
};

use super::{
    plain_chat_route,
    status_hint::{StatusAction, StatusHint, StatusSubject},
    tool_route::{SemanticAssessment, SemanticRoute, ToolDomain, assessment},
};

pub(super) fn classify_non_todo_route(text: &str, lower: &str) -> Option<SemanticAssessment> {
    if memory::route::has_memory_intent(text, lower) {
        return Some(tool_assessment(ToolDomain::Memory));
    }
    if weather::route::has_weather_intent(text, lower) {
        return Some(tool_assessment(ToolDomain::Weather));
    }
    if train::route::has_train_intent(text, lower) {
        return Some(tool_assessment(ToolDomain::Train));
    }
    if rss::route::has_rss_intent(text, lower) {
        return Some(tool_assessment(ToolDomain::Rss));
    }
    if has_search_intent(text, lower) {
        return Some(tool_assessment(ToolDomain::Search));
    }
    None
}

pub(super) fn classify_non_todo_status_hint(text: &str, lower: &str) -> Option<StatusHint> {
    if memory::route::has_memory_intent(text, lower) {
        return Some(StatusHint::new(StatusSubject::Record, StatusAction::Read));
    }
    if weather::route::has_weather_intent(text, lower) {
        return Some(StatusHint::new(StatusSubject::Weather, StatusAction::Query));
    }
    if train::route::has_train_intent(text, lower) {
        return Some(StatusHint::new(StatusSubject::Train, StatusAction::Query));
    }
    if rss::route::has_rss_intent(text, lower) {
        return Some(StatusHint::new(StatusSubject::Rss, StatusAction::Query));
    }
    if has_search_intent(text, lower) {
        return Some(StatusHint::new(StatusSubject::Tool, StatusAction::Query));
    }
    None
}

pub(super) fn mentions_inert_weather_topic(text: &str) -> bool {
    weather::route::mentions_inert_weather_topic(text)
}

pub(super) fn has_search_intent(text: &str, lower: &str) -> bool {
    search::route::has_search_intent(
        text,
        lower,
        plain_chat_route::has_local_text_processing_intent(text, lower),
    )
}

fn tool_assessment(domain: ToolDomain) -> SemanticAssessment {
    assessment(SemanticRoute::ToolLoop, domain, "semantic_tool_intent")
}

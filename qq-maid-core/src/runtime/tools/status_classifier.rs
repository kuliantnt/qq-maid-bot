//! 业务工具的状态提示分类。
//!
//! 这些轻量规则只用于用户可见状态，不参与工具暴露或执行决策。

use crate::runtime::memory;

use super::{
    rss, search,
    status::{StatusAction, StatusHint, StatusSubject},
    status_semantics, todo,
    todo::route::TodoRouteAction,
    train, weather,
};

pub(crate) fn classify_status_hint(
    text: &str,
    has_recent_todo_context: bool,
) -> Option<StatusHint> {
    let lower = text.to_ascii_lowercase();
    let non_tool_context = status_semantics::has_non_tool_status_context(text, &lower);
    let todo_intent =
        todo::route::classify_todo_route(text, &lower, has_recent_todo_context, non_tool_context);
    if todo_intent.routes_to_tool_loop() {
        let action = if todo::route::routes_as_todo_write_status(text, non_tool_context) {
            StatusAction::Write
        } else {
            match todo::route::todo_route_action(text) {
                TodoRouteAction::Confirm => StatusAction::Confirm,
                TodoRouteAction::Write => StatusAction::Write,
                TodoRouteAction::Query => StatusAction::Query,
                TodoRouteAction::Process => StatusAction::Process,
            }
        };
        return Some(StatusHint::new(StatusSubject::Todo, action));
    }
    if memory::route::has_memory_intent(text, &lower) {
        return Some(StatusHint::new(StatusSubject::Record, StatusAction::Read));
    }
    if weather::route::has_weather_intent(text, &lower) {
        return Some(StatusHint::new(StatusSubject::Weather, StatusAction::Query));
    }
    if train::route::has_train_intent(text, &lower) {
        return Some(StatusHint::new(StatusSubject::Train, StatusAction::Query));
    }
    if rss::route::has_rss_intent(text, &lower) {
        return Some(StatusHint::new(StatusSubject::Rss, StatusAction::Query));
    }
    if has_search_intent(text, &lower) {
        return Some(StatusHint::new(StatusSubject::Search, StatusAction::Query));
    }
    None
}

pub(crate) fn has_search_intent(text: &str, lower: &str) -> bool {
    search::route::has_search_intent(
        text,
        lower,
        status_semantics::has_local_text_processing_intent(text, lower),
    )
}

//! 天气领域的用户可见状态适配。

use crate::runtime::tools::status::{StatusAction, StatusHint, StatusSubject};

pub(crate) fn status_hint_for_tool_name(tool_name: &str) -> Option<StatusHint> {
    (tool_name == super::WEATHER_TOOL_NAME)
        .then_some(StatusHint::new(StatusSubject::Weather, StatusAction::Query))
}

pub(crate) fn classify_status_hint(text: &str) -> Option<StatusHint> {
    super::route::has_weather_status_intent(text)
        .then_some(StatusHint::new(StatusSubject::Weather, StatusAction::Query))
}

//! 列车查询领域的用户可见状态适配。

use crate::runtime::tools::status::{StatusAction, StatusHint, StatusSubject};

pub(crate) fn status_hint_for_tool_name(tool_name: &str) -> Option<StatusHint> {
    (tool_name == "get_train_schedule")
        .then_some(StatusHint::new(StatusSubject::Train, StatusAction::Query))
}

pub(crate) fn classify_status_hint(text: &str) -> Option<StatusHint> {
    super::route::has_train_status_intent(text)
        .then_some(StatusHint::new(StatusSubject::Train, StatusAction::Query))
}

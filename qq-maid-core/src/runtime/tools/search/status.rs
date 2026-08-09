//! 联网搜索领域的用户可见状态适配。

use crate::runtime::tools::status::{StatusAction, StatusHint, StatusSubject};

pub(crate) fn status_hint_for_tool_name(tool_name: &str) -> Option<StatusHint> {
    (tool_name == super::WEB_SEARCH_TOOL_NAME)
        .then_some(StatusHint::new(StatusSubject::Search, StatusAction::Query))
}

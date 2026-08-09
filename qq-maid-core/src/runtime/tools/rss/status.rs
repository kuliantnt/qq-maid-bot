//! RSS 领域的用户可见状态适配。

use crate::runtime::tools::status::{StatusAction, StatusHint, StatusSubject};

pub(crate) fn status_hint_for_tool_name(tool_name: &str) -> Option<StatusHint> {
    let action = match tool_name {
        "get_rss_recent_items" => StatusAction::Query,
        "manage_rss_subscriptions" => StatusAction::Process,
        _ => return None,
    };
    Some(StatusHint::new(StatusSubject::Rss, action))
}

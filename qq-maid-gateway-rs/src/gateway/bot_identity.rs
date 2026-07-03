//! Gateway 侧机器人身份匹配。
//!
//! QQ 普通群消息里的 mention 目标不一定等于 AppID；运行时从 READY 事件里学习
//! 当前机器人可比对的稳定 ID，并与配置中的兜底 ID 一起用于群消息 @ 判定。

use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
};

use serde_json::Value;
use tracing::info;

#[derive(Debug)]
pub(crate) struct BotIdentity {
    ids: RwLock<HashSet<String>>,
}

pub(crate) type SharedBotIdentity = Arc<BotIdentity>;

impl BotIdentity {
    pub(crate) fn new(app_id: &str, configured_ids: &[String]) -> Self {
        let mut ids = HashSet::new();
        insert_id(&mut ids, app_id);
        for id in configured_ids {
            insert_id(&mut ids, id);
        }
        Self {
            ids: RwLock::new(ids),
        }
    }

    pub(crate) fn contains(&self, value: &str) -> bool {
        let value = value.trim();
        !value.is_empty() && self.ids.read().unwrap().contains(value)
    }

    pub(crate) fn absorb_ready_payload(&self, payload: &Value) {
        let mut learned = HashSet::new();
        collect_ready_identity_candidates(payload, &mut learned);
        if learned.is_empty() {
            return;
        }

        let mut ids = self.ids.write().unwrap();
        let before = ids.len();
        ids.extend(learned);
        let added = ids.len().saturating_sub(before);
        if added > 0 {
            info!(
                learned_bot_identity_count = added,
                total_bot_identity_count = ids.len(),
                "learned QQ bot identity candidates from READY"
            );
        }
    }
}

fn insert_id(ids: &mut HashSet<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        ids.insert(value.to_owned());
    }
}

fn collect_ready_identity_candidates(payload: &Value, output: &mut HashSet<String>) {
    for key in ["user", "bot", "self", "application", "bot_info"] {
        if let Some(value) = payload.get(key) {
            collect_identity_object(value, output);
        }
    }
}

fn collect_identity_object(value: &Value, output: &mut HashSet<String>) {
    match value {
        Value::Object(map) => {
            for key in ["id", "openid", "user_openid", "member_openid", "bot_openid"] {
                if let Some(id) = map.get(key).and_then(Value::as_str) {
                    insert_id(output, id);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_identity_object(item, output);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identity_includes_configured_app_and_extra_ids() {
        let identity = BotIdentity::new("appid", &["bot-openid".to_owned()]);

        assert!(identity.contains("appid"));
        assert!(identity.contains("bot-openid"));
        assert!(!identity.contains("other"));
    }

    #[test]
    fn identity_learns_ready_candidates_without_session_id() {
        let identity = BotIdentity::new("appid", &[]);
        identity.absorb_ready_payload(&json!({
            "session_id": "session-should-not-match",
            "user": {"id": "bot-id", "openid": "bot-openid"},
            "application": {"id": "app-from-ready"}
        }));

        assert!(identity.contains("bot-id"));
        assert!(identity.contains("bot-openid"));
        assert!(identity.contains("app-from-ready"));
        assert!(!identity.contains("session-should-not-match"));
    }
}

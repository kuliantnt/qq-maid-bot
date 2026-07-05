//! Gateway 短时缓存。
//!
//! 这里只保存用于群聊触发策略的 bot 出站消息 id；引用上下文统一走 `ref_index`。

use std::collections::HashSet;

#[derive(Debug, Default)]
pub(crate) struct BotOutboundCache {
    message_ids: HashSet<String>,
}

impl BotOutboundCache {
    pub(crate) fn insert(&mut self, message_id: Option<String>) {
        if let Some(message_id) = message_id.filter(|value| !value.trim().is_empty()) {
            self.message_ids.insert(message_id);
        }
    }

    pub(crate) fn contains(&self, message_id: &str) -> bool {
        self.message_ids.contains(message_id)
    }
}

//! Gateway 短时缓存。
//!
//! 这里只保存用于群聊触发策略的 bot 出站真实消息 ID 和独立 ref_index ID；
//! 引用上下文内容统一走 `ref_index`。

use std::collections::HashSet;

#[derive(Debug, Default)]
pub(crate) struct BotOutboundCache {
    message_ids: HashSet<String>,
    ref_index_ids: HashSet<String>,
}

impl BotOutboundCache {
    pub(crate) fn insert(&mut self, message_id: Option<String>) {
        if let Some(message_id) = message_id.filter(|value| !value.trim().is_empty()) {
            self.message_ids.insert(message_id);
        }
    }

    pub(crate) fn insert_ref_index_id(&mut self, ref_index_id: Option<String>) {
        if let Some(ref_index_id) = ref_index_id.filter(|value| !value.trim().is_empty()) {
            self.ref_index_ids.insert(ref_index_id);
        }
    }

    pub(crate) fn contains(&self, message_id: &str) -> bool {
        self.message_ids.contains(message_id)
    }

    pub(crate) fn contains_ref_index_id(&self, ref_index_id: &str) -> bool {
        self.ref_index_ids.contains(ref_index_id)
    }
}

//! Gateway 短时缓存。
//!
//! 这里只保存进程内、可丢弃的 QQ 消息辅助状态：reply 内容回填缓存和机器人已发送
//! 群消息 id 缓存。它们不承载业务语义，也不参与持久化。

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use super::event::C2cMessage;

pub(crate) type ReplyCache = Arc<Mutex<HashMap<ReplyCacheKey, String>>>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ReplyCacheKey {
    scope_key: String,
    message_id: String,
}

impl ReplyCacheKey {
    fn new(scope_key: String, message_id: impl Into<String>) -> Self {
        Self {
            scope_key,
            message_id: message_id.into(),
        }
    }
}

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

/// Signal Layer 只是 gateway 内部的临时语义增强层，不是业务核心。
/// 这里只维护一个短时 `message_id -> content` 缓存，用于 reply.content 本地回填。
/// gateway 不负责 prompt 构建；真正交给 CoreService 的字符串统一在 respond.rs 的 Egress 层生成。
pub(crate) fn resolve_signals(message: &mut C2cMessage, cache: &ReplyCache) {
    let scope_key = crate::respond::scope_key_from_c2c_message(message);
    if !message.message_id.trim().is_empty() {
        cache.lock().unwrap().insert(
            ReplyCacheKey::new(scope_key.clone(), message.message_id.clone()),
            message.content.clone(),
        );
    }

    let Some(reply) = message.reply.as_mut() else {
        return;
    };
    if reply.content.is_some() || reply.message_id.trim().is_empty() {
        return;
    }
    if let Some(content) = cache
        .lock()
        .unwrap()
        .get(&ReplyCacheKey::new(scope_key, reply.message_id.clone()))
        .cloned()
    {
        // cache 只用于短时 reply 回填，不在 gateway 内承载更高层业务语义。
        reply.content = Some(content);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::event::MessageReply;

    fn c2c_message(
        message_id: &str,
        user_openid: &str,
        content: &str,
        reply_message_id: Option<&str>,
    ) -> C2cMessage {
        C2cMessage {
            message_id: message_id.to_owned(),
            event_id: Some(format!("event-{message_id}")),
            source_message_ids: vec![message_id.to_owned()],
            source_event_ids: vec![format!("event-{message_id}")],
            user_openid: user_openid.to_owned(),
            content: content.to_owned(),
            reply: reply_message_id.map(|message_id| MessageReply {
                message_id: message_id.to_owned(),
                content: None,
            }),
            timestamp: None,
            first_message_timestamp: None,
            last_message_timestamp: None,
            attachments: Vec::new(),
        }
    }

    #[tokio::test]
    async fn resolve_signals_fills_known_reply_content() {
        let cache: ReplyCache = Arc::new(Mutex::new(HashMap::new()));
        cache.lock().unwrap().insert(
            ReplyCacheKey::new(
                "platform:qq_official:account:-:private:user-1".to_owned(),
                "quoted-1",
            ),
            "上一条消息".to_owned(),
        );
        let mut message = c2c_message("msg-1", "user-1", "你好", Some("quoted-1"));

        resolve_signals(&mut message, &cache);

        assert_eq!(
            message.reply,
            Some(MessageReply {
                message_id: "quoted-1".to_owned(),
                content: Some("上一条消息".to_owned()),
            })
        );
        assert_eq!(
            cache
                .lock()
                .unwrap()
                .get(&ReplyCacheKey::new(
                    "platform:qq_official:account:-:private:user-1".to_owned(),
                    "msg-1"
                ))
                .map(String::as_str),
            Some("你好")
        );
    }

    #[test]
    fn resolve_signals_keeps_reply_content_none_on_cache_miss() {
        let cache: ReplyCache = Arc::new(Mutex::new(HashMap::new()));
        let mut message = c2c_message("msg-1", "user-1", "你好", Some("quoted-missing"));

        resolve_signals(&mut message, &cache);

        assert_eq!(
            message.reply,
            Some(MessageReply {
                message_id: "quoted-missing".to_owned(),
                content: None,
            })
        );
        assert_eq!(
            cache
                .lock()
                .unwrap()
                .get(&ReplyCacheKey::new(
                    "platform:qq_official:account:-:private:user-1".to_owned(),
                    "msg-1"
                ))
                .map(String::as_str),
            Some("你好")
        );
    }

    #[test]
    fn reply_cache_isolated_by_scope_key() {
        let cache: ReplyCache = Arc::new(Mutex::new(HashMap::new()));
        cache.lock().unwrap().insert(
            ReplyCacheKey::new(
                "platform:qq_official:account:-:private:user-a".to_owned(),
                "same-id",
            ),
            "私聊消息".to_owned(),
        );
        cache.lock().unwrap().insert(
            ReplyCacheKey::new(
                "platform:qq_official:account:-:group:group-a".to_owned(),
                "same-id",
            ),
            "群聊消息".to_owned(),
        );

        let mut private_message = c2c_message("m1", "user-a", "当前消息", Some("same-id"));
        resolve_signals(&mut private_message, &cache);

        let mut group_like_private = c2c_message("m2", "user-b", "另一条", Some("same-id"));
        resolve_signals(&mut group_like_private, &cache);

        assert_eq!(
            private_message.reply.and_then(|reply| reply.content),
            Some("私聊消息".to_owned())
        );
        assert_eq!(
            group_like_private.reply.and_then(|reply| reply.content),
            None
        );
    }
}

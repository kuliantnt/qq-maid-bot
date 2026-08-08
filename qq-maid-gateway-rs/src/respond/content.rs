//! Gateway 到 Core 的 scope 和文本内容构建。

use crate::{
    event::{C2cMessage, GroupMessage},
    gateway::platform,
};

/// Gateway 侧需要在入队前拿到与 Core 完全一致的 scope_key，用于会话串行调度和 reply cache 隔离。
pub fn scope_key_from_c2c_message(message: &C2cMessage) -> String {
    let inbound = platform::qq_official::inbound_from_c2c(message);
    platform::core_scope_key(&inbound).expect("QQ C2C inbound message should have a Core scope")
}

/// 群聊 scope 直接复用 Core 的 `group:{group_id}` 规则，避免 Gateway 自己维护第二套会话边界。
pub fn scope_key_from_group_message(message: &GroupMessage) -> String {
    let inbound = platform::qq_official::inbound_from_group(message);
    platform::core_scope_key(&inbound).expect("QQ group inbound message should have a Core scope")
}

/// Egress 层是 gateway 内唯一允许拼接 Core 文本协议的位置。
/// 这里把 reply block 和附件备注按既有协议收口，避免平台字段污染 Core 稳定模型。
pub fn build_respond_content(message: &C2cMessage) -> String {
    let inbound = platform::qq_official::inbound_from_c2c(message);
    platform::render_text_for_core(&inbound)
}

pub(super) fn clean_optional(value: String) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

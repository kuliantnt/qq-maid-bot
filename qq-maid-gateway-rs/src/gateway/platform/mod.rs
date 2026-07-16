//! Gateway 内部平台抽象边界。
//!
//! `model` 只定义平台无关入站结构；各平台 adapter 负责把原始协议转换为统一模型；
//! `core` 负责把统一模型映射到 CoreService 所需的请求和文本协议。

mod core;
pub(crate) mod member_enrich;
mod model;
pub(crate) mod onebot11;
pub(crate) mod qq_official;
pub(crate) mod wechat_service;

pub(crate) use core::{core_scope_key, render_text_for_core, to_core_request};
#[cfg(test)]
pub(crate) use model::Actor;
pub(crate) use model::{ConversationTarget, InboundMessage, Platform};

/// 只识别需要交给 Core 判定的斜杠命令候选，不在 Gateway 维护命令白名单。
pub(crate) fn is_slash_command_candidate(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with('/') || text.starts_with('／')
}

//! 平台无关的消息身份上下文。
//!
//! 这些结构只承载 Gateway 已知的入站身份事实。服务端权限、owner 和 session scope
//! 仍应使用各业务模块自己的稳定字段，不能让 LLM 基于本上下文反向决定真实身份。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    #[default]
    Event,
    MemberApi,
    Cache,
    LegacyFallback,
    TextWeak,
}

impl IdentitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::MemberApi => "member_api",
            Self::Cache => "cache",
            Self::LegacyFallback => "legacy_fallback",
            Self::TextWeak => "text_weak",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MentionConfidence {
    /// 平台事件明确给出的结构化 mention。
    #[default]
    Event,
    MemberApi,
    Cache,
    TextWeak,
}

impl MentionConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::MemberApi => "member_api",
            Self::Cache => "cache",
            Self::TextWeak => "text_weak",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MessageActorContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub union_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_member_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_bot: Option<bool>,
    #[serde(default)]
    pub source: IdentitySource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MentionIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    pub target: MessageActorContext,
    #[serde(default)]
    pub is_self: bool,
    #[serde(default)]
    pub confidence: MentionConfidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ConversationContext {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MessageContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<MessageActorContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<MentionIdentity>,
    pub conversation: ConversationContext,
}

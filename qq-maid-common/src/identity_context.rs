//! 平台无关的消息身份上下文。
//!
//! 这些结构只承载 Gateway 已知的入站身份事实。服务端权限、owner 和 session scope
//! 仍应使用各业务模块自己的稳定字段，不能让 LLM 基于本上下文反向决定真实身份。
//!
//! # 字段来源与优先级
//!
//! - `IdentitySource::Event`：来自平台入站事件的结构化字段，最高优先级。
//! - `IdentitySource::MemberApi` / `Cache`：Phase 3 接入 #229 成员详情补全后使用。
//! - `IdentitySource::LegacyFallback`：旧字段兼容兜底（如不可信的 author.id），
//!   仅作展示，不得用于强权限或数据归属。
//! - `IdentitySource::TextWeak`：仅来自文本 `@昵称`，无稳定 ID，不可当强身份。
//!
//! 缺失字段一律留空并如实标注来源，不得伪造稳定 ID 或 is_bot。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdentitySource {
    /// 平台入站事件直接给出的结构化身份字段，最高优先级。
    #[default]
    Event,
    /// 成员详情接口补全（Phase 3 接入 #229）。
    MemberApi,
    /// 成员详情缓存命中（Phase 3 接入 #229）。
    Cache,
    /// 旧字段兼容兜底，仅作展示，不可用于强权限或数据归属。
    LegacyFallback,
    /// 仅来自文本 `@昵称`，无稳定 ID，不可当强身份。
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
    /// 平台事件明确给出的结构化 mention（`mentions[]` / `GROUP_AT_MESSAGE_CREATE`）。
    #[default]
    Event,
    /// 成员详情接口补全后标注的 mention（Phase 3）。
    MemberApi,
    /// 成员详情缓存命中后标注的 mention（Phase 3）。
    Cache,
    /// 仅来自文本 `@昵称`，无稳定 ID，弱候选。
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

/// 平台无关的会话类型。
///
/// Gateway、Core 和 LLM ToolContext 共享该事实类型；具体平台事件仍应先在接入层归一化。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    Private,
    Group,
    Channel,
    ServiceAccount,
    #[default]
    Unknown,
}

impl ConversationKind {
    pub fn from_name(value: &str) -> Self {
        match value.trim() {
            "private" => Self::Private,
            "group" => Self::Group,
            "channel" => Self::Channel,
            "service_account" => Self::ServiceAccount,
            _ => Self::Unknown,
        }
    }

    pub fn is_group(self) -> bool {
        self == Self::Group
    }
}

/// 服务端执行能力时使用的权威操作者身份。
///
/// 与模型可见的 [`MessageActorContext`] 分离；这些字段只能由服务端入口派生，不能由
/// 模型参数或展示昵称补充。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionActorContext {
    pub user_id: Option<String>,
    pub group_member_role: Option<String>,
}

/// 服务端执行能力时使用的权威会话与作用域事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionConversationContext {
    pub platform: String,
    pub account_id: Option<String>,
    pub kind: ConversationKind,
    /// 平台提供的原始会话目标 ID；不是投递凭据。
    pub target_id: Option<String>,
    /// conversation 业务作用域。
    pub scope_id: String,
    /// actor-aware interaction 作用域，用于 pending、可见编号等个人交互状态。
    pub interaction_scope_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
/// 单个发言者 / 被提及者的身份摘要。
///
/// 稳定身份 key 只取 `user_id` / `union_id`；`display_name` / `group_member_role`
/// 仅供 LLM 理解和展示，不可单独用于权限或归属判断。
/// 缺失字段保持 `None` 并通过 `source` 如实标注来源，不伪造。
pub struct MessageActorContext {
    /// 平台结构化稳定 ID（member openid / user openid）。程序权威身份来源。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// 跨群稳定 ID（如 QQ union_openid），事件未提供时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub union_id: Option<String>,
    /// 展示名 / 昵称 / 群名片，仅供 LLM 理解，非稳定身份。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// 展示名来源，仅描述 display_name 本身，不代表稳定身份字段来源。
    ///
    /// 典型值：manual / event / member_api / cache / legacy_fallback / text_weak。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name_source: Option<String>,
    /// 群角色字符串（owner/admin/member/unknown），仅供 LLM 理解。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_member_role: Option<String>,
    /// 是否机器人；仅平台事件明确给出时为 Some，否则 None 表示未知。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_bot: Option<bool>,
    /// 本条身份字段的来源，见 `IdentitySource` 文档。
    #[serde(default)]
    pub source: IdentitySource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
/// 一条消息中 `@` 对象的结构化身份。
///
/// `target.user_id` 存在时为平台结构化稳定 ID（confidence >= Event/MemberApi/Cache）；
/// 只有文本 `@昵称` 时 `target.user_id` 为 None、`confidence = TextWeak`，
/// `target.display_name` 仅作弱线索，不可当稳定身份。
pub struct MentionIdentity {
    /// 原始 @ 文本（如 `@当前机器人` / `@小明`），如可用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    /// 被提及者身份摘要。
    pub target: MessageActorContext,
    /// 是否 @ 的是当前机器人本身。
    #[serde(default)]
    pub is_self: bool,
    /// 本条 mention 的置信度，见 `MentionConfidence` 文档。
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
    /// 当前共享会话发言人的脱敏引用，由 Core 使用 conversation scope 与稳定 user_id 生成。
    ///
    /// 仅用于让模型把当前发言人与历史 `actor_ref` 对齐，不用于权限判断或用户展示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_actor_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<MessageActorContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<MentionIdentity>,
    pub conversation: ConversationContext,
}

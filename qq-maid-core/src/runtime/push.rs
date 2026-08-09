//! Core 主动推送边界。
//!
//! Core 只表达“要推给哪个平台账号下的哪个目标、推什么内容”，不携带 HTTP URL、
//! token 或 QQ 原始 payload。实际平台发送、Markdown fallback 和群消息缓存仍由
//! Gateway/平台层实现。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::{collections::HashSet, str::FromStr};

use crate::{identity::parse_stable_scope_key, service::VisibleEntitySnapshot};

pub const QQ_OFFICIAL_PLATFORM: &str = "qq_official";
pub const ONEBOT11_PLATFORM: &str = "onebot11";

/// 平台无关的主动推送成员提醒。
///
/// `user_id` 只表达平台成员身份，`display_name` 仅供不支持原生 mention 时安全展示；
/// Core 不在这里生成 QQ 标记、OneBot segment 或其他平台协议内容。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PushMention {
    pub user_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl PushMention {
    pub fn new(user_id: impl Into<String>, display_name: Option<String>) -> Self {
        Self {
            user_id: user_id.into(),
            display_name,
        }
    }
}

/// 忽略空 ID，并按用户 ID 稳定去重；昵称不参与身份判断。
pub fn normalize_push_mentions(mentions: Vec<PushMention>) -> Vec<PushMention> {
    let mut seen = HashSet::new();
    mentions
        .into_iter()
        .filter_map(|mention| {
            let user_id = mention.user_id.trim().to_owned();
            if user_id.is_empty() || !seen.insert(user_id.clone()) {
                return None;
            }
            let display_name = clean_optional(mention.display_name);
            Some(PushMention {
                user_id,
                display_name,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushTargetType {
    Private,
    Group,
}

impl PushTargetType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Group => "group",
        }
    }
}

impl FromStr for PushTargetType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "private" => Ok(Self::Private),
            "group" => Ok(Self::Group),
            other => Err(format!("invalid push target type `{other}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTarget {
    pub platform: String,
    pub account_id: Option<String>,
    pub target_type: PushTargetType,
    pub target_id: String,
}

impl PushTarget {
    pub fn new(
        platform: impl Into<String>,
        account_id: Option<String>,
        target_type: PushTargetType,
        target_id: impl Into<String>,
    ) -> Self {
        Self {
            platform: clean_or_default_platform(platform.into()),
            account_id: clean_optional(account_id),
            target_type,
            target_id: target_id.into(),
        }
    }

    /// 兼容历史只保存 private/group + target_id 的 QQ 官方主动推送目标。
    pub fn qq_official(target_type: PushTargetType, target_id: impl Into<String>) -> Self {
        Self::new(QQ_OFFICIAL_PLATFORM, None, target_type, target_id)
    }

    pub fn onebot11(
        account_id: impl Into<String>,
        target_type: PushTargetType,
        target_id: impl Into<String>,
    ) -> Self {
        Self::new(
            ONEBOT11_PLATFORM,
            Some(account_id.into()),
            target_type,
            target_id,
        )
    }

    /// 从新 stable scope 中继承 platform/account；旧 scope 或不可解析 scope 仍按 QQ 官方处理。
    ///
    /// 业务存储里 `scope_key` 只承担身份隔离语义，这里只在生成主动推送目标时读取其中
    /// 已有的平台字段，避免 Core 继续把所有目标都默认交给 QQ sender。
    pub fn from_scope_key_or_qq_official(
        scope_key: &str,
        fallback_target_type: PushTargetType,
        fallback_target_id: impl Into<String>,
    ) -> Self {
        let fallback_target_id = fallback_target_id.into();
        if let Some(parsed) = parse_stable_scope_key(scope_key)
            && parsed.target_type == fallback_target_type.as_str()
        {
            return Self::new(
                parsed.platform,
                Some(parsed.account_id.to_owned()),
                fallback_target_type,
                fallback_target_id,
            );
        }
        Self::qq_official(fallback_target_type, fallback_target_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_scope_only_supplies_platform_and_account_not_delivery_target() {
        let target = PushTarget::from_scope_key_or_qq_official(
            "platform:qq_official:account:app-1:private:stale-openid",
            PushTargetType::Private,
            "current-openid",
        );

        assert_eq!(target.platform, "qq_official");
        assert_eq!(target.account_id.as_deref(), Some("app-1"));
        assert_eq!(target.target_type, PushTargetType::Private);
        assert_eq!(target.target_id, "current-openid");
    }

    #[test]
    fn legacy_scope_still_defaults_to_qq_official_target() {
        let target = PushTarget::from_scope_key_or_qq_official(
            "private:legacy-openid",
            PushTargetType::Private,
            "legacy-openid",
        );

        assert_eq!(target.platform, QQ_OFFICIAL_PLATFORM);
        assert_eq!(target.account_id, None);
        assert_eq!(target.target_id, "legacy-openid");
    }

    #[test]
    fn onebot_helper_requires_explicit_account_and_uses_gateway_platform_name() {
        let target = PushTarget::onebot11("bot-1", PushTargetType::Group, "group-1");

        assert_eq!(target.platform, ONEBOT11_PLATFORM);
        assert_eq!(target.account_id.as_deref(), Some("bot-1"));
        assert_eq!(target.target_type, PushTargetType::Group);
        assert_eq!(target.target_id, "group-1");
    }

    #[test]
    fn core_onebot_scope_is_normalized_for_gateway_routing() {
        let target = PushTarget::from_scope_key_or_qq_official(
            "platform:onebot:account:bot-1:private:user-1",
            PushTargetType::Private,
            "user-1",
        );

        assert_eq!(target.platform, ONEBOT11_PLATFORM);
        assert_eq!(target.account_id.as_deref(), Some("bot-1"));
        assert_eq!(target.target_id, "user-1");
    }

    #[test]
    fn mentions_ignore_empty_ids_and_stably_deduplicate_by_user_id() {
        let mentions = normalize_push_mentions(vec![
            PushMention::new(" user-1 ", Some(" 张三 ".to_owned())),
            PushMention::new("", Some("空".to_owned())),
            PushMention::new("user-2", None),
            PushMention::new("user-1", Some("另一个昵称".to_owned())),
        ]);

        assert_eq!(
            mentions,
            vec![
                PushMention::new("user-1", Some("张三".to_owned())),
                PushMention::new("user-2", None),
            ]
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushIntent {
    pub target: PushTarget,
    pub mentions: Vec<PushMention>,
    pub message_type: String,
    pub text: String,
    pub fallback_text: Option<String>,
    pub visible_entity_snapshot: Option<VisibleEntitySnapshot>,
}

#[derive(Debug, Error)]
pub enum PushError {
    #[error("push failed: {summary}")]
    Failed { summary: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushResult {
    pub message_id: Option<String>,
}

#[async_trait]
pub trait PushSink: Send + Sync {
    async fn push(&self, intent: PushIntent) -> Result<PushResult, PushError>;
}

fn clean_or_default_platform(value: String) -> String {
    let value = value.trim();
    match value {
        "" => QQ_OFFICIAL_PLATFORM.to_owned(),
        // CoreService 的稳定会话平台名是 `onebot`，Gateway 的协议/路由名是
        // `onebot11`。只在投递目标边界规范化，不能改写既有 session scope。
        "onebot" => ONEBOT11_PLATFORM.to_owned(),
        other => other.to_owned(),
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty() && value != "-")
}

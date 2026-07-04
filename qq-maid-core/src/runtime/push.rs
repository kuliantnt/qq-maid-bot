//! Core 主动推送边界。
//!
//! Core 只表达“要推给哪个平台账号下的哪个目标、推什么内容”，不携带 HTTP URL、
//! token 或 QQ 原始 payload。实际平台发送、Markdown fallback 和群消息缓存仍由
//! Gateway/平台层实现。

use async_trait::async_trait;
use thiserror::Error;

use crate::identity::parse_stable_scope_key;

pub const QQ_OFFICIAL_PLATFORM: &str = "qq_official";

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

    pub fn from_str(value: &str) -> Result<Self, String> {
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
                parsed.raw_target_id,
            );
        }
        Self::qq_official(fallback_target_type, fallback_target_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushIntent {
    pub target: PushTarget,
    pub message_type: String,
    pub text: String,
    pub fallback_text: Option<String>,
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
    if value.is_empty() {
        QQ_OFFICIAL_PLATFORM.to_owned()
    } else {
        value.to_owned()
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty() && value != "-")
}

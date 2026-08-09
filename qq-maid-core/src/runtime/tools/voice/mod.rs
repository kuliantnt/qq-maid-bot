//! 会话级语音回复偏好领域门面。
//!
//! 偏好只按平台、机器人账号和真实私聊/群聊目标持久化，不依赖 session_id；Respond
//! 只调用本门面，不复制 SQL、权限或配置可用性判断。

mod storage;

pub use storage::{VOICE_PREFERENCE_SCHEMA_V1, VoicePreferenceStore, VoiceStorageError};

use crate::{config::VoiceFeatureConfig, runtime::respond::RespondRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCommand {
    Query,
    Enable,
    Disable,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoicePreferenceKey {
    pub platform: String,
    pub account_id: String,
    pub target_type: &'static str,
    pub target_id: String,
}

#[derive(Clone)]
pub struct VoicePreferenceService {
    store: VoicePreferenceStore,
    config: VoiceFeatureConfig,
}

impl VoicePreferenceService {
    pub fn new(store: VoicePreferenceStore, config: VoiceFeatureConfig) -> Self {
        Self { store, config }
    }

    pub fn enabled_for_request(&self, request: &RespondRequest) -> Result<bool, VoiceStorageError> {
        let Some(key) = preference_key(request) else {
            return Ok(false);
        };
        self.store.is_enabled(&key)
    }

    /// 最终出站只有在偏好开启且当前 Provider 配置预检通过时才携带 voice hint。
    pub fn delivery_enabled_for_request(
        &self,
        request: &RespondRequest,
    ) -> Result<bool, VoiceStorageError> {
        Ok(self.config.is_available() && self.enabled_for_request(request)?)
    }

    pub fn execute(
        &self,
        command: VoiceCommand,
        request: &RespondRequest,
    ) -> Result<VoiceCommandResult, VoiceStorageError> {
        let Some(key) = preference_key(request) else {
            return Ok(VoiceCommandResult::new(false, "当前入口暂不支持语音回复"));
        };
        let current = self.store.is_enabled(&key)?;
        match command {
            VoiceCommand::Query => Ok(VoiceCommandResult::new(
                current,
                if current {
                    "当前会话语音回复：已开启"
                } else {
                    "当前会话语音回复：已关闭"
                },
            )),
            VoiceCommand::Invalid => Ok(VoiceCommandResult::new(
                current,
                "用法：/语音、/语音 开启、/语音 关闭",
            )),
            VoiceCommand::Enable | VoiceCommand::Disable if !may_modify_group(request) => Ok(
                VoiceCommandResult::new(current, "只有群主或管理员可以修改群聊语音设置"),
            ),
            VoiceCommand::Enable => {
                if let Some(reason) = self.config.enable_rejection_text() {
                    return Ok(VoiceCommandResult::new(current, reason));
                }
                self.store.set_enabled(&key, true)?;
                Ok(VoiceCommandResult::new(true, "语音回复已开启"))
            }
            VoiceCommand::Disable => {
                self.store.set_enabled(&key, false)?;
                Ok(VoiceCommandResult::new(false, "语音回复已关闭"))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceCommandResult {
    pub enabled: bool,
    pub text: String,
}

impl VoiceCommandResult {
    fn new(enabled: bool, text: impl Into<String>) -> Self {
        Self {
            enabled,
            text: text.into(),
        }
    }
}

pub fn parse_voice_command(text: &str) -> Option<VoiceCommand> {
    // Core 命令前缀层会把任意已配置前缀规范化为 canonical `/`；领域 parser 同时
    // 接受去前缀形式，便于领域单测和未来非 Slash 调用复用。
    let text = text.strip_prefix('/').unwrap_or(text);
    let mut parts = text.split_whitespace();
    if parts.next()? != "语音" {
        return None;
    }
    let action = match parts.next() {
        None => VoiceCommand::Query,
        Some("开启" | "打开" | "on") => VoiceCommand::Enable,
        Some("关闭" | "关掉" | "off") => VoiceCommand::Disable,
        Some(_) => VoiceCommand::Invalid,
    };
    Some(if parts.next().is_some() {
        VoiceCommand::Invalid
    } else {
        action
    })
}

fn preference_key(request: &RespondRequest) -> Option<VoicePreferenceKey> {
    // 第一版只有 QQ 官方 Gateway 能消费 voice delivery hint；其他入口查询也不创建状态。
    if request.platform != "qq_official" {
        return None;
    }
    let account_id = request
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let target_id = request
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    // 会话类型只信任 Gateway 归一化的权威字段；未知类型和不完整群上下文都不读写。
    let target_type = match request.conversation_kind {
        qq_maid_common::identity_context::ConversationKind::Private => "private",
        qq_maid_common::identity_context::ConversationKind::Group => {
            let group_id = request
                .group_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            if group_id != target_id {
                return None;
            }
            "group"
        }
        _ => return None,
    };
    Some(VoicePreferenceKey {
        platform: request.platform.clone(),
        account_id: account_id.to_owned(),
        target_type,
        target_id: target_id.to_owned(),
    })
}

fn may_modify_group(request: &RespondRequest) -> bool {
    match request.conversation_kind {
        qq_maid_common::identity_context::ConversationKind::Private => true,
        qq_maid_common::identity_context::ConversationKind::Group => matches!(
            request.group_member_role.as_deref(),
            Some("owner" | "admin")
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests;

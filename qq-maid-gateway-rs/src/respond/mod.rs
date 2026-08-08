//! Gateway 到 Core 的进程内响应边界。
//!
//! 本模块只负责 RespondClient 的核心门面和模块编排；平台内容拼接、寻址归一化、
//! 诊断日志和用户可见错误分别由职责明确的子模块实现。

use std::sync::Arc;

use qq_maid_common::command_prefix::CommandPrefix;
#[cfg(test)]
use qq_maid_common::input_part::MessageInputPart;
#[cfg(test)]
use qq_maid_core::service::CoreResponse;
use qq_maid_core::service::{
    CoreError, CoreInboundClassification, CoreRequest, CoreRespondOutput, CoreService,
};

use crate::{
    event::{C2cMessage, GroupMessage},
    gateway::platform,
    logging::mask_openid,
};
use tracing::warn;

mod addressing;
mod content;
mod diagnostics;
mod error;

pub use addressing::build_group_respond_content;
#[cfg(test)]
pub(crate) use addressing::normalized_group_inbound;
pub(crate) use addressing::{
    build_group_command_content_with_prefix, build_group_respond_content_with_prefix,
    normalized_group_inbound_with_prefix,
};
use content::clean_optional;
pub use content::{
    build_respond_content, scope_key_from_c2c_message, scope_key_from_group_message,
};
use diagnostics::{
    log_core_output_success, log_inbound_media_diagnostics, masked_log_context_from_inbound,
};
pub use error::{RespondError, respond_error_to_qq_text};

#[derive(Clone)]
pub struct RespondClient {
    core: Arc<dyn CoreService>,
    qq_official_account_id: Option<String>,
}

impl RespondClient {
    pub fn new(core: Arc<dyn CoreService>) -> Self {
        Self {
            core,
            qq_official_account_id: None,
        }
    }

    pub fn with_qq_official_account_id(mut self, account_id: impl Into<String>) -> Self {
        self.qq_official_account_id = clean_optional(account_id.into());
        self
    }

    /// `/ping check` 直接调用 Core 诊断入口，不创建 session，也不携带 QQ 用户内容。
    pub async fn check_upstream(&self) -> Result<(), RespondError> {
        self.core.upstream_check().await.map_err(RespondError::Core)
    }

    pub fn health_snapshot(&self) -> qq_maid_core::service::CoreHealthSnapshot {
        self.core.health_snapshot()
    }

    pub async fn respond_c2c(
        &self,
        message: &C2cMessage,
        content: String,
    ) -> Result<CoreRespondOutput, RespondError> {
        let request = self.core_request_from_c2c_message(message, content);
        let masked_user = mask_openid(&message.user_openid);
        let output = self.core.respond(request).await.map_err(|error| {
            warn!(
                message_id = %message.message_id,
                user = %masked_user,
                error = %format!("{}@{}", error.code, error.stage),
                "Core 回复请求失败"
            );
            RespondError::Core(error)
        })?;
        log_core_output_success(&message.message_id, Some(&masked_user), None, &output);
        Ok(output)
    }

    pub async fn classify_c2c(
        &self,
        message: &C2cMessage,
        content: String,
    ) -> Result<CoreInboundClassification, RespondError> {
        let request = self.core_request_from_c2c_message(message, content);
        self.core
            .classify_inbound(request)
            .await
            .map_err(RespondError::Core)
    }

    pub async fn respond_group(
        &self,
        message: &GroupMessage,
        content: String,
    ) -> Result<CoreRespondOutput, RespondError> {
        let request = self.core_request_from_group_message(message, content);
        let masked_group = mask_openid(&message.group_openid);
        let output = self.core.respond(request).await.map_err(|error| {
            warn!(
                message_id = %message.message_id,
                group = %masked_group,
                error = %format!("{}@{}", error.code, error.stage),
                "Core 群聊回复请求失败"
            );
            RespondError::Core(error)
        })?;
        log_core_output_success(&message.message_id, None, Some(&masked_group), &output);
        Ok(output)
    }

    pub async fn classify_group(
        &self,
        message: &GroupMessage,
        active_keywords: &[String],
        command_prefix: CommandPrefix,
        content: String,
    ) -> Result<CoreInboundClassification, RespondError> {
        let inbound =
            normalized_group_inbound_with_prefix(message, active_keywords, command_prefix);
        let request = platform::to_core_request(&self.prepare_inbound(inbound), content)
            .expect("QQ group inbound message should map to CoreRequest");
        self.core
            .classify_inbound(request)
            .await
            .map_err(RespondError::Core)
    }

    pub(crate) async fn respond_inbound(
        &self,
        inbound: &platform::InboundMessage,
        content: String,
    ) -> Result<CoreRespondOutput, RespondError> {
        let (masked_user, masked_group) = masked_log_context_from_inbound(inbound);
        let request = platform::to_core_request(inbound, content).map_err(|error| {
            warn!(
                message_id = %inbound.message_id,
                user = masked_user.as_deref().unwrap_or(""),
                group = masked_group.as_deref().unwrap_or(""),
                platform = %inbound.platform.as_str(),
                error = %error,
                "Core 入站消息转换失败"
            );
            RespondError::Core(CoreError {
                code: "invalid_request".to_owned(),
                stage: "gateway_mapping".to_owned(),
                message: error.to_string(),
            })
        })?;
        let output = self.core.respond(request).await.map_err(|error| {
            warn!(
                message_id = %inbound.message_id,
                user = masked_user.as_deref().unwrap_or(""),
                group = masked_group.as_deref().unwrap_or(""),
                platform = %inbound.platform.as_str(),
                error = %format!("{}@{}", error.code, error.stage),
                "Core 入站回复请求失败"
            );
            RespondError::Core(error)
        })?;
        log_core_output_success(
            &inbound.message_id,
            masked_user.as_deref(),
            masked_group.as_deref(),
            &output,
        );
        Ok(output)
    }

    pub(crate) async fn classify_inbound(
        &self,
        inbound: &platform::InboundMessage,
        content: String,
    ) -> Result<CoreInboundClassification, RespondError> {
        let request = platform::to_core_request(&self.prepare_inbound(inbound.clone()), content)
            .map_err(|error| {
                RespondError::Core(CoreError {
                    code: "invalid_request".to_owned(),
                    stage: "gateway_mapping".to_owned(),
                    message: error.to_string(),
                })
            })?;
        self.core
            .classify_inbound(request)
            .await
            .map_err(RespondError::Core)
    }

    pub fn core_request_from_c2c_message(
        &self,
        message: &C2cMessage,
        content: String,
    ) -> CoreRequest {
        let inbound = platform::qq_official::inbound_from_c2c(message);
        platform::to_core_request(&self.prepare_inbound(inbound), content)
            .expect("QQ C2C inbound message should map to CoreRequest")
    }

    pub fn core_request_from_group_message(
        &self,
        message: &GroupMessage,
        content: String,
    ) -> CoreRequest {
        let inbound = platform::qq_official::inbound_from_group(message);
        platform::to_core_request(&self.prepare_inbound(inbound), content)
            .expect("QQ group inbound message should map to CoreRequest")
    }

    /// Gateway 入队、聚合和 Core respond 必须使用同一套账号注入逻辑计算 scope_key。
    pub fn scope_key_from_c2c_message(&self, message: &C2cMessage) -> String {
        let inbound = platform::qq_official::inbound_from_c2c(message);
        platform::core_scope_key(&self.prepare_inbound(inbound))
            .expect("QQ C2C inbound message should have a Core scope")
    }

    /// 群聊 scope 按群目标隔离，actor 只表示发言人，不参与群 session 拆分。
    pub fn scope_key_from_group_message(&self, message: &GroupMessage) -> String {
        let inbound = platform::qq_official::inbound_from_group(message);
        platform::core_scope_key(&self.prepare_inbound(inbound))
            .expect("QQ group inbound message should have a Core scope")
    }

    /// 注入 gateway 级账号隔离字段，供 ref_index、调度 scope 和 Core request 复用。
    pub(crate) fn prepare_inbound(
        &self,
        mut inbound: platform::InboundMessage,
    ) -> platform::InboundMessage {
        if inbound.platform == platform::Platform::QqOfficial && inbound.account_id.is_none() {
            inbound.account_id = self.qq_official_account_id.clone();
        }
        log_inbound_media_diagnostics(&inbound);
        inbound
    }
}

#[cfg(test)]
mod tests;

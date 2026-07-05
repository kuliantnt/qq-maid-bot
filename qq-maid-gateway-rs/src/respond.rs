//! Gateway 到 Core 的进程内响应边界。
//!
//! 本模块只负责 Gateway 入站消息到 `CoreRequest` 的映射、内容拼接和安全错误文案。
//! 不再保留 HTTP、JSON DTO 或 SSE 解析，避免同进程组件之间出现第二套传输协议。

use std::sync::Arc;

use qq_maid_common::input_part::{MediaStatus, MessageInputPart};
use qq_maid_core::service::{
    CoreError, CoreInboundClassification, CoreRequest, CoreRespondOutput, CoreResponse,
    CoreResponseEvent, CoreResponseStream, CoreService,
};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::{
    event::{C2cMessage, GroupMessage},
    gateway::platform,
    logging::mask_openid,
};

#[derive(Clone)]
pub struct RespondClient {
    core: Arc<dyn CoreService>,
    qq_official_account_id: Option<String>,
}

pub type RespondResponse = CoreResponse;

#[derive(Debug)]
pub enum RespondTransport {
    Complete(CoreResponse),
    Stream(CoreResponseStream),
}

pub type RespondEvent = CoreResponseEvent;

#[derive(Debug, Error)]
pub enum RespondError {
    #[error("core request failed: {0}")]
    Core(#[from] CoreError),
}

impl RespondError {
    pub fn log_summary(&self) -> String {
        match self {
            Self::Core(error) => format!("{}@{}", error.code, error.stage),
        }
    }

    pub fn qq_visible_kind(&self) -> String {
        match self {
            Self::Core(error) if error.code == "timeout" => "timeout".to_owned(),
            Self::Core(error) if error.code == "config" => "config".to_owned(),
            Self::Core(error) => format!("{}@{}", error.code, error.stage),
        }
    }
}

pub fn respond_error_to_qq_text(err: &RespondError) -> String {
    match err {
        RespondError::Core(error) => {
            respond_error_info_to_qq_text(&error.code, &error.stage, &error.message)
        }
    }
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
    ) -> Result<RespondTransport, RespondError> {
        let request = self.core_request_from_c2c_message(message, content);
        let masked_user = mask_openid(&message.user_openid);
        let output = self.core.respond(request).await.map_err(|error| {
            warn!(
                message_id = %message.message_id,
                user = %masked_user,
                error = %format!("{}@{}", error.code, error.stage),
                "core respond request failed"
            );
            RespondError::Core(error)
        })?;
        log_core_output_success(&message.message_id, Some(&masked_user), None, &output);
        Ok(output.into())
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
    ) -> Result<RespondTransport, RespondError> {
        let request = self.core_request_from_group_message(message, content);
        let masked_group = mask_openid(&message.group_openid);
        let output = self.core.respond(request).await.map_err(|error| {
            warn!(
                message_id = %message.message_id,
                group = %masked_group,
                error = %format!("{}@{}", error.code, error.stage),
                "core group respond request failed"
            );
            RespondError::Core(error)
        })?;
        log_core_output_success(&message.message_id, None, Some(&masked_group), &output);
        Ok(output.into())
    }

    pub(crate) async fn respond_inbound(
        &self,
        inbound: &platform::InboundMessage,
        content: String,
    ) -> Result<RespondTransport, RespondError> {
        let (masked_user, masked_group) = masked_log_context_from_inbound(inbound);
        let request = platform::to_core_request(inbound, content).map_err(|error| {
            warn!(
                message_id = %inbound.message_id,
                user = masked_user.as_deref().unwrap_or(""),
                group = masked_group.as_deref().unwrap_or(""),
                platform = %inbound.platform.as_str(),
                error = %error,
                "core inbound mapping failed"
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
                "core inbound respond request failed"
            );
            RespondError::Core(error)
        })?;
        log_core_output_success(
            &inbound.message_id,
            masked_user.as_deref(),
            masked_group.as_deref(),
            &output,
        );
        Ok(output.into())
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

impl From<CoreRespondOutput> for RespondTransport {
    fn from(value: CoreRespondOutput) -> Self {
        match value {
            CoreRespondOutput::Complete(response) => Self::Complete(response),
            CoreRespondOutput::Stream(stream) => Self::Stream(stream),
        }
    }
}

fn log_core_output_success(
    message_id: &str,
    masked_user: Option<&str>,
    masked_group: Option<&str>,
    output: &CoreRespondOutput,
) {
    let output_policy = output.output_policy().as_str();
    match output {
        CoreRespondOutput::Complete(response) => {
            info!(
                message_id,
                user = masked_user.unwrap_or(""),
                group = masked_group.unwrap_or(""),
                handled = response.handled.unwrap_or(false),
                handled_present = response.handled.is_some(),
                command = response.command.as_deref().unwrap_or(""),
                reply_len = response
                    .text
                    .as_deref()
                    .map(|text| text.chars().count())
                    .unwrap_or(0),
                transport = "complete",
                response_delivery_mode = output_policy,
                "core respond request succeeded"
            );
        }
        CoreRespondOutput::Stream(_) => {
            debug!(
                message_id,
                user = masked_user.unwrap_or(""),
                group = masked_group.unwrap_or(""),
                transport = "stream",
                response_delivery_mode = output_policy,
                "core respond stream initialized"
            );
        }
    }
}

fn masked_log_context_from_inbound(
    inbound: &platform::InboundMessage,
) -> (Option<String>, Option<String>) {
    match inbound.conversation.kind() {
        "private" | "service_account" => {
            (inbound.actor.sender_id.as_deref().map(mask_openid), None)
        }
        "group" => (None, Some(mask_openid(inbound.conversation.target_id()))),
        _ => (None, None),
    }
}

pub fn core_request_from_c2c_message(message: &C2cMessage, content: String) -> CoreRequest {
    let inbound = platform::qq_official::inbound_from_c2c(message);
    log_inbound_media_diagnostics(&inbound);
    platform::to_core_request(&inbound, content)
        .expect("QQ C2C inbound message should map to CoreRequest")
}

pub fn core_request_from_group_message(message: &GroupMessage, content: String) -> CoreRequest {
    let inbound = platform::qq_official::inbound_from_group(message);
    log_inbound_media_diagnostics(&inbound);
    platform::to_core_request(&inbound, content)
        .expect("QQ group inbound message should map to CoreRequest")
}

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

fn clean_optional(value: String) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

fn log_inbound_media_diagnostics(inbound: &platform::InboundMessage) {
    let mut image_part_count = 0usize;
    let mut file_part_count = 0usize;
    let mut image_has_remote_url = false;
    let mut image_has_media_id = false;
    let mut image_url_scheme = "none";
    let mut media_status = "none";

    for part in &inbound.input_parts {
        match part {
            MessageInputPart::Image { media } => {
                image_part_count += 1;
                image_has_remote_url |= media.remote_url().is_some();
                image_has_media_id |= has_any_media_id(
                    media.media_id.as_deref(),
                    media.file_id.as_deref(),
                    media.attachment_id.as_deref(),
                );
                image_url_scheme = media.url_scheme().as_str();
                media_status = media_status_label(media.status);
            }
            MessageInputPart::File { media } => {
                file_part_count += 1;
                media_status = media_status_label(media.status);
            }
            MessageInputPart::Text { .. } | MessageInputPart::Unknown { .. } => {}
        }
    }

    if image_part_count == 0 && file_part_count == 0 {
        return;
    }

    debug!(
        message_id = %inbound.message_id,
        platform = %inbound.platform.as_str(),
        conversation_kind = %inbound.conversation.kind(),
        input_part_count = inbound.input_parts.len(),
        image_part_count,
        file_part_count,
        image_has_remote_url,
        image_has_media_id,
        image_url_scheme,
        media_status,
        "inbound media readability diagnostics"
    );
}

fn has_any_media_id(
    media_id: Option<&str>,
    file_id: Option<&str>,
    attachment_id: Option<&str>,
) -> bool {
    [media_id, file_id, attachment_id]
        .into_iter()
        .flatten()
        .any(|value| !value.trim().is_empty())
}

fn media_status_label(status: MediaStatus) -> &'static str {
    match status {
        MediaStatus::Available => "available",
        MediaStatus::MissingReadableUrl => "missing_readable_url",
        MediaStatus::SizeExceeded => "size_exceeded",
        MediaStatus::UnsupportedType => "unsupported_type",
        MediaStatus::DownloadFailed => "download_failed",
        MediaStatus::Expired => "expired",
    }
}

pub fn build_group_respond_content(message: &GroupMessage, active_keywords: &[String]) -> String {
    let content = normalize_group_command_content(&message.content, active_keywords);
    let mut inbound = platform::qq_official::inbound_from_group(message);
    inbound.text = content.clone();
    if inbound.attachments.is_empty() {
        inbound.input_parts = if content.trim().is_empty() {
            Vec::new()
        } else {
            vec![qq_maid_common::input_part::MessageInputPart::text(
                content.clone(),
            )]
        };
    }
    platform::render_text_for_core(&inbound)
}

fn normalize_group_command_content(content: &str, active_keywords: &[String]) -> String {
    let mut candidate = content.trim_start();
    for _ in 0..4 {
        if let Some(command) = command_remainder(candidate) {
            return command;
        }
        if let Some(rest) = strip_group_command_prefix(candidate, active_keywords) {
            candidate = rest;
            continue;
        }
        break;
    }
    content.to_owned()
}

fn command_remainder(text: &str) -> Option<String> {
    let rest = trim_command_separator(text.trim_start());
    if rest.starts_with('/') {
        return Some(rest.trim().to_owned());
    }
    if let Some(command) = rest.strip_prefix('／') {
        return Some(format!("/{command}").trim().to_owned());
    }
    None
}

fn trim_command_separator(text: &str) -> &str {
    text.trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, ':' | '：' | ',' | '，'))
}

fn strip_group_command_prefix<'a>(text: &'a str, active_keywords: &[String]) -> Option<&'a str> {
    let text = text.trim_start();
    if let Some(rest) = strip_cq_at_prefix(text) {
        return Some(rest);
    }
    if let Some(rest) = strip_angle_mention_prefix(text) {
        return Some(rest);
    }
    if let Some(rest) = strip_display_mention_prefix(text) {
        return Some(rest);
    }
    strip_active_keyword_prefix(text, active_keywords)
}

fn strip_cq_at_prefix(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("[CQ:at,")?;
    let end = rest.find(']')?;
    Some(&rest[end + 1..])
}

fn strip_angle_mention_prefix(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("<@")?;
    let end = rest.find('>')?;
    Some(&rest[end + 1..])
}

fn strip_display_mention_prefix(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('@')?;
    let split_at = rest.find(char::is_whitespace)?;
    Some(&rest[split_at..])
}

fn strip_active_keyword_prefix<'a>(text: &'a str, active_keywords: &[String]) -> Option<&'a str> {
    active_keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| !keyword.is_empty())
        .find_map(|keyword| {
            text.get(..keyword.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(keyword))
                .then(|| text.get(keyword.len()..))
                .flatten()
        })
}

fn respond_error_info_to_qq_text(code: &str, stage: &str, message: &str) -> String {
    let code = code.trim();
    let stage = stage.trim();
    let safe_message = sanitize_visible_error_message(message);
    match code {
        "timeout" => "LLM 服务处理超时，请稍后再试".to_owned(),
        "config" => "LLM 服务配置未完成，请联系维护者处理".to_owned(),
        "safety_blocked" => {
            "这条消息触发了上游安全拦截，我没法按原样继续。可以换个说法再试。".to_owned()
        }
        "unsupported_input_part" => safe_message.unwrap_or_else(|| {
            "我收到图片或文件了，但当前模型暂时不支持图片/文件理解。你可以补充文字说明，我先帮你记录。".to_owned()
        }),
        "invalid_request" | "bad_request" => safe_message
            .map(|message| format!("请求格式有误：{message}"))
            .unwrap_or_else(|| "请求格式有误，请调整后再试".to_owned()),
        "not_found" => safe_message
            .map(|message| format!("没有找到相关结果：{message}"))
            .unwrap_or_else(|| "没有找到相关结果，请换个说法再试".to_owned()),
        "io_error" => "服务存储暂时不可用，请稍后再试".to_owned(),
        "provider_error" | "http_error" => "上游服务暂时不可用，请稍后再试".to_owned(),
        _ => safe_message
            .map(|message| format!("处理失败：{message}"))
            .unwrap_or_else(|| format!("处理失败（阶段：{stage}，错误码：{code}）")),
    }
}

/// 只允许把较安全、较短、且不含敏感痕迹的错误文本直接展示给 QQ 用户。
fn sanitize_visible_error_message(message: &str) -> Option<String> {
    let compact = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }

    let lower = compact.to_ascii_lowercase();
    let blocked_fragments = [
        "authorization",
        "bearer ",
        "access_token",
        "refresh_token",
        "token=",
        "secret=",
        "openid",
        "http://",
        "https://",
        "/home/",
        ".env",
        "-----begin",
    ];
    if compact.contains("sk-")
        || compact.contains('\\')
        || blocked_fragments
            .iter()
            .any(|fragment| lower.contains(fragment))
    {
        return None;
    }

    Some(truncate_visible_message(&compact, 120))
}

fn truncate_visible_message(text: &str, limit: usize) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= limit {
        return text.to_owned();
    }
    let keep = limit.saturating_sub(1);
    format!("{}…", chars.into_iter().take(keep).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{
        Attachment, C2cMessage, GroupEventType, GroupMemberRole, GroupMessage, MessageReply,
    };
    use qq_maid_core::service::{
        CoreConversation, CoreGroupMemberRole, CoreHealthSnapshot, CoreInboundClassification,
        CoreRequest, CoreRespondOutput, Platform, UpstreamStatusSnapshot,
    };

    #[derive(Default)]
    struct NoopCore;

    #[async_trait::async_trait]
    impl CoreService for NoopCore {
        async fn respond(&self, _request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
            unreachable!("respond is not used in mapping tests")
        }

        async fn classify_inbound(
            &self,
            _request: CoreRequest,
        ) -> Result<CoreInboundClassification, CoreError> {
            unreachable!("classify is not used in mapping tests")
        }

        async fn upstream_check(&self) -> Result<(), CoreError> {
            Ok(())
        }

        fn health_snapshot(&self) -> CoreHealthSnapshot {
            CoreHealthSnapshot {
                ok: true,
                provider: "test".to_owned(),
                model: "test".to_owned(),
                stream: false,
                upstream: UpstreamStatusSnapshot::default(),
            }
        }
    }

    fn c2c_message(content: &str) -> C2cMessage {
        C2cMessage {
            message_id: "m1".to_owned(),
            current_msg_idx: None,
            event_id: Some("e1".to_owned()),
            source_message_ids: vec!["m1".to_owned()],
            source_event_ids: vec!["e1".to_owned()],
            user_openid: "u1".to_owned(),
            content: content.to_owned(),
            reply: None,
            timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
            first_message_timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
            last_message_timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
            input_parts: if content.trim().is_empty() {
                Vec::new()
            } else {
                vec![qq_maid_common::input_part::MessageInputPart::text(content)]
            },
            attachments: Vec::new(),
        }
    }

    fn group_message(content: &str, member: Option<&str>) -> GroupMessage {
        GroupMessage {
            message_id: "gm1".to_owned(),
            current_msg_idx: None,
            group_openid: "g1".to_owned(),
            member_openid: member.map(str::to_owned),
            member_role: None,
            content: content.to_owned(),
            mentions: Vec::new(),
            reply: None,
            timestamp: None,
            input_parts: if content.trim().is_empty() {
                Vec::new()
            } else {
                vec![qq_maid_common::input_part::MessageInputPart::text(content)]
            },
            attachments: Vec::new(),
            event_type: GroupEventType::GroupAtMessage,
            author_is_bot: false,
            author_is_self: false,
        }
    }

    #[test]
    fn c2c_message_maps_to_private_core_request() {
        let request = core_request_from_c2c_message(&c2c_message("/todo"), "/todo".to_owned());

        assert_eq!(request.text, "/todo");
        assert_eq!(request.platform, Platform::QqOfficial);
        assert_eq!(request.actor.user_id.as_deref(), Some("u1"));
        assert_eq!(
            request.conversation,
            CoreConversation::Private {
                peer_id: "u1".to_owned()
            }
        );
    }

    #[test]
    fn group_message_maps_to_group_scope_without_member_split() {
        let request = core_request_from_group_message(
            &group_message("/rss", Some("member1")),
            "/rss".to_owned(),
        );

        assert_eq!(request.actor.user_id.as_deref(), Some("member1"));
        assert_eq!(
            request.conversation,
            CoreConversation::Group {
                group_id: "g1".to_owned()
            }
        );

        let missing_member =
            core_request_from_group_message(&group_message("/rss", None), "/rss".to_owned());
        assert_eq!(missing_member.actor.user_id, None);
        assert_eq!(
            missing_member.scope_key(),
            "platform:qq_official:account:-:group:g1"
        );
    }

    #[test]
    fn respond_client_injects_qq_account_into_scope_key() {
        let client = RespondClient::new(Arc::new(NoopCore)).with_qq_official_account_id("app-123");
        let message = c2c_message("你好");

        assert_eq!(
            client.scope_key_from_c2c_message(&message),
            "platform:qq_official:account:app-123:private:u1"
        );
        let request = client.core_request_from_c2c_message(&message, "你好".to_owned());
        assert_eq!(request.account_id.as_deref(), Some("app-123"));
        assert_eq!(request.actor.user_id.as_deref(), Some("u1"));

        let group = group_message("/rss", Some("member1"));
        assert_eq!(
            client.scope_key_from_group_message(&group),
            "platform:qq_official:account:app-123:group:g1"
        );
        let request = client.core_request_from_group_message(&group, "/rss".to_owned());
        assert_eq!(request.account_id.as_deref(), Some("app-123"));
        assert_eq!(request.actor.user_id.as_deref(), Some("member1"));
    }

    #[test]
    fn prepare_inbound_injects_account_before_core_scope_mapping() {
        let client = RespondClient::new(Arc::new(NoopCore)).with_qq_official_account_id("app-123");
        let c2c = client.prepare_inbound(platform::qq_official::inbound_from_c2c(&c2c_message(
            "你好",
        )));
        let group = client.prepare_inbound(platform::qq_official::inbound_from_group(
            &group_message("/rss", Some("member1")),
        ));

        assert_eq!(c2c.account_id.as_deref(), Some("app-123"));
        assert_eq!(
            platform::core_scope_key(&c2c).unwrap(),
            "platform:qq_official:account:app-123:private:u1"
        );
        assert_eq!(group.account_id.as_deref(), Some("app-123"));
        assert_eq!(
            platform::core_scope_key(&group).unwrap(),
            "platform:qq_official:account:app-123:group:g1"
        );
    }

    #[test]
    fn group_member_role_maps_to_core_actor() {
        let mut message = group_message("/rss add https://example.test/feed.xml", Some("member1"));
        message.member_role = Some(GroupMemberRole::Admin);

        let request = core_request_from_group_message(&message, message.content.clone());

        assert_eq!(
            request.actor.group_member_role,
            Some(CoreGroupMemberRole::Admin)
        );
        let respond: qq_maid_core::runtime::respond::RespondRequest = request.into();
        assert_eq!(respond.group_member_role.as_deref(), Some("admin"));
    }

    #[test]
    fn group_command_content_strips_platform_prefixes() {
        let keywords = vec!["召唤词".to_owned()];

        for input in [
            "@脸脸家的小女仆 /help",
            "[CQ:at,qq=123] /help",
            "<@member-1> /help",
            "@脸脸家的小女仆 ／help",
            "[CQ:at,qq=123] ／help",
            "召唤词 /rss add https://hnrss.org/newcomments",
            "召唤词：/rss",
            "召唤词：／rss",
            "召唤词： /rss \n",
            "召唤词： ／rss \n",
        ] {
            let content =
                build_group_respond_content(&group_message(input, Some("member1")), &keywords);

            assert!(
                content.starts_with('/'),
                "input should normalize to slash command: {input} -> {content}"
            );
            assert_eq!(
                content,
                content.trim(),
                "normalized command should be trimmed"
            );
        }
    }

    #[test]
    fn group_active_keyword_prefix_with_chinese_text_does_not_panic() {
        let keywords = vec!["小女仆".to_owned()];
        let content = build_group_respond_content(
            &group_message("小女仆 at你咋没响应啊", Some("member1")),
            &keywords,
        );

        assert_eq!(content, "小女仆 at你咋没响应啊");
    }

    #[test]
    fn group_non_command_content_keeps_trigger_prefix() {
        let keywords = vec!["召唤词".to_owned()];
        let content = build_group_respond_content(
            &group_message("召唤词 你在吗", Some("member1")),
            &keywords,
        );

        assert_eq!(content, "召唤词 你在吗");
    }

    #[test]
    fn quote_context_is_not_rendered_into_gateway_text_protocol() {
        let mut message = c2c_message("正文");
        message.reply = Some(MessageReply {
            message_id: "reply-1".to_owned(),
            ref_msg_idx: None,
            content: Some("被回复内容".to_owned()),
            input_parts: Vec::new(),
            media_summaries: Vec::new(),
        });
        message.attachments = vec![Attachment {
            content_type: Some("image/png".to_owned()),
            filename: Some("a.png".to_owned()),
            url: Some("https://example.test/a.png".to_owned()),
            size_bytes: None,
            media_id: None,
            file_id: None,
            attachment_id: None,
        }];
        message
            .input_parts
            .push(message.attachments[0].to_input_part("qq_official"));

        let content = build_respond_content(&message);

        assert!(content.starts_with("正文"));
        assert!(content.contains("[图片 image/png: a.png]"));
    }

    #[test]
    fn inbound_log_context_masks_private_user() {
        let inbound = platform::qq_official::inbound_from_c2c(&c2c_message("你好"));

        let (user, group) = masked_log_context_from_inbound(&inbound);

        assert_eq!(user.as_deref(), Some("******"));
        assert_eq!(group, None);
    }

    #[test]
    fn inbound_log_context_masks_wechat_service_user() {
        let inbound = platform::wechat_service::inbound_from_text_message(
            &platform::wechat_service::WechatTextMessage {
                to_user_name: "gh_service".to_owned(),
                from_user_name: "wechat_user_openid_abcdef".to_owned(),
                create_time: Some("1460537339".to_owned()),
                content: "你好".to_owned(),
                msg_id: "msg-1".to_owned(),
            },
        );

        let (user, group) = masked_log_context_from_inbound(&inbound);

        assert_eq!(user.as_deref(), Some("******abcdef"));
        assert_ne!(user.as_deref(), Some("wechat_user_openid_abcdef"));
        assert_eq!(group, None);
    }

    #[test]
    fn inbound_log_context_masks_group_target_without_member_user() {
        let mut message = group_message("你好", Some("member_openid_abcdef"));
        message.group_openid = "group_openid_123456".to_owned();
        let inbound = platform::qq_official::inbound_from_group(&message);

        let (user, group) = masked_log_context_from_inbound(&inbound);

        assert_eq!(user, None);
        assert_eq!(group.as_deref(), Some("******123456"));
        assert_ne!(group.as_deref(), Some("group_openid_123456"));
    }

    #[test]
    fn unsafe_error_detail_is_not_shown_to_user() {
        let _response = RespondResponse {
            text: None,
            markdown: None,
            handled: Some(false),
            session_id: None,
            command: None,
            diagnostics: None,
        };

        let text = respond_error_to_qq_text(&RespondError::Core(CoreError::new(
            "bad_request",
            "provider",
            "Authorization Bearer sk-secret token leaked",
        )));

        assert_eq!(text, "请求格式有误，请调整后再试");
        assert!(!text.contains("sk-secret"));
    }
}

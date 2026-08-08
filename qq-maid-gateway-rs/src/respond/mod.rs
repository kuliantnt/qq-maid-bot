//! Gateway 到 Core 的进程内响应边界。
//!
//! 本模块只负责 Gateway 入站消息到 `CoreRequest` 的映射、内容拼接和安全错误文案。
//! 不再保留 HTTP、JSON DTO 或 SSE 解析，避免同进程组件之间出现第二套传输协议。

use std::sync::Arc;

use qq_maid_common::{
    command_prefix::CommandPrefix,
    input_part::{MediaStatus, MessageInputPart},
};
#[cfg(test)]
use qq_maid_core::service::CoreResponse;
use qq_maid_core::service::{
    CoreError, CoreInboundClassification, CoreRequest, CoreRespondOutput, CoreService,
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
                    .text_content()
                    .map(|text| text.chars().count())
                    .unwrap_or(0),
                transport = "complete",
                response_delivery_mode = output_policy,
                "Core 回复请求成功"
            );
        }
        CoreRespondOutput::Stream(_) => {
            debug!(
                message_id,
                user = masked_user.unwrap_or(""),
                group = masked_group.unwrap_or(""),
                transport = "stream",
                response_delivery_mode = output_policy,
                "Core 回复流已初始化"
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
        "入站媒体可读性诊断"
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
    build_group_respond_content_with_prefix(message, active_keywords, CommandPrefix::default())
}

pub(crate) fn build_group_respond_content_with_prefix(
    message: &GroupMessage,
    active_keywords: &[String],
    command_prefix: CommandPrefix,
) -> String {
    let inbound = normalized_group_inbound_with_prefix(message, active_keywords, command_prefix);
    platform::render_text_for_core(&inbound)
}

/// 群聊本地命令只允许使用用户本轮显式正文。ARK、平行消息和聊天记录的安全摘要
/// 会进入 `input_parts` 供 Core 理解，但绝不能被 Gateway 当成用户输入的 slash 命令。
pub(crate) fn build_group_command_content_with_prefix(
    message: &GroupMessage,
    active_keywords: &[String],
    command_prefix: CommandPrefix,
) -> String {
    normalize_group_addressed_content(message, &message.content, active_keywords, command_prefix)
}

#[cfg(test)]
pub(crate) fn normalized_group_inbound(
    message: &GroupMessage,
    active_keywords: &[String],
) -> platform::InboundMessage {
    normalized_group_inbound_with_prefix(message, active_keywords, CommandPrefix::default())
}

pub(crate) fn normalized_group_inbound_with_prefix(
    message: &GroupMessage,
    active_keywords: &[String],
    command_prefix: CommandPrefix,
) -> platform::InboundMessage {
    let content = normalize_group_addressed_content(
        message,
        &message.content,
        active_keywords,
        command_prefix,
    );
    let mut inbound = platform::qq_official::inbound_from_group(message);
    inbound.text = content.clone();
    // Core 只消费平台无关的寻址事实。QQ 结构化 @ 已由 adapter 标记，Active 模式
    // 的配置唤醒词在归一化边界补充，不能让 Core 理解 GROUP_ACTIVE_KEYWORDS。
    inbound.mentioned_bot |=
        crate::gateway::contains_active_keyword(&message.content, active_keywords);

    // 有序内容块存在时 Core 会优先使用 input_parts。寻址 mention 只改写正文文本块，
    // 因此仅同步首个正文文本块，媒体块及其相对顺序、状态和元数据保持原样。
    if content != message.content
        && let Some(MessageInputPart::Text { text, .. }) = inbound.input_parts.first_mut()
    {
        *text = normalize_group_addressed_content(message, text, active_keywords, command_prefix);
        if text.is_empty() {
            inbound.input_parts.remove(0);
        }
    }

    inbound
}

fn normalize_group_addressed_content(
    message: &GroupMessage,
    content: &str,
    active_keywords: &[String],
    command_prefix: CommandPrefix,
) -> String {
    let mut candidate = content.trim_start();
    let mut stripped_address = false;
    let mut mention_index = 0usize;
    let mut stripped_mention = false;
    for _ in 0..4 {
        if let Some(command) = command_remainder(candidate, command_prefix) {
            return command;
        }
        if let Some((rest, prefix_kind)) = strip_group_command_prefix(
            candidate,
            message,
            active_keywords,
            mention_index,
            stripped_mention,
        ) {
            candidate = rest;
            stripped_address = true;
            if prefix_kind == GroupAddressPrefixKind::Mention {
                mention_index += 1;
                stripped_mention = true;
            }
            continue;
        }
        break;
    }
    if let Some(rest) = strip_group_command_suffix(candidate, message, active_keywords) {
        candidate = rest;
        stripped_address = true;
    }
    if stripped_address {
        if let Some(command) = command_remainder(candidate, command_prefix) {
            return command;
        }
        if command_prefix.as_char() != '/' && candidate.trim_start().starts_with('/') {
            // 自定义前缀启用后，`@机器人 /help` 只是普通正文；保留原始寻址文本，避免
            // Gateway 后续把剥离后的 `/help` 误判成已经规范化的配置命令。
            return content.to_owned();
        }
        trim_command_separator(candidate.trim_start())
            .trim()
            .to_owned()
    } else {
        content.to_owned()
    }
}

fn command_remainder(text: &str, command_prefix: CommandPrefix) -> Option<String> {
    let rest = trim_command_separator(text.trim_start());
    if command_prefix.is_candidate(rest) {
        // Core 负责把配置前缀规范化为内部 `/`；Gateway 这里只剥离 @/唤醒词，
        // 必须保留配置字符，避免跨层重复规范化后被当成旧前缀普通文本。
        return Some(rest.trim().to_owned());
    }
    if command_prefix.as_char() == '/'
        && let Some(command) = rest.strip_prefix('／')
    {
        return Some(format!("/{command}").trim().to_owned());
    }
    None
}

fn trim_command_separator(text: &str) -> &str {
    text.trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, ':' | '：' | ',' | '，'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupAddressPrefixKind {
    Mention,
    ActiveKeyword,
}

fn strip_group_command_prefix<'a>(
    text: &'a str,
    message: &GroupMessage,
    active_keywords: &[String],
    mention_index: usize,
    stripped_mention: bool,
) -> Option<(&'a str, GroupAddressPrefixKind)> {
    let text = text.trim_start();
    if let Some((rest, target_id)) = strip_cq_at_prefix(text)
        && can_strip_encoded_mention(message, mention_index, stripped_mention, target_id)
    {
        return Some((rest, GroupAddressPrefixKind::Mention));
    }
    if let Some((rest, target_id)) = strip_angle_mention_prefix(text)
        && can_strip_encoded_mention(message, mention_index, stripped_mention, target_id)
    {
        return Some((rest, GroupAddressPrefixKind::Mention));
    }
    if let Some((rest, display_name)) = strip_display_mention_prefix(text)
        && can_strip_display_mention(message, active_keywords, mention_index, display_name)
    {
        return Some((rest, GroupAddressPrefixKind::Mention));
    }
    strip_active_keyword_prefix(text, active_keywords)
        .map(|rest| (rest, GroupAddressPrefixKind::ActiveKeyword))
}

fn strip_group_command_suffix<'a>(
    text: &'a str,
    message: &GroupMessage,
    active_keywords: &[String],
) -> Option<&'a str> {
    let text = text.trim_end();
    if let Some((rest, target_id)) = strip_cq_at_suffix(text)
        && can_strip_encoded_mention_suffix(message, target_id)
    {
        return Some(trim_group_address_suffix(rest));
    }
    if let Some((rest, target_id)) = strip_angle_mention_suffix(text)
        && can_strip_encoded_mention_suffix(message, target_id)
    {
        return Some(trim_group_address_suffix(rest));
    }
    if let Some((rest, display_name)) = strip_display_mention_suffix(text)
        && can_strip_display_mention_suffix(message, active_keywords, display_name)
    {
        return Some(trim_group_address_suffix(rest));
    }
    None
}

fn strip_cq_at_prefix(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("[CQ:at,")?;
    let end = rest.find(']')?;
    let attributes = &rest[..end];
    let target_id = attributes
        .split(',')
        .find_map(|attribute| attribute.strip_prefix("qq="))?;
    Some((&rest[end + 1..], target_id))
}

fn strip_angle_mention_prefix(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("<@")?;
    let end = rest.find('>')?;
    let target_id = rest[..end].strip_prefix('!').unwrap_or(&rest[..end]);
    if target_id.trim().is_empty() {
        return None;
    }
    Some((&rest[end + 1..], target_id))
}

fn strip_display_mention_prefix(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix('@')?;
    let split_at = rest.find(is_group_address_separator)?;
    Some((&rest[split_at..], &rest[..split_at]))
}

fn strip_cq_at_suffix(text: &str) -> Option<(&str, &str)> {
    let start = text.rfind("[CQ:at,")?;
    let (rest, target_id) = strip_cq_at_prefix(&text[start..])?;
    rest.is_empty().then_some((&text[..start], target_id))
}

fn strip_angle_mention_suffix(text: &str) -> Option<(&str, &str)> {
    let start = text.rfind("<@")?;
    let (rest, target_id) = strip_angle_mention_prefix(&text[start..])?;
    rest.is_empty().then_some((&text[..start], target_id))
}

fn strip_display_mention_suffix(text: &str) -> Option<(&str, &str)> {
    let start = text.rfind('@')?;
    let display_name = text[start + 1..].trim();
    (!display_name.is_empty() && !display_name.chars().any(char::is_whitespace))
        .then_some((&text[..start], display_name))
}

fn can_strip_encoded_mention(
    message: &GroupMessage,
    mention_index: usize,
    stripped_mention: bool,
    target_id: &str,
) -> bool {
    if let Some(mention) = message.mentions.get(mention_index) {
        return mention.is_current_bot
            && mention
                .target_id
                .as_deref()
                .is_none_or(|expected| expected == target_id);
    }
    !stripped_mention && message.event_type == crate::event::GroupEventType::GroupAtMessage
}

fn can_strip_display_mention(
    message: &GroupMessage,
    active_keywords: &[String],
    mention_index: usize,
    display_name: &str,
) -> bool {
    if let Some(mention) = message.mentions.get(mention_index) {
        return mention.is_current_bot;
    }
    // 缺少结构化身份时只兼容已配置的机器人展示名，不能把任意 @群成员当作寻址前缀。
    active_keywords.iter().any(|keyword| {
        let keyword = keyword.trim();
        !keyword.is_empty() && display_name.eq_ignore_ascii_case(keyword)
    })
}

fn can_strip_encoded_mention_suffix(message: &GroupMessage, target_id: &str) -> bool {
    if let Some(mention) = message.mentions.last() {
        return mention.is_current_bot
            && mention
                .target_id
                .as_deref()
                .is_none_or(|expected| expected == target_id);
    }
    message.event_type == crate::event::GroupEventType::GroupAtMessage
}

fn can_strip_display_mention_suffix(
    message: &GroupMessage,
    active_keywords: &[String],
    display_name: &str,
) -> bool {
    if let Some(mention) = message.mentions.last() {
        return mention.is_current_bot;
    }
    active_keywords.iter().any(|keyword| {
        let keyword = keyword.trim();
        !keyword.is_empty() && display_name.eq_ignore_ascii_case(keyword)
    })
}

fn trim_group_address_suffix(text: &str) -> &str {
    text.trim_end_matches(is_group_address_separator)
}

fn strip_active_keyword_prefix<'a>(text: &'a str, active_keywords: &[String]) -> Option<&'a str> {
    active_keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| !keyword.is_empty())
        .find_map(|keyword| {
            let rest = text
                .get(..keyword.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(keyword))
                .then(|| text.get(keyword.len()..))
                .flatten()?;
            (rest.is_empty()
                || rest.starts_with('/')
                || rest.starts_with('／')
                || rest.starts_with(is_group_address_separator))
            .then_some(rest)
        })
}

fn is_group_address_separator(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, ':' | '：' | ',' | '，')
}

fn respond_error_info_to_qq_text(code: &str, stage: &str, message: &str) -> String {
    let code = code.trim();
    let stage = stage.trim();
    let safe_message = sanitize_visible_error_message(message);
    match code {
        "timeout" => "LLM 请求超时，请稍后重试。".to_owned(),
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
        "authentication_failed" => "LLM 服务鉴权失败，请联系维护者处理。".to_owned(),
        "rate_limited" => "LLM 请求受到限流，请稍后重试。".to_owned(),
        "network_error" | "http_error" => "LLM 网络连接失败，请稍后重试。".to_owned(),
        "upstream_unavailable" | "provider_error" => {
            "上游服务暂时不可用，请稍后再试".to_owned()
        }
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
mod tests;

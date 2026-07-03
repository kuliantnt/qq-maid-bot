//! Gateway 到 Core 的进程内响应边界。
//!
//! 本模块只负责 QQ 平台消息到 `CoreRequest` 的映射、内容拼接和安全错误文案。
//! 不再保留 HTTP、JSON DTO 或 SSE 解析，避免同进程组件之间出现第二套传输协议。

use std::sync::Arc;

use qq_maid_core::service::{
    CoreActor, CoreConversation, CoreError, CoreInboundClassification, CoreRequest,
    CoreRespondOutput, CoreResponse, CoreResponseEvent, CoreResponseStream, CoreService, Platform,
};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::{
    event::{C2cMessage, GroupMessage},
    logging::mask_openid,
};

#[derive(Clone)]
pub struct RespondClient {
    core: Arc<dyn CoreService>,
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
        Self { core }
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
        let request = core_request_from_c2c_message(message, content);
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
        let request = core_request_from_c2c_message(message, content);
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
        let request = core_request_from_group_message(message, content);
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
                "core respond request succeeded"
            );
        }
        CoreRespondOutput::Stream(_) => {
            debug!(
                message_id,
                user = masked_user.unwrap_or(""),
                group = masked_group.unwrap_or(""),
                transport = "stream",
                "core respond stream initialized"
            );
        }
    }
}

pub fn core_request_from_c2c_message(message: &C2cMessage, content: String) -> CoreRequest {
    CoreRequest {
        text: content,
        platform: Platform::QqOfficial,
        actor: CoreActor {
            user_id: Some(message.user_openid.clone()),
        },
        conversation: CoreConversation::Private {
            peer_id: message.user_openid.clone(),
        },
    }
}

pub fn core_request_from_group_message(message: &GroupMessage, content: String) -> CoreRequest {
    CoreRequest {
        text: content,
        platform: Platform::QqOfficial,
        actor: CoreActor {
            user_id: message.member_openid.clone(),
        },
        // 群聊 scope 由 Core 根据 group_id 派生，成员身份只放在 actor 中；
        // 不能把群会话拆成每个成员独立 session。
        conversation: CoreConversation::Group {
            group_id: message.group_openid.clone(),
        },
    }
}

/// Gateway 侧需要在入队前拿到与 Core 完全一致的 scope_key，用于会话串行调度和 reply cache 隔离。
pub fn scope_key_from_c2c_message(message: &C2cMessage) -> String {
    core_request_from_c2c_message(message, String::new()).scope_key()
}

/// 群聊 scope 直接复用 Core 的 `group:{group_id}` 规则，避免 Gateway 自己维护第二套会话边界。
pub fn scope_key_from_group_message(message: &GroupMessage) -> String {
    core_request_from_group_message(message, String::new()).scope_key()
}

/// Egress 层是 gateway 内唯一允许拼接 Core 文本协议的位置。
/// 这里把 reply block 和附件备注按既有协议收口，避免平台字段污染 Core 稳定模型。
pub fn build_respond_content(message: &C2cMessage) -> String {
    build_respond_content_parts(
        &message.content,
        message.reply.as_ref(),
        &message.attachments,
    )
}

pub fn build_group_respond_content(message: &GroupMessage, active_keywords: &[String]) -> String {
    let content = normalize_group_command_content(&message.content, active_keywords);
    build_respond_content_parts(&content, message.reply.as_ref(), &message.attachments)
}

fn normalize_group_command_content(content: &str, active_keywords: &[String]) -> String {
    let mut candidate = content.trim_start();
    for _ in 0..4 {
        if let Some(command) = command_remainder(candidate) {
            return command.to_owned();
        }
        if let Some(rest) = strip_group_command_prefix(candidate, active_keywords) {
            candidate = rest;
            continue;
        }
        break;
    }
    content.to_owned()
}

fn command_remainder(text: &str) -> Option<&str> {
    let rest = trim_command_separator(text.trim_start());
    if rest.starts_with('/') {
        return Some(rest.trim());
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
    let lower = text.to_ascii_lowercase();
    active_keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| !keyword.is_empty())
        .find_map(|keyword| {
            let keyword_lower = keyword.to_ascii_lowercase();
            lower
                .starts_with(&keyword_lower)
                .then_some(&text[keyword.len()..])
        })
}

fn build_respond_content_parts(
    message_content: &str,
    reply: Option<&crate::event::MessageReply>,
    attachments: &[crate::event::Attachment],
) -> String {
    let mut content = String::new();
    let Some(reply) = reply else {
        content.push_str(message_content);
        append_attachment_notes(&mut content, attachments);
        return content;
    };

    content.push_str(&format!("[reply message_id={}]\n", reply.message_id));
    if let Some(reply_content) = reply.content.as_deref() {
        content.push_str(reply_content);
    }
    content.push_str("\n[/reply]\n");
    content.push_str(message_content);
    append_attachment_notes(&mut content, attachments);
    content
}

fn append_attachment_notes(content: &mut String, attachments: &[crate::event::Attachment]) {
    for attachment in attachments {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&attachment.note());
    }
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
    use crate::event::{Attachment, C2cMessage, GroupEventType, GroupMessage, MessageReply};
    use qq_maid_core::service::{CoreConversation, Platform};

    fn c2c_message(content: &str) -> C2cMessage {
        C2cMessage {
            message_id: "m1".to_owned(),
            event_id: Some("e1".to_owned()),
            source_message_ids: vec!["m1".to_owned()],
            source_event_ids: vec!["e1".to_owned()],
            user_openid: "u1".to_owned(),
            content: content.to_owned(),
            reply: None,
            timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
            first_message_timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
            last_message_timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
            attachments: Vec::new(),
        }
    }

    fn group_message(content: &str, member: Option<&str>) -> GroupMessage {
        GroupMessage {
            message_id: "gm1".to_owned(),
            group_openid: "g1".to_owned(),
            member_openid: member.map(str::to_owned),
            content: content.to_owned(),
            mention_ids: Vec::new(),
            reply: None,
            timestamp: None,
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
        assert_eq!(missing_member.scope_key(), "group:g1");
    }

    #[test]
    fn group_command_content_strips_platform_prefixes() {
        let keywords = vec!["召唤词".to_owned()];

        for input in [
            "@脸脸家的小女仆 /help",
            "[CQ:at,qq=123] /help",
            "<@member-1> /help",
            "召唤词 /rss add https://hnrss.org/newcomments",
            "召唤词：/rss",
            "召唤词： /rss \n",
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
    fn group_non_command_content_keeps_trigger_prefix() {
        let keywords = vec!["召唤词".to_owned()];
        let content = build_group_respond_content(
            &group_message("召唤词 你在吗", Some("member1")),
            &keywords,
        );

        assert_eq!(content, "召唤词 你在吗");
    }

    #[test]
    fn reply_block_and_attachment_notes_keep_existing_text_protocol() {
        let mut message = c2c_message("正文");
        message.reply = Some(MessageReply {
            message_id: "reply-1".to_owned(),
            content: Some("被回复内容".to_owned()),
        });
        message.attachments = vec![Attachment {
            content_type: Some("image/png".to_owned()),
            filename: Some("a.png".to_owned()),
            url: Some("https://example.test/a.png".to_owned()),
        }];

        let content = build_respond_content(&message);

        assert!(content.starts_with("[reply message_id=reply-1]\n被回复内容\n[/reply]\n正文"));
        assert!(content.contains("[附件 image/png: a.png https://example.test/a.png]"));
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

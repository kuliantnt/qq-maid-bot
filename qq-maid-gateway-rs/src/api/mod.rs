use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use reqwest::StatusCode;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;
use tracing::{info, trace, warn};

mod image;
mod response;
mod voice;

pub use voice::{VoiceMedia, build_c2c_voice_payload, build_group_voice_payload};

#[cfg(test)]
use response::extract_sent_message_id;
use response::{extract_c2c_stream_response, extract_sent_message_ids, qq_api_error_fields};

use crate::{
    auth::{AccessTokenManager, AuthError},
    logging::{mask_identifier, mask_openid, reqwest_error_summary},
    markdown::{MarkdownPayload, build_c2c_markdown_payload, build_group_markdown_payload},
    media::ImagePayload,
    render::OutboundMessage,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct C2cReplyTarget {
    pub user_openid: String,
    pub msg_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupReplyTarget {
    pub group_openid: String,
    pub msg_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QqApiClient {
    client: reqwest::Client,
    api_base: String,
    auth: AccessTokenManager,
    msg_seq: Arc<AtomicU64>,
    /// 群成员详情 TTL 缓存（#319），Arc 共享，Clone 后同一缓存。
    member_cache: member_detail::MemberDetailCache,
}

#[derive(Debug, Error)]
pub enum ApiError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("QQ OpenAPI request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("QQ OpenAPI returned {status}")]
    Status { status: StatusCode, body: String },
    #[error("{0} sending is not supported by this sender")]
    Unsupported(&'static str),
    #[error("invalid media payload: {0}")]
    InvalidMedia(&'static str),
    #[error("QQ voice URL upload failed")]
    VoiceUpload(#[source] Box<ApiError>),
    #[error("QQ voice message send failed")]
    VoiceSend(#[source] Box<ApiError>),
    #[error("invalid QQ C2C stream response: {0}")]
    InvalidStreamResponse(&'static str),
}

impl ApiError {
    pub fn log_summary(&self) -> String {
        match self {
            Self::Auth(_) => "QQ auth error".to_owned(),
            Self::Http(error) => reqwest_error_summary(error),
            Self::Status { status, body } => {
                let summary = qq_api_error_body_summary(body);
                if summary.is_empty() {
                    format!("http status {status}")
                } else {
                    format!("http status {status}: {summary}")
                }
            }
            Self::Unsupported(kind) => format!("{kind} sending is unsupported"),
            Self::InvalidMedia(reason) => format!("invalid media payload: {reason}"),
            Self::VoiceUpload(source) => format!("voice_upload: {}", source.log_summary()),
            Self::VoiceSend(source) => format!("voice_send: {}", source.log_summary()),
            Self::InvalidStreamResponse(reason) => {
                format!("invalid QQ C2C stream response: {reason}")
            }
        }
    }
}

/// QQ 错误响应只保留短摘要用于诊断，避免把完整响应体或潜在敏感字段写入日志。
fn qq_api_error_body_summary(body: &str) -> String {
    const MAX_CHARS: usize = 200;
    let (code, message) = qq_api_error_fields(body);
    let mut summary = match (code, message) {
        (Some(code), Some(message)) => format!("code={code} message={message}"),
        (Some(code), None) => format!("code={code}"),
        (None, Some(message)) => format!("message={message}"),
        (None, None) => format!(
            "unparseable QQ error response ({} chars)",
            body.chars().count()
        ),
    };
    if summary.chars().count() > MAX_CHARS {
        summary = summary.chars().take(MAX_CHARS).collect::<String>();
        summary.push('…');
    }
    summary
}

#[derive(Debug, Serialize)]
struct C2cTextPayload<'a> {
    content: &'a str,
    msg_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

#[derive(Debug, Serialize)]
struct C2cInputNotify {
    input_type: u8,
    input_second: u32,
}

#[derive(Debug, Serialize)]
struct C2cTypingPayload<'a> {
    msg_type: u8,
    input_notify: C2cInputNotify,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

#[derive(Debug, Serialize)]
struct GroupTextPayload<'a> {
    content: &'a str,
    msg_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SendMessageIds {
    /// QQ OpenAPI 返回的真实平台消息 ID，用于 outbound cache、去重和平台消息操作。
    pub message_id: Option<String>,
    /// QQ 引用上下文索引，如 `REFIDX_*`，只用于 ref_index/quoted lookup。
    pub ref_index_id: Option<String>,
}

impl SendMessageIds {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn message_id(message_id: impl Into<String>) -> Self {
        Self {
            message_id: Some(message_id.into()),
            ref_index_id: None,
        }
    }

    pub fn ref_index_id(ref_index_id: impl Into<String>) -> Self {
        Self {
            message_id: None,
            ref_index_id: Some(ref_index_id.into()),
        }
    }

    pub fn ref_index_lookup_id(&self) -> Option<&str> {
        self.ref_index_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

pub type SendResult = Result<SendMessageIds, ApiError>;
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = SendResult> + Send + 'a>>;

/// 官方 C2C StreamSession 请求载荷。
///
/// `/stream_messages` 的 replace 模式接收累计全文；它与普通 `/messages` 的
/// `msg_type/markdown` 载荷不同，不能把模型 delta 放进 `content_raw`，也不能携带
/// 旧协议的 `stream` 对象。
#[derive(Debug, Serialize)]
struct C2cStreamPayload<'a> {
    input_mode: &'static str,
    input_state: u8,
    content_type: &'static str,
    content_raw: &'a str,
    event_id: &'a str,
    msg_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_msg_id: Option<&'a str>,
    msg_seq: u32,
    index: u32,
}

/// 一次官方流式请求成功后返回的消息信息。
///
/// 官方 StreamSession 以首个成功响应的 `id` 作为后续 `stream_msg_id`，而
/// `ext_info.ref_idx` 是可写入引用索引的独立字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct C2cStreamResponse {
    pub(crate) message_id: String,
    pub(crate) ref_index_id: Option<String>,
}

/// 官方 StreamSession 的传输游标。
///
/// 正文、生命周期和发送所有权由 Gateway 状态机维护；这里仅维护官方请求需要的
/// `stream_msg_id/msg_seq/index`，且只在 QQ 成功响应后推进。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct C2cStreamTransportState {
    pub(crate) stream_msg_id: Option<String>,
    pub(crate) msg_seq: Option<u32>,
    pub(crate) index: u32,
}

impl C2cStreamTransportState {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

/// 官方 C2C 流式发送结果；成功响应必须包含平台返回的消息 ID。
pub(crate) type StreamSendResult = Result<C2cStreamResponse, ApiError>;

pub trait OutboundSender: Send + Sync {
    fn send_text<'a>(&'a self, target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a>;
    fn send_markdown<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a>;
    fn send_image<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        image: &'a ImagePayload,
    ) -> SendFuture<'a>;
    fn send_voice_url<'a>(
        &'a self,
        _target: &'a C2cReplyTarget,
        _audio_url: &'a str,
    ) -> SendFuture<'a> {
        Box::pin(async { Err(ApiError::Unsupported("voice")) })
    }
}

pub trait GroupOutboundSender: Send + Sync {
    fn send_text<'a>(&'a self, target: &'a GroupReplyTarget, text: &'a str) -> SendFuture<'a>;
    fn send_markdown<'a>(
        &'a self,
        target: &'a GroupReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a>;
    fn send_image<'a>(
        &'a self,
        _target: &'a GroupReplyTarget,
        _image: &'a ImagePayload,
    ) -> SendFuture<'a> {
        Box::pin(async { Err(ApiError::Unsupported("image")) })
    }
    fn send_voice_url<'a>(
        &'a self,
        _target: &'a GroupReplyTarget,
        _audio_url: &'a str,
    ) -> SendFuture<'a> {
        Box::pin(async { Err(ApiError::Unsupported("voice")) })
    }
}

impl QqApiClient {
    pub fn new(
        client: reqwest::Client,
        api_base: impl Into<String>,
        auth: AccessTokenManager,
    ) -> Self {
        Self {
            client,
            api_base: api_base.into().trim_end_matches('/').to_owned(),
            auth,
            msg_seq: Arc::new(AtomicU64::new(0)),
            member_cache: member_detail::MemberDetailCache::default_ttl(),
        }
    }

    pub fn next_msg_seq(&self) -> u32 {
        let value = self.msg_seq.fetch_add(1, Ordering::Relaxed);
        (value % 10_000 + 1) as u32
    }

    pub async fn send_c2c_text(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        text: &str,
    ) -> SendResult {
        let payload = build_c2c_text_payload(text, msg_id, self.next_msg_seq());
        self.post_c2c_message(user_openid, msg_id, "text", &payload)
            .await
    }

    pub async fn send_c2c_typing(&self, user_openid: &str, msg_id: Option<&str>) -> SendResult {
        let payload = build_c2c_typing_payload(msg_id, self.next_msg_seq());
        self.post_c2c_message(user_openid, msg_id, "typing", &payload)
            .await
    }

    pub async fn send_group_text(
        &self,
        group_openid: &str,
        msg_id: Option<&str>,
        text: &str,
    ) -> SendResult {
        let payload = build_group_text_payload(text, msg_id, self.next_msg_seq());
        self.post_group_message(group_openid, msg_id, "text", &payload)
            .await
    }

    pub async fn send_group_markdown(
        &self,
        group_openid: &str,
        msg_id: Option<&str>,
        markdown: &MarkdownPayload,
    ) -> SendResult {
        let payload = build_group_markdown_payload(markdown, msg_id, self.next_msg_seq());
        self.post_group_message(group_openid, msg_id, "markdown", &payload)
            .await
    }

    pub async fn send_c2c_markdown(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        markdown: &MarkdownPayload,
    ) -> SendResult {
        let payload = build_c2c_markdown_payload(markdown, msg_id, self.next_msg_seq());
        self.post_c2c_message(user_openid, msg_id, "markdown", &payload)
            .await
    }

    /// 通过官方 `/stream_messages` 发送一次累计全文更新或完成请求。
    ///
    /// `input_state=1` 表示持续生成，`input_state=10` 表示完成。所有请求都携带同一
    /// `msg_seq`，首个成功响应的 `id` 作为后续 `stream_msg_id`；失败请求不会推进
    /// `index`、会话 ID 或成功正文。
    pub(crate) async fn send_c2c_stream_message(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        content_raw: &str,
        stream_state: &mut C2cStreamTransportState,
        input_state: u8,
    ) -> StreamSendResult {
        let msg_id = msg_id.ok_or(ApiError::InvalidStreamResponse(
            "passive C2C stream requires source msg_id",
        ))?;
        let msg_seq = stream_state.msg_seq.unwrap_or_else(|| self.next_msg_seq());
        let payload =
            build_c2c_stream_payload(content_raw, msg_id, msg_seq, stream_state, input_state);
        self.post_c2c_stream_message(user_openid, msg_id, stream_state, msg_seq, &payload)
            .await
    }

    /// 官方 StreamSession 的底层 HTTP POST。
    async fn post_c2c_stream_message(
        &self,
        user_openid: &str,
        msg_id: &str,
        stream_state: &mut C2cStreamTransportState,
        msg_seq: u32,
        payload: &Value,
    ) -> StreamSendResult {
        let url = format!("{}/v2/users/{user_openid}/stream_messages", self.api_base);
        let masked_user = mask_openid(user_openid);
        let masked_message_id = mask_identifier(msg_id);
        let input_state = payload
            .get("input_state")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let index = payload
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let content_chars = stream_payload_content_chars(payload);
        let has_stream_msg_id = stream_state.stream_msg_id.is_some();
        let response = self
            .client
            .post(url)
            .header("Authorization", self.auth.authorization_header().await?)
            .json(payload)
            .send()
            .await
            .map_err(|error| {
                warn!(
                    user = %masked_user,
                    source_message_id = %masked_message_id,
                    endpoint = "stream_messages",
                    input_state,
                    index,
                    msg_seq,
                    has_stream_msg_id,
                    content_chars,
                    http_status = "",
                    qq_code = "",
                    qq_message = "",
                    index_committed = false,
                    msg_seq_committed = false,
                    error = %reqwest_error_summary(&error),
                    "QQ 官方 C2C 流式请求失败"
                );
                ApiError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let (qq_code, qq_message) = qq_api_error_fields(&body);
            warn!(
                user = %masked_user,
                source_message_id = %masked_message_id,
                endpoint = "stream_messages",
                input_state,
                index,
                msg_seq,
                has_stream_msg_id,
                content_chars,
                http_status = %status,
                qq_code = qq_code.as_deref().unwrap_or(""),
                qq_message = qq_message.as_deref().unwrap_or(""),
                index_committed = false,
                msg_seq_committed = false,
                error_summary = %qq_api_error_body_summary(&body),
                "QQ 官方 C2C 流式请求返回非成功状态码"
            );
            return Err(ApiError::Status { status, body });
        }

        let body = response.text().await.map_err(ApiError::Http)?;
        let Some((message_id, ref_index_id)) = extract_c2c_stream_response(&body) else {
            warn!(
                user = %masked_user,
                source_message_id = %masked_message_id,
                endpoint = "stream_messages",
                input_state,
                index,
                msg_seq,
                has_stream_msg_id,
                content_chars,
                http_status = %status,
                "QQ 官方 C2C 流式成功响应缺少消息 id"
            );
            return Err(ApiError::InvalidStreamResponse("missing response id"));
        };
        let (qq_code, qq_message) = qq_api_error_fields(&body);
        trace!(
            user = %masked_user,
            source_message_id = %masked_message_id,
            endpoint = "stream_messages",
            input_state,
            index,
            msg_seq,
            has_stream_msg_id,
            content_chars,
            http_status = %status,
            qq_code = qq_code.as_deref().unwrap_or(""),
            qq_message = qq_message.as_deref().unwrap_or(""),
            returned_message_id = %mask_identifier(&message_id),
            returned_ref_index_id = %ref_index_id.as_deref().map(mask_identifier).unwrap_or_default(),
            "QQ 官方 C2C 流式请求成功"
        );
        // 只有状态码、响应体和消息 ID 都确认成功后，才推进 StreamSession 游标。
        stream_state.msg_seq.get_or_insert(msg_seq);
        if stream_state.stream_msg_id.is_none() {
            stream_state.stream_msg_id = Some(message_id.clone());
        }
        stream_state.index = stream_state.index.saturating_add(1);
        Ok(C2cStreamResponse {
            message_id,
            ref_index_id,
        })
    }

    async fn post_c2c_message(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        message_type: &'static str,
        payload: &Value,
    ) -> SendResult {
        let url = format!("{}/v2/users/{user_openid}/messages", self.api_base);
        let masked_user = mask_openid(user_openid);
        let response = self
            .client
            .post(url)
            .header("Authorization", self.auth.authorization_header().await?)
            .json(payload)
            .send()
            .await
            .map_err(|error| {
                warn!(
                    user = %masked_user,
                    source_message_id = msg_id.unwrap_or(""),
                    message_type = message_type,
                    error = %reqwest_error_summary(&error),
                    "QQ 发送请求失败"
                );
                ApiError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            warn!(
                user = %masked_user,
                source_message_id = msg_id.unwrap_or(""),
                message_type = message_type,
                status = %status,
                "QQ 发送返回非成功状态码"
            );
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::Status { status, body });
        }

        let body = response.text().await.map_err(ApiError::Http)?;
        let sent_ids = extract_sent_message_ids(&body);
        info!(
            user = %masked_user,
            source_message_id = msg_id.unwrap_or(""),
            sent_message_id = sent_ids.message_id.as_deref().unwrap_or(""),
            sent_ref_index_id = sent_ids.ref_index_id.as_deref().unwrap_or(""),
            message_type = message_type,
            "QQ 发送成功"
        );
        Ok(sent_ids)
    }

    async fn post_group_message(
        &self,
        group_openid: &str,
        msg_id: Option<&str>,
        message_type: &'static str,
        payload: &Value,
    ) -> SendResult {
        let url = format!("{}/v2/groups/{group_openid}/messages", self.api_base);
        let masked_group = mask_openid(group_openid);
        let response = self
            .client
            .post(url)
            .header("Authorization", self.auth.authorization_header().await?)
            .json(payload)
            .send()
            .await
            .map_err(|error| {
                warn!(
                    group = %masked_group,
                    source_message_id = msg_id.unwrap_or(""),
                    message_type = message_type,
                    error = %reqwest_error_summary(&error),
                    "QQ 群聊发送请求失败"
                );
                ApiError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            warn!(
                group = %masked_group,
                source_message_id = msg_id.unwrap_or(""),
                message_type = message_type,
                status = %status,
                "QQ 群聊发送返回非成功状态码"
            );
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::Status { status, body });
        }

        let body = response.text().await.map_err(ApiError::Http)?;
        let sent_ids = extract_sent_message_ids(&body);
        info!(
            group = %masked_group,
            source_message_id = msg_id.unwrap_or(""),
            sent_message_id = sent_ids.message_id.as_deref().unwrap_or(""),
            sent_ref_index_id = sent_ids.ref_index_id.as_deref().unwrap_or(""),
            message_type = message_type,
            "QQ 群聊发送成功"
        );
        Ok(sent_ids)
    }
}

impl OutboundSender for QqApiClient {
    fn send_text<'a>(&'a self, target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            self.send_c2c_text(&target.user_openid, target.msg_id.as_deref(), text)
                .await
        })
    }

    fn send_markdown<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            self.send_c2c_markdown(&target.user_openid, target.msg_id.as_deref(), markdown)
                .await
        })
    }

    fn send_image<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        image: &'a ImagePayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            self.send_c2c_image(&target.user_openid, target.msg_id.as_deref(), image)
                .await
        })
    }
}

/// 构建官方 `/stream_messages` 请求载荷。
fn build_c2c_stream_payload(
    content_raw: &str,
    msg_id: &str,
    msg_seq: u32,
    stream_state: &C2cStreamTransportState,
    input_state: u8,
) -> Value {
    serde_json::to_value(C2cStreamPayload {
        input_mode: "replace",
        input_state,
        content_type: "markdown",
        content_raw,
        // SDK 默认把 event_id 绑定到被动回复的源消息 ID；Gateway 当前可用的事件
        // 标识就是 C2C 入站 msg_id，不能用 stream_msg_id 或 ref_idx 替代。
        event_id: msg_id,
        msg_id,
        stream_msg_id: stream_state.stream_msg_id.as_deref(),
        msg_seq,
        index: stream_state.index,
    })
    .expect("official C2C stream payload should serialize")
}

pub fn build_c2c_text_payload(text: &str, msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(C2cTextPayload {
        content: text,
        msg_type: 0,
        msg_id,
        msg_seq,
    })
    .expect("C2C text payload should serialize")
}

fn build_c2c_typing_payload(msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(C2cTypingPayload {
        msg_type: 6,
        input_notify: C2cInputNotify {
            input_type: 1,
            input_second: 60,
        },
        msg_id,
        msg_seq,
    })
    .expect("C2C typing payload should serialize")
}

pub fn build_group_text_payload(text: &str, msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(GroupTextPayload {
        content: text,
        msg_type: 0,
        msg_id,
        msg_seq,
    })
    .expect("group text payload should serialize")
}

fn stream_payload_content_chars(payload: &Value) -> usize {
    payload
        .get("content_raw")
        .and_then(Value::as_str)
        .map(|content| content.chars().count())
        .unwrap_or(0)
}

pub async fn send_outbound_with_fallback<S: OutboundSender + ?Sized>(
    sender: &S,
    target: &C2cReplyTarget,
    outbound: &OutboundMessage,
) -> SendResult {
    match outbound {
        OutboundMessage::Text { text } => sender.send_text(target, text).await,
        OutboundMessage::Markdown {
            markdown,
            fallback_text,
        } => match sender.send_markdown(target, markdown).await {
            Ok(message_id) => Ok(message_id),
            Err(err) if !fallback_text.trim().is_empty() => {
                warn!(
                    user = %mask_openid(&target.user_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    error = %err.log_summary(),
                    "Markdown 发送失败，将降级为文本发送"
                );
                match sender.send_text(target, fallback_text).await {
                    Ok(message_id) => Ok(message_id),
                    Err(fallback_err) => {
                        warn!(
                            user = %mask_openid(&target.user_openid),
                            source_message_id = target.msg_id.as_deref().unwrap_or(""),
                            error = %fallback_err.log_summary(),
                            "Markdown 降级文本发送失败"
                        );
                        Err(fallback_err)
                    }
                }
            }
            Err(err) => Err(err),
        },
        OutboundMessage::Image {
            image,
            fallback_text,
        } => match sender.send_image(target, image).await {
            Ok(message_id) => Ok(message_id),
            Err(err) if !fallback_text.trim().is_empty() => {
                warn!(
                    user = %mask_openid(&target.user_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    error = %err.log_summary(),
                    "图片发送失败，将降级为文本发送"
                );
                match sender.send_text(target, fallback_text).await {
                    Ok(message_id) => Ok(message_id),
                    Err(fallback_err) => {
                        warn!(
                            user = %mask_openid(&target.user_openid),
                            source_message_id = target.msg_id.as_deref().unwrap_or(""),
                            error = %fallback_err.log_summary(),
                            "图片降级文本发送失败"
                        );
                        Err(fallback_err)
                    }
                }
            }
            Err(err) => Err(err),
        },
        OutboundMessage::ImagePlaceholder { fallback_text }
        | OutboundMessage::AttachmentPlaceholder { fallback_text } => {
            sender.send_text(target, fallback_text).await
        }
    }
}

pub async fn send_group_outbound_with_fallback<S: GroupOutboundSender + ?Sized>(
    sender: &S,
    target: &GroupReplyTarget,
    outbound: &OutboundMessage,
) -> SendResult {
    match outbound {
        OutboundMessage::Text { text } => sender.send_text(target, text).await,
        OutboundMessage::Markdown {
            markdown,
            fallback_text,
        } => match sender.send_markdown(target, markdown).await {
            Ok(message_id) => Ok(message_id),
            Err(err) if !fallback_text.trim().is_empty() => {
                warn!(
                    group = %mask_openid(&target.group_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    error = %err.log_summary(),
                    "群聊 Markdown 发送失败，将降级为文本发送"
                );
                match sender.send_text(target, fallback_text).await {
                    Ok(message_id) => Ok(message_id),
                    Err(fallback_err) => {
                        warn!(
                            group = %mask_openid(&target.group_openid),
                            source_message_id = target.msg_id.as_deref().unwrap_or(""),
                            error = %fallback_err.log_summary(),
                            "群聊 Markdown 降级文本发送失败"
                        );
                        Err(fallback_err)
                    }
                }
            }
            Err(err) => Err(err),
        },
        OutboundMessage::Image {
            image,
            fallback_text,
        } => match sender.send_image(target, image).await {
            Ok(message_id) => Ok(message_id),
            Err(err) if !fallback_text.trim().is_empty() => {
                warn!(
                    group = %mask_openid(&target.group_openid),
                    source_message_id = target.msg_id.as_deref().unwrap_or(""),
                    error = %err.log_summary(),
                    "群聊图片发送失败，将降级为文本发送"
                );
                sender.send_text(target, fallback_text).await
            }
            Err(err) => Err(err),
        },
        OutboundMessage::ImagePlaceholder { fallback_text }
        | OutboundMessage::AttachmentPlaceholder { fallback_text } => {
            sender.send_text(target, fallback_text).await
        }
    }
}

pub mod member_detail;

#[cfg(test)]
mod tests;

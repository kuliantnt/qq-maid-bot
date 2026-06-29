use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tracing::{info, warn};

use crate::{
    auth::{AccessTokenManager, AuthError},
    logging::{mask_openid, reqwest_error_summary},
    markdown::{MarkdownPayload, build_c2c_markdown_payload, build_group_markdown_payload},
    media::{ImagePayload, build_c2c_image_payload},
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
        }
    }
}

/// QQ 错误响应只保留短摘要用于诊断，避免把完整响应体或潜在敏感字段写入日志。
fn qq_api_error_body_summary(body: &str) -> String {
    const MAX_CHARS: usize = 200;
    let mut summary = body.split_whitespace().collect::<Vec<_>>().join(" ");
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
struct GroupTextPayload<'a> {
    content: &'a str,
    msg_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

pub type SendResult = Result<Option<String>, ApiError>;
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = SendResult> + Send + 'a>>;

/// QQ C2C 普通文本流式消息载荷。
///
/// 参考协议使用顶层 `content` + `msg_id` + `msg_seq` + `stream`，这里故意不携带
/// `msg_type`、`markdown` 或模板字段，避免把 Markdown 流式协议误当作文本流式验证。
#[derive(Debug, Serialize)]
struct C2cTextStreamPayload<'a> {
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
    stream: StreamInfo<'a>,
}

/// QQ 流式消息的 stream 控制字段。
///
/// - `state`: 1 = 生成中, 10 = 结束流式消息
/// - `id`: 首帧必须为 JSON null，后续使用首帧响应返回的真实 stream id 续接
/// - `index`: 从 0 开始递增的分片序号
/// - `reset`: true 表示用本帧 content 替换已有气泡内容
#[derive(Debug, Serialize)]
struct StreamInfo<'a> {
    state: u8,
    id: Option<&'a str>,
    index: u32,
    reset: bool,
}

/// C2C 文本流首帧响应 DTO。
///
/// 目前只接受顶层 `id` 作为 stream id；真实 QQ 联调确认字段前，不能把原始
/// `msg_id` 或其它普通消息 id 路径猜作流式续接 id。
#[derive(Debug, Deserialize)]
struct C2cTextStreamSendResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    code: Option<Value>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    msg: Option<String>,
}

/// C2C 流式发送的结果：成功时返回 API 返回的消息 id。
///
/// 注意：首帧返回的 id 才作为本次流的续接 id；后续分片即使继续返回 id，
/// 也不能覆盖既有 stream id，否则最终帧可能因 id/index 序列不匹配被 QQ 拒绝。
pub type StreamSendResult = Result<Option<String>, ApiError>;

/// C2C 流式发送状态管理。
///
/// 在一次流式会话中维护首帧 stream_id 和分片 index，确保每次发送到 QQ
/// 的 stream 参数正确。
#[derive(Debug)]
pub(crate) struct C2cStreamState {
    pub(crate) stream_id: Option<String>,
    pub(crate) index: u32,
}

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
}

pub trait GroupOutboundSender: Send + Sync {
    fn send_text<'a>(&'a self, target: &'a GroupReplyTarget, text: &'a str) -> SendFuture<'a>;
    fn send_markdown<'a>(
        &'a self,
        target: &'a GroupReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a>;
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

    pub async fn send_c2c_image(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        image: &ImagePayload,
    ) -> SendResult {
        let payload = build_c2c_image_payload(image, msg_id, self.next_msg_seq());
        self.post_c2c_message(user_openid, msg_id, "image", &payload)
            .await
    }

    /// 发送 C2C 普通文本流式消息分片。
    ///
    /// 依据参考协议只发送顶层 `content` 和 `stream` 控制字段；这里的 `msg_id`
    /// 是被动回复绑定的原始 QQ 消息 ID，不能当作 stream id 使用。
    pub(crate) async fn send_c2c_text_stream(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        content: &str,
        stream_state: &C2cStreamState,
        state: u8,
        reset: bool,
    ) -> StreamSendResult {
        let payload = build_c2c_text_stream_payload(
            content,
            msg_id,
            self.next_msg_seq(),
            stream_state,
            state,
            reset,
        );
        self.post_c2c_stream_message(user_openid, msg_id, state, reset, stream_state, &payload)
            .await
    }

    /// 发送 C2C 流式消息底层的 HTTP POST，返回提取的消息 id。
    async fn post_c2c_stream_message(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        state: u8,
        reset: bool,
        stream_state: &C2cStreamState,
        payload: &Value,
    ) -> StreamSendResult {
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
                    phase = %stream_log_phase(state, stream_state.index),
                    state,
                    index = stream_state.index,
                    reset,
                    has_stream_id = stream_state.stream_id.is_some(),
                    content_chars = stream_payload_content_chars(payload),
                    error = %reqwest_error_summary(&error),
                    "QQ stream send request failed"
                );
                ApiError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let (qq_code, qq_message) = qq_api_error_fields(&body);
            warn!(
                user = %masked_user,
                source_message_id = msg_id.unwrap_or(""),
                phase = %stream_log_phase(state, stream_state.index),
                state,
                index = stream_state.index,
                reset,
                has_stream_id = stream_state.stream_id.is_some(),
                content_chars = stream_payload_content_chars(payload),
                http_status = %status,
                qq_code = qq_code.as_deref().unwrap_or(""),
                qq_message = qq_message.as_deref().unwrap_or(""),
                error_summary = %qq_api_error_body_summary(&body),
                "QQ stream send returned non-success status"
            );
            return Err(ApiError::Status { status, body });
        }

        let body = response.text().await.map_err(ApiError::Http)?;
        let sent_stream_id = extract_c2c_text_stream_id(&body);
        let (qq_code, qq_message) = qq_api_error_fields(&body);
        info!(
            user = %masked_user,
            source_message_id = msg_id.unwrap_or(""),
            phase = %stream_log_phase(state, stream_state.index),
            state,
            index = stream_state.index,
            reset,
            has_stream_id = stream_state.stream_id.is_some(),
            content_chars = stream_payload_content_chars(payload),
            http_status = %status,
            qq_code = qq_code.as_deref().unwrap_or(""),
            qq_message = qq_message.as_deref().unwrap_or(""),
            returned_stream_id = sent_stream_id.as_deref().unwrap_or(""),
            "qq stream send success"
        );
        Ok(sent_stream_id)
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
                    "QQ send request failed"
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
                "QQ send returned non-success status"
            );
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::Status { status, body });
        }

        let body = response.text().await.map_err(ApiError::Http)?;
        let sent_message_id = extract_sent_message_id(&body);
        info!(
            user = %masked_user,
            source_message_id = msg_id.unwrap_or(""),
            sent_message_id = sent_message_id.as_deref().unwrap_or(""),
            message_type = message_type,
            "qq send success"
        );
        Ok(sent_message_id)
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
                    "QQ group send request failed"
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
                "QQ group send returned non-success status"
            );
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::Status { status, body });
        }

        let body = response.text().await.map_err(ApiError::Http)?;
        let sent_message_id = extract_sent_message_id(&body);
        info!(
            group = %masked_group,
            source_message_id = msg_id.unwrap_or(""),
            sent_message_id = sent_message_id.as_deref().unwrap_or(""),
            message_type = message_type,
            "qq group send success"
        );
        Ok(sent_message_id)
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

/// 构建 C2C 普通文本流式载荷的 JSON Value。
fn build_c2c_text_stream_payload(
    content: &str,
    msg_id: Option<&str>,
    msg_seq: u32,
    stream_state: &C2cStreamState,
    state: u8,
    reset: bool,
) -> Value {
    serde_json::to_value(C2cTextStreamPayload {
        content,
        msg_id,
        msg_seq,
        stream: StreamInfo {
            state,
            id: stream_state.stream_id.as_deref(),
            index: stream_state.index,
            reset,
        },
    })
    .expect("C2C text stream payload should serialize")
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

pub fn build_group_text_payload(text: &str, msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(GroupTextPayload {
        content: text,
        msg_type: 0,
        msg_id,
        msg_seq,
    })
    .expect("group text payload should serialize")
}

fn extract_c2c_text_stream_id(body: &str) -> Option<String> {
    let response = serde_json::from_str::<C2cTextStreamSendResponse>(body).ok()?;
    response
        .id
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
}

fn qq_api_error_fields(body: &str) -> (Option<String>, Option<String>) {
    let Ok(response) = serde_json::from_str::<C2cTextStreamSendResponse>(body) else {
        return (None, None);
    };
    let code = response.code.map(|value| match value {
        Value::String(value) => value,
        other => other.to_string(),
    });
    let message = response.message.or(response.msg);
    (code, message)
}

fn stream_log_phase(state: u8, index: u32) -> &'static str {
    match (state, index) {
        (10, _) => "final_chunk",
        (_, 0) => "first_chunk",
        _ => "middle_chunk",
    }
}

fn stream_payload_content_chars(payload: &Value) -> usize {
    payload
        .get("content")
        .and_then(Value::as_str)
        .map(|content| content.chars().count())
        .unwrap_or(0)
}

pub(crate) fn extract_sent_message_id(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let candidates = [
        value.get("id"),
        value.get("message_id"),
        value.get("msg_id"),
        value.get("d").and_then(|item| item.get("id")),
        value.get("d").and_then(|item| item.get("message_id")),
        value.get("d").and_then(|item| item.get("msg_id")),
        value.get("data").and_then(|item| item.get("id")),
        value.get("data").and_then(|item| item.get("message_id")),
        value.get("data").and_then(|item| item.get("msg_id")),
        value.get("message").and_then(|item| item.get("id")),
        value.get("message").and_then(|item| item.get("message_id")),
        value.get("message").and_then(|item| item.get("msg_id")),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
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
                    "markdown send failed; falling back to text"
                );
                match sender.send_text(target, fallback_text).await {
                    Ok(message_id) => Ok(message_id),
                    Err(fallback_err) => {
                        warn!(
                            user = %mask_openid(&target.user_openid),
                            source_message_id = target.msg_id.as_deref().unwrap_or(""),
                            error = %fallback_err.log_summary(),
                            "markdown fallback text send failed"
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
                    "image send failed; falling back to text"
                );
                match sender.send_text(target, fallback_text).await {
                    Ok(message_id) => Ok(message_id),
                    Err(fallback_err) => {
                        warn!(
                            user = %mask_openid(&target.user_openid),
                            source_message_id = target.msg_id.as_deref().unwrap_or(""),
                            error = %fallback_err.log_summary(),
                            "image fallback text send failed"
                        );
                        Err(fallback_err)
                    }
                }
            }
            Err(err) => Err(err),
        },
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
                    "group markdown send failed; falling back to text"
                );
                match sender.send_text(target, fallback_text).await {
                    Ok(message_id) => Ok(message_id),
                    Err(fallback_err) => {
                        warn!(
                            group = %mask_openid(&target.group_openid),
                            source_message_id = target.msg_id.as_deref().unwrap_or(""),
                            error = %fallback_err.log_summary(),
                            "group markdown fallback text send failed"
                        );
                        Err(fallback_err)
                    }
                }
            }
            Err(err) => Err(err),
        },
        OutboundMessage::Image { fallback_text, .. } => {
            sender.send_text(target, fallback_text).await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::{markdown::MarkdownPayload, media::ImagePayload, render::OutboundMessage};

    #[test]
    fn extracts_sent_message_id_from_common_response_shapes() {
        assert_eq!(
            extract_sent_message_id(r#"{"id":"msg-1"}"#).as_deref(),
            Some("msg-1")
        );
        assert_eq!(
            extract_sent_message_id(r#"{"data":{"message_id":"msg-2"}}"#).as_deref(),
            Some("msg-2")
        );
        assert_eq!(
            extract_sent_message_id(r#"{"d":{"msg_id":"msg-3"}}"#).as_deref(),
            Some("msg-3")
        );
        assert_eq!(
            extract_sent_message_id(r#"{"message":{"id":"msg-4"}}"#).as_deref(),
            Some("msg-4")
        );
        assert_eq!(extract_sent_message_id(r#"{"ok":true}"#), None);
    }

    #[test]
    fn c2c_text_payload_matches_qq_shape() {
        let payload = build_c2c_text_payload("hello", Some("msg-1"), 7);

        assert_eq!(payload["content"], "hello");
        assert_eq!(payload["msg_type"], 0);
        assert_eq!(payload["msg_id"], "msg-1");
        assert_eq!(payload["msg_seq"], 7);
    }

    #[test]
    fn c2c_text_stream_payload_matches_reference_shape() {
        let first_payload = build_c2c_text_stream_payload(
            "ovo",
            Some("msg-1"),
            6,
            &C2cStreamState {
                stream_id: None,
                index: 0,
            },
            1,
            false,
        );
        assert_eq!(first_payload["content"], "ovo");
        assert_eq!(first_payload["msg_id"], "msg-1");
        assert_eq!(first_payload["msg_seq"], 6);
        assert!(first_payload["stream"]["id"].is_null());
        assert_eq!(first_payload["stream"]["index"], 0);
        assert_eq!(first_payload["stream"]["state"], 1);
        assert_eq!(first_payload["stream"]["reset"], false);
        assert!(first_payload.get("msg_type").is_none());
        assert!(first_payload.get("markdown").is_none());

        let payload = build_c2c_text_stream_payload(
            "qwqwqwqw",
            Some("msg-1"),
            7,
            &C2cStreamState {
                stream_id: Some("stream-1".to_owned()),
                index: 2,
            },
            1,
            true,
        );

        // 被动回复 msg_id 和流式续接 id 分属两个协议字段，缺一都会导致 QQ 端退化或续接失败。
        assert_eq!(payload["content"], "qwqwqwqw");
        assert_eq!(payload["msg_id"], "msg-1");
        assert_eq!(payload["msg_seq"], 7);
        assert_eq!(payload["stream"]["id"], "stream-1");
        assert_eq!(payload["stream"]["index"], 2);
        assert_eq!(payload["stream"]["state"], 1);
        assert_eq!(payload["stream"]["reset"], true);
    }

    #[test]
    fn c2c_text_stream_response_uses_typed_top_level_id_only() {
        assert_eq!(
            extract_c2c_text_stream_id(r#"{"id":"stream-1","code":0}"#).as_deref(),
            Some("stream-1")
        );
        assert_eq!(
            extract_c2c_text_stream_id(r#"{"data":{"id":"ordinary-message"}}"#),
            None
        );
        assert_eq!(extract_c2c_text_stream_id(r#"{"msg_id":"msg-1"}"#), None);
    }

    #[derive(Debug, Default)]
    struct MockSender {
        calls: Mutex<Vec<String>>,
    }

    impl MockSender {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl OutboundSender for MockSender {
        fn send_text<'a>(&'a self, _target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(format!("text:{text}"));
                Ok(None)
            })
        }

        fn send_markdown<'a>(
            &'a self,
            _target: &'a C2cReplyTarget,
            _markdown: &'a MarkdownPayload,
        ) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("markdown".to_owned());
                Err(ApiError::Unsupported("markdown"))
            })
        }

        fn send_image<'a>(
            &'a self,
            _target: &'a C2cReplyTarget,
            _image: &'a ImagePayload,
        ) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("image".to_owned());
                Err(ApiError::Unsupported("image"))
            })
        }
    }

    impl GroupOutboundSender for MockSender {
        fn send_text<'a>(&'a self, _target: &'a GroupReplyTarget, text: &'a str) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("group-text:{text}"));
                Ok(None)
            })
        }

        fn send_markdown<'a>(
            &'a self,
            _target: &'a GroupReplyTarget,
            _markdown: &'a MarkdownPayload,
        ) -> SendFuture<'a> {
            Box::pin(async move {
                self.calls.lock().unwrap().push("group-markdown".to_owned());
                Err(ApiError::Unsupported("markdown"))
            })
        }
    }

    fn target() -> C2cReplyTarget {
        C2cReplyTarget {
            user_openid: "user-1".to_owned(),
            msg_id: Some("msg-1".to_owned()),
        }
    }

    fn group_target() -> GroupReplyTarget {
        GroupReplyTarget {
            group_openid: "group-1".to_owned(),
            msg_id: Some("msg-1".to_owned()),
        }
    }

    /// 合并 2 个 send 回退测试为表驱动测试。
    #[tokio::test]
    async fn send_failure_falls_back_to_text() {
        struct Case {
            name: &'static str,
            outbound: OutboundMessage,
            expected_calls: &'static [&'static str],
        }

        let cases = [
            Case {
                name: "markdown_send_failure_falls_back_to_text",
                outbound: OutboundMessage::Markdown {
                    markdown: MarkdownPayload::new("# hello"),
                    fallback_text: "hello".to_owned(),
                },
                expected_calls: &["markdown", "text:hello"],
            },
            Case {
                name: "image_send_failure_falls_back_to_text",
                outbound: OutboundMessage::Image {
                    image: ImagePayload::new("file-info"),
                    fallback_text: "image fallback".to_owned(),
                },
                expected_calls: &["image", "text:image fallback"],
            },
        ];

        for case in &cases {
            let sender = MockSender::default();
            send_outbound_with_fallback(&sender, &target(), &case.outbound)
                .await
                .unwrap_or_else(|e| panic!("case '{}' failed: {:?}", case.name, e));
            assert_eq!(
                sender.calls(),
                case.expected_calls,
                "case '{}' failed: calls mismatch",
                case.name
            );
        }
    }

    #[tokio::test]
    async fn group_markdown_send_failure_falls_back_to_text() {
        let sender = MockSender::default();
        let outbound = OutboundMessage::Markdown {
            markdown: MarkdownPayload::new("# hello"),
            fallback_text: "hello".to_owned(),
        };
        send_group_outbound_with_fallback(&sender, &group_target(), &outbound)
            .await
            .unwrap();
        assert_eq!(sender.calls(), vec!["group-markdown", "group-text:hello"]);
    }
}

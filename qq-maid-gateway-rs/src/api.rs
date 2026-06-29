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
    logging::{mask_identifier, mask_openid, reqwest_error_summary},
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

/// QQ C2C Markdown 流式消息载荷。
///
/// 内容分片只发送 `msg_type=2` 和 `markdown.content`，禁止同时携带顶层 `content`，
/// 避免 QQ 端把同一帧解释为普通文本流。结束包保留空 markdown，满足 QQ 必填校验但不重复正文。
#[derive(Debug, Serialize)]
struct C2cMarkdownStreamPayload<'a> {
    msg_type: u8,
    markdown: &'a MarkdownPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
    stream: StreamInfo<'a>,
}

/// QQ 流式消息的 stream 控制字段。
///
/// - `state`: 1 = 生成中, 10 = 结束流式消息；真实环境确认 JSON 字段名必须是 `state`
/// - `done`: 结束包必须为 true，内容分片为 false
/// - `id`: 首帧必须为 JSON null，后续使用首帧响应返回的真实 stream id 续接
/// - `index`: 从 0 开始递增的内容分片序号；结束包当前已确认语义不包含 index，避免凭空增加
/// - `reset`: 仅在内容分片中按需携带；结束语义由 `done=true` 表达，不用 `reset=true` 替代
#[derive(Debug, Serialize)]
struct StreamInfo<'a> {
    state: u8,
    done: bool,
    id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset: Option<bool>,
}

/// C2C 流式首帧响应 DTO。
///
/// 目前只接受顶层 `id` 作为 stream id；真实 QQ 联调确认字段前，不能把原始
/// `msg_id` 或其它普通消息 id 路径猜作流式续接 id。
#[derive(Debug, Deserialize)]
struct C2cStreamSendResponse {
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
/// 在一次流式会话中维护首帧 stream_id 和下一次内容分片要使用的 index。
/// index 只在 QQ 明确接受非终包后推进；终包属于关闭动作，不再生成新的 next_index。
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

    /// 发送 C2C Markdown 流式消息分片。
    ///
    /// 这里的 `msg_id` 是被动回复绑定的原始 QQ 消息 ID，不能当作 stream id 使用；
    /// stream id 只来自首帧响应的顶层 `id`。
    pub(crate) async fn send_c2c_markdown_stream(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        markdown: &MarkdownPayload,
        stream_state: &C2cStreamState,
        stream_state_value: u8,
        reset: Option<bool>,
    ) -> StreamSendResult {
        let payload = build_c2c_markdown_stream_payload(
            markdown,
            msg_id,
            self.next_msg_seq(),
            stream_state,
            stream_state_value,
            reset,
        );
        self.post_c2c_stream_message(
            user_openid,
            msg_id,
            stream_state_value,
            stream_state,
            &payload,
        )
        .await
    }

    /// 发送 C2C 流式消息底层的 HTTP POST，返回提取的消息 id。
    async fn post_c2c_stream_message(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        stream_state_value: u8,
        stream_state: &C2cStreamState,
        payload: &Value,
    ) -> StreamSendResult {
        let url = format!("{}/v2/users/{user_openid}/messages", self.api_base);
        let masked_user = mask_openid(user_openid);
        let masked_message_id = msg_id.map(mask_identifier).unwrap_or_default();
        let done = stream_state_value == 10;
        let reset = stream_reset_log_value(payload);
        let index_present = stream_payload_index(payload).is_some();
        let reset_present = reset.is_some();
        let request_fields = stream_request_log_fields(stream_state_value, stream_state, false);
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
                    phase = %stream_log_phase(stream_state_value, stream_state.index),
                    stream_state_value,
                    done,
                    index_present,
                    reset_present,
                    stream_index = ?stream_payload_index(payload),
                    reset = ?reset,
                    previous_success_index = ?request_fields.previous_success_index,
                    next_index = request_fields.next_index,
                    has_stream_id = stream_state.stream_id.is_some(),
                    content_chars = stream_payload_content_chars(payload),
                    http_status = "",
                    qq_code = "",
                    qq_message = "",
                    index_committed = request_fields.index_committed,
                    error = %reqwest_error_summary(&error),
                    "QQ stream send request failed"
                );
                ApiError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let (qq_code, qq_message) = qq_api_error_fields(&body);
            let failed_fields = stream_request_log_fields(stream_state_value, stream_state, false);
            warn!(
                user = %masked_user,
                source_message_id = %masked_message_id,
                phase = %stream_log_phase(stream_state_value, stream_state.index),
                stream_state_value,
                done,
                index_present,
                reset_present,
                stream_index = ?stream_payload_index(payload),
                reset = ?reset,
                previous_success_index = ?failed_fields.previous_success_index,
                next_index = failed_fields.next_index,
                has_stream_id = stream_state.stream_id.is_some(),
                content_chars = stream_payload_content_chars(payload),
                http_status = %status,
                qq_code = qq_code.as_deref().unwrap_or(""),
                qq_message = qq_message.as_deref().unwrap_or(""),
                index_committed = failed_fields.index_committed,
                error_summary = %qq_api_error_body_summary(&body),
                "QQ stream send returned non-success status"
            );
            return Err(ApiError::Status { status, body });
        }

        let body = response.text().await.map_err(ApiError::Http)?;
        let sent_stream_id = extract_c2c_text_stream_id(&body);
        let (qq_code, qq_message) = qq_api_error_fields(&body);
        let success_fields = stream_request_log_fields(stream_state_value, stream_state, true);
        info!(
            user = %masked_user,
            source_message_id = %masked_message_id,
            phase = %stream_log_phase(stream_state_value, stream_state.index),
            stream_state_value,
            done,
            index_present,
            reset_present,
            stream_index = ?stream_payload_index(payload),
            reset = ?reset,
            previous_success_index = ?success_fields.previous_success_index,
            next_index = success_fields.next_index,
            has_stream_id = stream_state.stream_id.is_some(),
            content_chars = stream_payload_content_chars(payload),
            http_status = %status,
            qq_code = qq_code.as_deref().unwrap_or(""),
            qq_message = qq_message.as_deref().unwrap_or(""),
            index_committed = success_fields.index_committed,
            returned_stream_id = %sent_stream_id.as_deref().map(mask_identifier).unwrap_or_default(),
            returned_stream_id_present = sent_stream_id.is_some(),
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

/// 构建 C2C Markdown 流式载荷的 JSON Value。
fn build_c2c_markdown_stream_payload(
    markdown: &MarkdownPayload,
    msg_id: Option<&str>,
    msg_seq: u32,
    stream_state: &C2cStreamState,
    stream_state_value: u8,
    reset: Option<bool>,
) -> Value {
    let done = stream_state_value == 10;
    serde_json::to_value(C2cMarkdownStreamPayload {
        msg_type: 2,
        markdown,
        msg_id,
        msg_seq,
        stream: StreamInfo {
            state: stream_state_value,
            done,
            id: stream_state.stream_id.as_deref(),
            // 结束包以 `done=true` + `state=10` 关闭流；当前项目内无协议依据要求继续携带 index。
            index: (!done).then_some(stream_state.index),
            reset: if done { None } else { reset },
        },
    })
    .expect("C2C markdown stream payload should serialize")
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
    let response = serde_json::from_str::<C2cStreamSendResponse>(body).ok()?;
    response
        .id
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
}

fn qq_api_error_fields(body: &str) -> (Option<String>, Option<String>) {
    let Ok(response) = serde_json::from_str::<C2cStreamSendResponse>(body) else {
        return (None, None);
    };
    let code = response.code.map(|value| match value {
        Value::String(value) => value,
        other => other.to_string(),
    });
    let message = response.message.or(response.msg);
    (code, message)
}

fn stream_log_phase(stream_state_value: u8, index: u32) -> &'static str {
    match (stream_state_value, index) {
        (10, _) => "final_chunk",
        (_, 0) => "first_chunk",
        _ => "middle_chunk",
    }
}

fn stream_payload_content_chars(payload: &Value) -> usize {
    payload
        .get("markdown")
        .and_then(|markdown| markdown.get("content"))
        .or_else(|| payload.get("content"))
        .and_then(Value::as_str)
        .map(|content| content.chars().count())
        .unwrap_or(0)
}

fn stream_reset_log_value(payload: &Value) -> Option<bool> {
    payload
        .get("stream")
        .and_then(|stream| stream.get("reset"))
        .and_then(Value::as_bool)
}

fn stream_payload_index(payload: &Value) -> Option<u64> {
    payload
        .get("stream")
        .and_then(|stream| stream.get("index"))
        .and_then(Value::as_u64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamRequestLogFields {
    previous_success_index: Option<u32>,
    next_index: u32,
    index_committed: bool,
}

fn stream_request_log_fields(
    stream_state_value: u8,
    stream_state: &C2cStreamState,
    request_succeeded: bool,
) -> StreamRequestLogFields {
    // `stream_state.index` 表示下一次内容分片要使用的 index；只有非终包成功后才推进。
    let index_committed = request_succeeded && stream_state_value != 10;
    StreamRequestLogFields {
        previous_success_index: stream_state.index.checked_sub(1),
        next_index: if index_committed {
            stream_state.index.saturating_add(1)
        } else {
            stream_state.index
        },
        index_committed,
    }
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
    fn c2c_markdown_stream_payload_matches_reference_shape() {
        let first_markdown = MarkdownPayload::new("**hello**");
        let first_payload = build_c2c_markdown_stream_payload(
            &first_markdown,
            Some("msg-1"),
            6,
            &C2cStreamState {
                stream_id: None,
                index: 0,
            },
            1,
            Some(false),
        );
        assert_eq!(first_payload["msg_type"], 2);
        assert_eq!(first_payload["markdown"]["content"], "**hello**");
        assert_eq!(first_payload["msg_id"], "msg-1");
        assert_eq!(first_payload["msg_seq"], 6);
        assert!(first_payload.get("content").is_none());
        assert!(first_payload["stream"]["id"].is_null());
        assert_eq!(first_payload["stream"]["index"], 0);
        assert_eq!(first_payload["stream"]["state"], 1);
        assert_eq!(first_payload["stream"]["done"], false);
        assert!(first_payload["stream"].get("type").is_none());
        assert_eq!(first_payload["stream"]["reset"], false);

        let middle_markdown = MarkdownPayload::new(" delta");
        let middle_payload = build_c2c_markdown_stream_payload(
            &middle_markdown,
            Some("msg-1"),
            7,
            &C2cStreamState {
                stream_id: Some("stream-1".to_owned()),
                index: 1,
            },
            1,
            Some(false),
        );

        // 被动回复 msg_id 和流式续接 id 分属两个协议字段，缺一都会导致 QQ 端退化或续接失败。
        assert_eq!(middle_payload["msg_type"], 2);
        assert_eq!(middle_payload["markdown"]["content"], " delta");
        assert!(middle_payload.get("content").is_none());
        assert_eq!(middle_payload["stream"]["id"], "stream-1");
        assert_eq!(middle_payload["stream"]["index"], 1);
        assert_eq!(middle_payload["stream"]["state"], 1);
        assert_eq!(middle_payload["stream"]["done"], false);
        assert!(middle_payload["stream"].get("type").is_none());
        assert_eq!(middle_payload["stream"]["reset"], false);

        let middle_json = serde_json::to_string(&middle_payload).unwrap();
        assert!(middle_json.contains("\"state\":1"));
        assert!(!middle_json.contains("\"type\":1"));

        let final_markdown = MarkdownPayload::new("**hello** delta");
        let final_payload = build_c2c_markdown_stream_payload(
            &final_markdown,
            Some("msg-1"),
            8,
            &C2cStreamState {
                stream_id: Some("stream-1".to_owned()),
                index: 2,
            },
            10,
            None,
        );
        assert_eq!(final_payload["msg_type"], 2);
        assert_eq!(final_payload["markdown"]["content"], "**hello** delta");
        assert!(final_payload.get("content").is_none());
        assert_eq!(final_payload["stream"]["id"], "stream-1");
        assert_eq!(final_payload["stream"]["state"], 10);
        assert_eq!(final_payload["stream"]["done"], true);
        assert!(final_payload["stream"].get("type").is_none());
        assert!(final_payload["stream"].get("index").is_none());
        assert!(final_payload["stream"].get("reset").is_none());

        let final_json = serde_json::to_string(&final_payload).unwrap();
        assert!(final_json.contains("\"state\":10"));
        assert!(final_json.contains("\"done\":true"));
        assert!(!final_json.contains("\"type\":10"));
        assert!(!final_json.contains("\"index\""));
        assert!(!final_json.contains("\"reset\""));
        assert!(final_json.contains("\"markdown\":{"));
        assert!(final_json.contains("\"content\":\"**hello** delta\""));
    }

    #[test]
    fn c2c_stream_response_uses_typed_top_level_id_only() {
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

    #[test]
    fn stream_request_log_fields_report_index_commit_semantics() {
        let first_state = C2cStreamState {
            stream_id: None,
            index: 0,
        };
        assert_eq!(
            stream_request_log_fields(1, &first_state, true),
            StreamRequestLogFields {
                previous_success_index: None,
                next_index: 1,
                index_committed: true,
            }
        );

        let middle_state = C2cStreamState {
            stream_id: Some("stream-1".to_owned()),
            index: 1,
        };
        assert_eq!(
            stream_request_log_fields(1, &middle_state, false),
            StreamRequestLogFields {
                previous_success_index: Some(0),
                next_index: 1,
                index_committed: false,
            }
        );

        // 终包只关闭已存在的流式气泡，不再提交新的 next_index。
        let final_state = C2cStreamState {
            stream_id: Some("stream-1".to_owned()),
            index: 2,
        };
        assert_eq!(
            stream_request_log_fields(10, &final_state, true),
            StreamRequestLogFields {
                previous_success_index: Some(1),
                next_index: 2,
                index_committed: false,
            }
        );
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

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
use tracing::{info, trace, warn};

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
struct C2cTypingPayload<'a> {
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

/// QQ C2C Markdown 流式消息载荷。
///
/// 内容分片只发送 `msg_type=2` 和 `markdown.content`，禁止同时携带顶层 `content`，
/// 避免 QQ 端把同一帧解释为普通文本流。真实环境要求结束包的 Markdown 非空，
/// 因此结束包也携带完整最终正文，并沿用同一套 stream id/index/reset 字段。
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
/// - `id`: 首帧必须为 JSON null，后续使用首帧响应返回的真实 stream id 续接
/// - `index`: 从 0 开始递增；完成包使用下一个连续 index
/// - `reset`: 参考实现中生成中和完成包都携带，当前统一使用 false 续接同一条消息
#[derive(Debug, Serialize)]
struct StreamInfo<'a> {
    state: u8,
    id: Option<&'a str>,
    index: u32,
    reset: bool,
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
/// index 只在 QQ 明确接受对应 stream 帧后推进；完成包也使用并提交连续 index。
#[derive(Debug, Default)]
pub(crate) struct C2cStreamState {
    pub(crate) stream_id: Option<String>,
    pub(crate) index: u32,
    msg_seq: C2cStreamMsgSeqState,
}

impl C2cStreamState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    fn begin_msg_seq_attempt(
        &mut self,
        stream_state_value: u8,
        next_msg_seq: impl FnOnce() -> u32,
    ) -> C2cStreamMsgSeqAttempt {
        let key = C2cStreamMsgSeqKey {
            state: stream_state_value,
            // final 虽然携带 stream.index，但本轮不改变外层 msg_seq 重试粒度；
            // 这里继续只按 state 区分完成包，避免把协议形状调整扩大到 msg_seq 语义。
            stream_index: (stream_state_value != 10).then_some(self.index),
        };
        if let Some(pending) = self.msg_seq.pending.filter(|pending| pending.key == key) {
            return C2cStreamMsgSeqAttempt {
                key,
                msg_seq: pending.msg_seq,
                previous_success_msg_seq: pending.previous_success_msg_seq,
            };
        }

        let attempt = C2cStreamMsgSeqAttempt {
            key,
            msg_seq: next_msg_seq(),
            previous_success_msg_seq: self.msg_seq.previous_success_msg_seq,
        };
        self.msg_seq.pending = Some(C2cStreamPendingMsgSeq {
            key,
            msg_seq: attempt.msg_seq,
            previous_success_msg_seq: attempt.previous_success_msg_seq,
        });
        attempt
    }

    fn commit_msg_seq_attempt(&mut self, attempt: C2cStreamMsgSeqAttempt) {
        self.msg_seq.previous_success_msg_seq = Some(attempt.msg_seq);
        if self
            .msg_seq
            .pending
            .is_some_and(|pending| pending.key == attempt.key && pending.msg_seq == attempt.msg_seq)
        {
            self.msg_seq.pending = None;
        }
    }
}

#[derive(Debug, Default)]
struct C2cStreamMsgSeqState {
    previous_success_msg_seq: Option<u32>,
    pending: Option<C2cStreamPendingMsgSeq>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct C2cStreamMsgSeqKey {
    state: u8,
    stream_index: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct C2cStreamPendingMsgSeq {
    key: C2cStreamMsgSeqKey,
    msg_seq: u32,
    previous_success_msg_seq: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct C2cStreamMsgSeqAttempt {
    key: C2cStreamMsgSeqKey,
    msg_seq: u32,
    previous_success_msg_seq: Option<u32>,
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
        stream_state: &mut C2cStreamState,
        stream_state_value: u8,
        reset: Option<bool>,
    ) -> StreamSendResult {
        let msg_seq_attempt =
            stream_state.begin_msg_seq_attempt(stream_state_value, || self.next_msg_seq());
        let payload = build_c2c_markdown_stream_payload(
            markdown,
            msg_id,
            msg_seq_attempt.msg_seq,
            stream_state,
            stream_state_value,
            reset,
        );
        self.post_c2c_stream_message(
            user_openid,
            msg_id,
            stream_state_value,
            stream_state,
            msg_seq_attempt,
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
        stream_state: &mut C2cStreamState,
        msg_seq_attempt: C2cStreamMsgSeqAttempt,
        payload: &Value,
    ) -> StreamSendResult {
        let url = format!("{}/v2/users/{user_openid}/messages", self.api_base);
        let masked_user = mask_openid(user_openid);
        let masked_message_id = msg_id.map(mask_identifier).unwrap_or_default();
        let reset = stream_reset_log_value(payload);
        let index_present = stream_payload_index(payload).is_some();
        let reset_present = reset.is_some();
        let request_fields =
            stream_request_log_fields(stream_state_value, stream_state, msg_seq_attempt, false);
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
                    msg_seq = request_fields.msg_seq,
                    previous_success_msg_seq = ?request_fields.previous_success_msg_seq,
                    state = stream_state_value,
                    stream_state_value,
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
                    msg_seq_committed = request_fields.msg_seq_committed,
                    error = %reqwest_error_summary(&error),
                    "QQ stream send request failed"
                );
                ApiError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let (qq_code, qq_message) = qq_api_error_fields(&body);
            let failed_fields =
                stream_request_log_fields(stream_state_value, stream_state, msg_seq_attempt, false);
            warn!(
                user = %masked_user,
                source_message_id = %masked_message_id,
                phase = %stream_log_phase(stream_state_value, stream_state.index),
                msg_seq = failed_fields.msg_seq,
                previous_success_msg_seq = ?failed_fields.previous_success_msg_seq,
                state = stream_state_value,
                stream_state_value,
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
                msg_seq_committed = failed_fields.msg_seq_committed,
                error_summary = %qq_api_error_body_summary(&body),
                "QQ stream send returned non-success status"
            );
            return Err(ApiError::Status { status, body });
        }

        stream_state.commit_msg_seq_attempt(msg_seq_attempt);
        let body = response.text().await.map_err(ApiError::Http)?;
        let sent_stream_id = extract_c2c_text_stream_id(&body);
        let (qq_code, qq_message) = qq_api_error_fields(&body);
        let success_fields =
            stream_request_log_fields(stream_state_value, stream_state, msg_seq_attempt, true);
        trace!(
            user = %masked_user,
            source_message_id = %masked_message_id,
            phase = %stream_log_phase(stream_state_value, stream_state.index),
            msg_seq = success_fields.msg_seq,
            previous_success_msg_seq = ?success_fields.previous_success_msg_seq,
            state = stream_state_value,
            stream_state_value,
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
            msg_seq_committed = success_fields.msg_seq_committed,
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
        let sent_ids = extract_sent_message_ids(&body);
        info!(
            user = %masked_user,
            source_message_id = msg_id.unwrap_or(""),
            sent_message_id = sent_ids.message_id.as_deref().unwrap_or(""),
            sent_ref_index_id = sent_ids.ref_index_id.as_deref().unwrap_or(""),
            message_type = message_type,
            "qq send success"
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
        let sent_ids = extract_sent_message_ids(&body);
        info!(
            group = %masked_group,
            source_message_id = msg_id.unwrap_or(""),
            sent_message_id = sent_ids.message_id.as_deref().unwrap_or(""),
            sent_ref_index_id = sent_ids.ref_index_id.as_deref().unwrap_or(""),
            message_type = message_type,
            "qq group send success"
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

/// 构建 C2C Markdown 流式载荷的 JSON Value。
fn build_c2c_markdown_stream_payload(
    markdown: &MarkdownPayload,
    msg_id: Option<&str>,
    msg_seq: u32,
    stream_state: &C2cStreamState,
    stream_state_value: u8,
    reset: Option<bool>,
) -> Value {
    serde_json::to_value(C2cMarkdownStreamPayload {
        msg_type: 2,
        markdown,
        msg_id,
        msg_seq,
        stream: StreamInfo {
            state: stream_state_value,
            id: stream_state.stream_id.as_deref(),
            index: stream_state.index,
            reset: reset.unwrap_or(false),
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

fn build_c2c_typing_payload(msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(C2cTypingPayload {
        msg_type: 6,
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
    msg_seq: u32,
    previous_success_msg_seq: Option<u32>,
    index_committed: bool,
    msg_seq_committed: bool,
}

fn stream_request_log_fields(
    _stream_state_value: u8,
    stream_state: &C2cStreamState,
    msg_seq_attempt: C2cStreamMsgSeqAttempt,
    request_succeeded: bool,
) -> StreamRequestLogFields {
    // `stream_state.index` 表示下一次 stream 帧要使用的 index；完成包成功后也提交该 index。
    let index_committed = request_succeeded;
    StreamRequestLogFields {
        previous_success_index: stream_state.index.checked_sub(1),
        next_index: if index_committed {
            stream_state.index.saturating_add(1)
        } else {
            stream_state.index
        },
        msg_seq: msg_seq_attempt.msg_seq,
        previous_success_msg_seq: msg_seq_attempt.previous_success_msg_seq,
        index_committed,
        msg_seq_committed: request_succeeded,
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

pub(crate) fn extract_sent_ref_index_id(body: &str) -> Option<String> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let candidates = [
        value.get("msg_idx"),
        value.get("ref_msg_idx"),
        value.get("d").and_then(|item| item.get("msg_idx")),
        value.get("d").and_then(|item| item.get("ref_msg_idx")),
        value.get("data").and_then(|item| item.get("msg_idx")),
        value.get("data").and_then(|item| item.get("ref_msg_idx")),
        value.get("message").and_then(|item| item.get("msg_idx")),
        value
            .get("message")
            .and_then(|item| item.get("ref_msg_idx")),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(crate) fn extract_sent_message_ids(body: &str) -> SendMessageIds {
    SendMessageIds {
        message_id: extract_sent_message_id(body),
        ref_index_id: extract_sent_ref_index_id(body),
    }
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
        OutboundMessage::Image { fallback_text, .. }
        | OutboundMessage::ImagePlaceholder { fallback_text }
        | OutboundMessage::AttachmentPlaceholder { fallback_text } => {
            sender.send_text(target, fallback_text).await
        }
    }
}

pub mod member_detail;

#[cfg(test)]
mod tests;

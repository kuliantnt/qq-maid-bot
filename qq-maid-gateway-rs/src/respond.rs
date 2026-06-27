use reqwest::{StatusCode, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{
    event::{C2cMessage, GroupMessage},
    logging::{mask_openid, reqwest_error_summary},
};

#[derive(Debug, Clone)]
pub struct RespondClient {
    client: reqwest::Client,
    respond_url: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RespondRequest {
    pub scope_key: String,
    pub content: String,
    /// 引用消息的正文（仅当平台可解析引用内容时填充，无引用或平台未提供时为空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_text: Option<String>,
    pub platform: String,
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guild_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct UpstreamCheckRequest {
    scope_key: &'static str,
    content: &'static str,
    platform: &'static str,
    event_type: &'static str,
    diagnostic: &'static str,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RespondResponse {
    pub ok: bool,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub markdown: Option<String>,
    #[serde(default)]
    pub handled: Option<bool>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub diagnostics: Option<Value>,
    #[serde(default)]
    pub error: Option<Value>,
}

#[derive(Debug)]
pub enum RespondTransport {
    Json(RespondResponse),
    Stream(RespondStream),
}

#[derive(Debug)]
pub enum RespondStreamEvent {
    Delta { text: String },
    Final { response: RespondResponse },
}

#[derive(Debug)]
pub struct RespondStream {
    pub receiver: mpsc::Receiver<RespondStreamEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RespondErrorInfo {
    code: String,
    message: String,
    stage: String,
}

#[derive(Debug, Error)]
pub enum RespondError {
    #[error("respond request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("respond endpoint returned {status}")]
    Status { status: StatusCode, body: String },
    #[error("upstream check failed: {summary}")]
    UpstreamCheck { summary: String },
}

impl RespondError {
    pub fn log_summary(&self) -> String {
        match self {
            Self::Http(error) => reqwest_error_summary(error),
            Self::Status { status, .. } => format!("http status {status}"),
            Self::UpstreamCheck { summary } => summary.clone(),
        }
    }

    pub fn qq_visible_kind(&self) -> String {
        match self {
            Self::Status { status, .. } => format!("http status {}", status.as_u16()),
            Self::UpstreamCheck { summary } => summary.clone(),
            Self::Http(error) if error.is_timeout() => "timeout".to_owned(),
            Self::Http(error) if error.is_connect() => "connect".to_owned(),
            Self::Http(error) if error.is_decode() => "decode".to_owned(),
            Self::Http(_) => "request failed".to_owned(),
        }
    }
}

pub fn respond_error_to_qq_text(err: &RespondError) -> String {
    match err {
        // 非 2xx 时优先复用后端返回的结构化错误，避免 QQ 端只看到笼统状态码。
        RespondError::Status { body, .. } => error_info_from_status_body(body)
            .map(|info| respond_error_info_to_qq_text(&info))
            .unwrap_or_else(|| {
                format!("LLM 服务暂时不可用：{}，请稍后再试", err.qq_visible_kind())
            }),
        RespondError::Http(_) => {
            format!("LLM 服务暂时不可用：{}，请稍后再试", err.qq_visible_kind())
        }
        RespondError::UpstreamCheck { summary } => summary.clone(),
    }
}

pub fn respond_response_error_to_qq_text(response: &RespondResponse) -> Option<String> {
    error_info_from_response(response).map(|info| respond_error_info_to_qq_text(&info))
}

/// `/v1/respond` 返回 `ok: false` 时，统一转换为可直接回给 QQ 的安全错误文案。
pub fn respond_not_ok_to_qq_text(response: &RespondResponse) -> String {
    respond_response_error_to_qq_text(response).unwrap_or_else(|| "处理失败，请稍后再试".to_owned())
}

pub fn respond_response_error_summary(response: &RespondResponse) -> String {
    error_info_from_response(response)
        .map(|info| format!("{}@{}", info.code, info.stage))
        .unwrap_or_else(|| "respond_not_ok".to_owned())
}

impl RespondClient {
    pub fn new(client: reqwest::Client, respond_url: impl Into<String>) -> Self {
        Self {
            client,
            respond_url: respond_url.into(),
        }
    }

    /// `/ping check` 使用独立诊断请求，不携带 QQ 用户或消息内容。
    pub async fn check_upstream(&self) -> Result<(), RespondError> {
        let response = self
            .client
            .post(&self.respond_url)
            .json(&UpstreamCheckRequest {
                scope_key: "diagnostic:upstream_check",
                content: "",
                platform: "gateway_diagnostic",
                event_type: "upstream_check",
                diagnostic: "upstream_check",
            })
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RespondError::Status { status, body });
        }
        // LLM 会把检查结果写入 healthz 快照；这里仅确保响应体是合法 JSON，
        // 不把后端错误正文直接用于 `/ping` 展示。
        let response = response.json::<RespondResponse>().await?;
        if !response.ok {
            return Err(RespondError::UpstreamCheck {
                summary: upstream_check_error_summary(&response),
            });
        }
        Ok(())
    }

    pub async fn respond_c2c(
        &self,
        message: &C2cMessage,
        content: String,
        reply_text: Option<String>,
    ) -> Result<RespondTransport, RespondError> {
        let request = RespondRequest::from_c2c_message(message, content, reply_text);
        let masked_user = mask_openid(&message.user_openid);
        let response = self
            .client
            .post(&self.respond_url)
            .header(
                reqwest::header::ACCEPT,
                "text/event-stream, application/json",
            )
            .json(&request)
            .send()
            .await
            .map_err(|error| {
                warn!(
                    message_id = %message.message_id,
                    user = %masked_user,
                    error = %reqwest_error_summary(&error),
                    "respond request failed"
                );
                RespondError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            warn!(
                message_id = %message.message_id,
                user = %masked_user,
                status = %status,
                "respond endpoint returned non-success status"
            );
            let body = response.text().await.unwrap_or_default();
            return Err(RespondError::Status { status, body });
        }

        if is_stream_response(response.headers().get(CONTENT_TYPE)) {
            info!(
                message_id = %message.message_id,
                user = %masked_user,
                "respond stream established"
            );
            return Ok(self.spawn_respond_stream(
                response,
                message.message_id.clone(),
                masked_user,
            ));
        }

        let response = response.json::<RespondResponse>().await.map_err(|error| {
            warn!(
                message_id = %message.message_id,
                user = %masked_user,
                error = %reqwest_error_summary(&error),
                "respond response decode failed"
            );
            RespondError::Http(error)
        })?;
        info!(
            message_id = %message.message_id,
            user = %masked_user,
            handled = response.handled.unwrap_or(false),
            handled_present = response.handled.is_some(),
            command = response.command.as_deref().unwrap_or(""),
            reply_len = response.text.as_deref().map(|text| text.chars().count()).unwrap_or(0),
            "respond request succeeded"
        );
        Ok(RespondTransport::Json(response))
    }

    pub async fn respond_group(
        &self,
        message: &GroupMessage,
        content: String,
        reply_text: Option<String>,
    ) -> Result<RespondTransport, RespondError> {
        let request = RespondRequest::from_group_message(message, content, reply_text);
        let masked_group = mask_openid(&message.group_openid);
        let response = self
            .client
            .post(&self.respond_url)
            .header(
                reqwest::header::ACCEPT,
                "text/event-stream, application/json",
            )
            .json(&request)
            .send()
            .await
            .map_err(|error| {
                warn!(
                    message_id = %message.message_id,
                    group = %masked_group,
                    error = %reqwest_error_summary(&error),
                    "respond group request failed"
                );
                RespondError::Http(error)
            })?;

        let status = response.status();
        if !status.is_success() {
            warn!(
                message_id = %message.message_id,
                group = %masked_group,
                status = %status,
                "respond group endpoint returned non-success status"
            );
            let body = response.text().await.unwrap_or_default();
            return Err(RespondError::Status { status, body });
        }

        if is_stream_response(response.headers().get(CONTENT_TYPE)) {
            info!(
                message_id = %message.message_id,
                group = %masked_group,
                "respond group stream established"
            );
            return Ok(self.spawn_respond_stream(
                response,
                message.message_id.clone(),
                masked_group,
            ));
        }

        let response = response.json::<RespondResponse>().await.map_err(|error| {
            warn!(
                message_id = %message.message_id,
                group = %masked_group,
                error = %reqwest_error_summary(&error),
                "respond group response decode failed"
            );
            RespondError::Http(error)
        })?;
        info!(
            message_id = %message.message_id,
            group = %masked_group,
            handled = response.handled.unwrap_or(false),
            handled_present = response.handled.is_some(),
            command = response.command.as_deref().unwrap_or(""),
            reply_len = response.text.as_deref().map(|text| text.chars().count()).unwrap_or(0),
            "respond group request succeeded"
        );
        Ok(RespondTransport::Json(response))
    }

    fn spawn_respond_stream(
        &self,
        response: reqwest::Response,
        message_id: String,
        masked_user: String,
    ) -> RespondTransport {
        let (event_tx, event_rx) = mpsc::channel(16);
        tokio::spawn(async move {
            let mut buffer = Vec::new();
            let mut response = response;
            while let Some(chunk) = match response.chunk().await {
                Ok(chunk) => chunk,
                Err(error) => {
                    warn!(
                        message_id = %message_id,
                        user = %masked_user,
                        error = %reqwest_error_summary(&error),
                        "respond stream read failed"
                    );
                    send_stream_final_error(&event_tx, "respond stream read failed").await;
                    return;
                }
            } {
                buffer.extend_from_slice(&chunk);
                while let Some(frame) = take_sse_frame(&mut buffer) {
                    match parse_sse_frame(&frame) {
                        Ok(Some(event)) => match event {
                            ParsedSseEvent::Delta(text) => {
                                if !text.is_empty() {
                                    let _ = event_tx.send(RespondStreamEvent::Delta { text }).await;
                                }
                            }
                            ParsedSseEvent::Final(response) => {
                                let _ = event_tx.send(RespondStreamEvent::Final { response }).await;
                                return;
                            }
                        },
                        Ok(None) => {}
                        Err(error) => {
                            warn!(
                                message_id = %message_id,
                                user = %masked_user,
                                error = %error,
                                "respond stream frame decode failed"
                            );
                            send_stream_final_error(&event_tx, &error).await;
                            return;
                        }
                    }
                }
            }

            if !buffer.is_empty() {
                match parse_sse_frame(&buffer) {
                    Ok(Some(event)) => match event {
                        ParsedSseEvent::Delta(text) => {
                            if !text.is_empty() {
                                let _ = event_tx.send(RespondStreamEvent::Delta { text }).await;
                            }
                        }
                        ParsedSseEvent::Final(response) => {
                            let _ = event_tx.send(RespondStreamEvent::Final { response }).await;
                            return;
                        }
                    },
                    Ok(None) => {}
                    Err(error) => {
                        warn!(
                            message_id = %message_id,
                            user = %masked_user,
                            error = %error,
                            "respond stream frame decode failed"
                        );
                        send_stream_final_error(&event_tx, &error).await;
                        return;
                    }
                }
            }

            warn!(
                message_id = %message_id,
                user = %masked_user,
                "respond stream ended without final event"
            );
            send_stream_final_error(&event_tx, "respond stream ended without final event").await;
        });

        RespondTransport::Stream(RespondStream { receiver: event_rx })
    }
}

#[derive(Debug)]
enum ParsedSseEvent {
    Delta(String),
    Final(RespondResponse),
}

fn is_stream_response(content_type: Option<&reqwest::header::HeaderValue>) -> bool {
    content_type
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
        .unwrap_or(false)
}

fn take_sse_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let (index, delimiter_len) = find_sse_delimiter(buffer)?;
    let frame = buffer[..index].to_vec();
    buffer.drain(..index + delimiter_len);
    Some(frame)
}

fn find_sse_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(a), Some(b)) if a < b => Some((a, 2)),
        (Some(_), Some(b)) => Some((b, 4)),
        (Some(a), None) => Some((a, 2)),
        (None, Some(b)) => Some((b, 4)),
        (None, None) => None,
    }
}

fn parse_sse_frame(frame: &[u8]) -> Result<Option<ParsedSseEvent>, String> {
    let text = std::str::from_utf8(frame).map_err(|err| format!("invalid SSE UTF-8: {err}"))?;
    let mut event = None;
    let mut data_lines = Vec::new();
    for raw_line in text.replace("\r\n", "\n").lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim_start().to_owned());
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_owned());
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let data = data_lines.join("\n");
    if data.trim() == "[DONE]" {
        return Ok(None);
    }

    match event.as_deref().unwrap_or("") {
        "delta" => Ok(Some(ParsedSseEvent::Delta(data))),
        "final" => {
            let response = serde_json::from_str::<RespondResponse>(&data)
                .map_err(|err| format!("invalid final response JSON: {err}"))?;
            Ok(Some(ParsedSseEvent::Final(response)))
        }
        _ => Ok(None),
    }
}

async fn send_stream_final_error(event_tx: &mpsc::Sender<RespondStreamEvent>, message: &str) {
    let response = RespondResponse {
        ok: false,
        text: None,
        markdown: None,
        handled: Some(false),
        session_id: None,
        command: None,
        diagnostics: None,
        error: Some(serde_json::json!({
            "code": "http_error",
            "message": message,
            "stage": "stream",
        })),
    };
    let _ = event_tx.send(RespondStreamEvent::Final { response }).await;
}

impl RespondRequest {
    pub fn from_c2c_message(
        message: &C2cMessage,
        content: String,
        reply_text: Option<String>,
    ) -> Self {
        Self {
            scope_key: format!("private:{}", message.user_openid),
            content,
            reply_text,
            platform: "qq_official".to_owned(),
            event_type: "c2c_message".to_owned(),
            user_id: Some(message.user_openid.clone()),
            group_id: None,
            guild_id: None,
            channel_id: None,
            message_id: Some(message.message_id.clone()),
            timestamp: message.timestamp.clone(),
        }
    }

    pub fn from_group_message(
        message: &GroupMessage,
        content: String,
        reply_text: Option<String>,
    ) -> Self {
        Self {
            // 群聊 scope_key 必须保持"当前 QQ 目标"语义，避免把 RSS、会话等群级能力
            // 意外拆成成员分片；成员身份只通过 user_id 和冷却逻辑参与判断。
            scope_key: format!("group:{}", message.group_openid),
            content,
            reply_text,
            platform: "qq_official".to_owned(),
            event_type: message.event_type.as_respond_event_type().to_owned(),
            user_id: message.member_openid.clone(),
            group_id: Some(message.group_openid.clone()),
            guild_id: None,
            channel_id: None,
            message_id: Some(message.message_id.clone()),
            timestamp: message.timestamp.clone(),
        }
    }
}

/// 提取引用消息的正文（用于 `reply_text`），优先从已解析的 reply.content 获取。
/// 缓存未命中时返回 None，由 Egress 层正常降级，不伪造引用正文。
pub(crate) fn extract_reply_text(message: &C2cMessage) -> Option<String> {
    message
        .reply
        .as_ref()
        .and_then(|reply| reply.content.as_deref())
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.trim().to_owned())
}

/// 提取群引用消息的正文。
pub(crate) fn extract_group_reply_text(message: &GroupMessage) -> Option<String> {
    message
        .reply
        .as_ref()
        .and_then(|reply| reply.content.as_deref())
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.trim().to_owned())
}

/// Egress 层是 gateway 内唯一允许拼接 `/v1/respond` 语义字符串的位置。
/// 注意：引用正文不再拼入 content，而是通过 `reply_text` 字段独立传递。
pub fn build_respond_content(message: &C2cMessage) -> String {
    build_egress_content(&message.content, &message.attachments)
}

pub fn build_group_respond_content(message: &GroupMessage) -> String {
    build_egress_content(&message.content, &message.attachments)
}

/// 构造发送给 `/v1/respond` 的 content 字段，只包含用户正文和附件备注，
/// 不包含引用块（引用内容通过 `reply_text` 字段独立传递）。
fn build_egress_content(message_content: &str, attachments: &[crate::event::Attachment]) -> String {
    let mut content = String::new();
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

fn error_info_from_status_body(body: &str) -> Option<RespondErrorInfo> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let error = value.get("error").unwrap_or(&value);
    error_info_from_value(error)
}

fn error_info_from_response(response: &RespondResponse) -> Option<RespondErrorInfo> {
    response.error.as_ref().and_then(error_info_from_value)
}

fn error_info_from_value(value: &Value) -> Option<RespondErrorInfo> {
    let object = value.as_object()?;
    let code = object.get("code")?.as_str()?.trim();
    let stage = object.get("stage")?.as_str()?.trim();
    let message = object.get("message")?.as_str()?.trim();
    if code.is_empty() || stage.is_empty() || message.is_empty() {
        return None;
    }
    Some(RespondErrorInfo {
        code: code.to_owned(),
        message: message.to_owned(),
        stage: stage.to_owned(),
    })
}

fn respond_error_info_to_qq_text(info: &RespondErrorInfo) -> String {
    let safe_message = sanitize_visible_error_message(&info.message);
    match info.code.as_str() {
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
            .unwrap_or_else(|| format!("处理失败（阶段：{}，错误码：{}）", info.stage, info.code)),
    }
}

/// 主动检查的后端 message 已经是白名单摘要；Gateway 再过滤一次后保留其诊断价值。
fn upstream_check_error_summary(response: &RespondResponse) -> String {
    error_info_from_response(response)
        .and_then(|info| sanitize_visible_error_message(&info.message))
        .unwrap_or_else(|| "上游检查失败（错误详情已隐藏）".to_owned())
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
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };

    use super::*;
    use crate::event::C2cMessage;

    fn spawn_one_response_server(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(response);
        });
        format!("http://{addr}")
    }

    fn spawn_hanging_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let Ok((_stream, _)) = listener.accept() else {
                return;
            };
            thread::sleep(Duration::from_secs(1));
        });
        format!("http://{addr}")
    }

    fn spawn_capture_server() -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0; 4096];
            let size = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..size]).into_owned();
            let _ = sender.send(request);
            let body = br#"{"ok":true,"diagnostics":{"upstream_check":true}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.write_all(body);
        });
        (format!("http://{addr}/v1/respond"), receiver)
    }

    async fn request_error_for(url: &str) -> reqwest::Error {
        reqwest::Client::new()
            .get(url)
            .send()
            .await
            .expect_err("request should fail")
    }

    #[tokio::test]
    async fn upstream_check_uses_diagnostic_payload_without_qq_identity() {
        let (url, request_rx) = spawn_capture_server();
        let client = RespondClient::new(reqwest::Client::new(), url);

        client.check_upstream().await.unwrap();
        let request = request_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        assert!(request.starts_with("POST /v1/respond HTTP/1.1"));
        assert!(request.contains(r#""diagnostic":"upstream_check""#));
        assert!(request.contains(r#""scope_key":"diagnostic:upstream_check""#));
        assert!(!request.contains("user_id"));
        assert!(!request.contains("message_id"));
        assert!(!request.contains("openid"));
    }

    #[tokio::test]
    async fn upstream_check_treats_not_ok_response_as_failure_without_leaking_message() {
        let url = spawn_one_response_server(
            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{\"ok\":false,\"error\":{\"code\":\"provider_error\",\"stage\":\"http\",\"message\":\"Authorization: Bearer sk-secret-token\"}}",
        );
        let client = RespondClient::new(reqwest::Client::new(), url);

        let error = client.check_upstream().await.unwrap_err();

        let summary = error.qq_visible_kind();
        assert_eq!(summary, "上游检查失败（错误详情已隐藏）");
        assert!(!summary.contains("Authorization"));
        assert!(!summary.contains("Bearer"));
        assert!(!summary.contains("sk-secret"));
    }

    #[tokio::test]
    async fn upstream_check_preserves_safe_actionable_error_summary() {
        let url = spawn_one_response_server(
            b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{\"ok\":false,\"error\":{\"code\":\"http_error\",\"stage\":\"http\",\"message\":\"\\u4e0a\\u6e38\\u9274\\u6743\\u5931\\u8d25\\uff08HTTP 401\\uff09\"}}",
        );
        let client = RespondClient::new(reqwest::Client::new(), url);

        let error = client.check_upstream().await.unwrap_err();

        assert_eq!(error.qq_visible_kind(), "上游鉴权失败（HTTP 401）");
    }

    #[test]
    fn respond_payload_has_only_http_schema_fields() {
        let message = C2cMessage {
            message_id: "msg-1".to_owned(),
            user_openid: "user-1".to_owned(),
            content: "你好".to_owned(),
            reply: None,
            timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
            attachments: Vec::new(),
        };

        let reply_text = extract_reply_text(&message);
        let value = serde_json::to_value(RespondRequest::from_c2c_message(
            &message,
            build_respond_content(&message),
            reply_text,
        ))
        .unwrap();
        let object = value.as_object().unwrap();
        let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
        keys.sort_unstable();

        assert_eq!(
            keys,
            [
                "content",
                "event_type",
                "message_id",
                "platform",
                "scope_key",
                "timestamp",
                "user_id",
            ]
        );
        assert_eq!(value["scope_key"], "private:user-1");
        assert_eq!(value["platform"], "qq_official");
        assert_eq!(value["event_type"], "c2c_message");
        // reply_text 为 None 时被 skip_serializing_if 跳过
        assert!(
            value.get("reply_text").is_none(),
            "None reply_text should be skipped"
        );
        assert!(value.get("attachments").is_none());
        assert!(value.get("metadata").is_none());
        assert!(value.get("session_id").is_none());
        assert!(value.get("user_text").is_none());
    }

    #[test]
    fn respond_payload_passes_reply_text_separately() {
        let message = C2cMessage {
            message_id: "msg-1".to_owned(),
            user_openid: "user-1".to_owned(),
            content: "你好".to_owned(),
            reply: Some(crate::event::MessageReply {
                message_id: "quoted-1".to_owned(),
                content: Some("上一条".to_owned()),
            }),
            timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
            attachments: Vec::new(),
        };

        let reply_text = extract_reply_text(&message);
        let request =
            RespondRequest::from_c2c_message(&message, build_respond_content(&message), reply_text);

        // content 不应包含引用块，引用正文通过 reply_text 独立传递
        assert_eq!(request.content, "你好");
        assert_eq!(request.reply_text.as_deref(), Some("上一条"));
    }

    #[test]
    fn group_respond_payload_uses_group_scope_and_real_event_type() {
        let message = GroupMessage {
            message_id: "msg-1".to_owned(),
            group_openid: "group-1".to_owned(),
            member_openid: Some("user-1".to_owned()),
            content: "/rss".to_owned(),
            reply: None,
            timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
            attachments: Vec::new(),
            event_type: crate::event::GroupEventType::GroupMessage,
            author_is_bot: false,
            author_is_self: false,
        };

        let reply_text = extract_group_reply_text(&message);
        let request = RespondRequest::from_group_message(
            &message,
            build_group_respond_content(&message),
            reply_text,
        );

        assert_eq!(request.scope_key, "group:group-1");
        assert_eq!(request.event_type, "group_message");
        assert_eq!(request.user_id.as_deref(), Some("user-1"));
        assert_eq!(request.group_id.as_deref(), Some("group-1"));
        assert_eq!(request.reply_text, None);
    }

    #[test]
    fn group_respond_payload_keeps_group_scope_when_member_missing() {
        let message = GroupMessage {
            message_id: "msg-1".to_owned(),
            group_openid: "group-1".to_owned(),
            member_openid: None,
            content: "/rss".to_owned(),
            reply: None,
            timestamp: None,
            attachments: Vec::new(),
            event_type: crate::event::GroupEventType::GroupAtMessage,
            author_is_bot: false,
            author_is_self: false,
        };

        let reply_text = extract_group_reply_text(&message);
        let request = RespondRequest::from_group_message(
            &message,
            build_group_respond_content(&message),
            reply_text,
        );

        assert_eq!(request.scope_key, "group:group-1");
        assert_eq!(request.event_type, "group_at_message");
        assert_eq!(request.user_id, None);
        assert_eq!(request.reply_text, None);
    }

    #[test]
    fn build_respond_content_appends_attachment_notes_in_egress() {
        let message = C2cMessage {
            message_id: "msg-1".to_owned(),
            user_openid: "user-1".to_owned(),
            content: "你好".to_owned(),
            reply: None,
            timestamp: None,
            attachments: vec![crate::event::Attachment {
                content_type: Some("image/jpeg".to_owned()),
                filename: Some("a.jpg".to_owned()),
                url: Some("https://example.test/a.jpg".to_owned()),
            }],
        };

        assert_eq!(
            build_respond_content(&message),
            "你好\n[附件 image/jpeg: a.jpg https://example.test/a.jpg]"
        );
    }

    #[test]
    fn parses_sse_frames_for_delta_and_final() {
        let mut delta_buffer = "event: delta\ndata: 你".as_bytes().to_vec();
        assert!(take_sse_frame(&mut delta_buffer).is_none());
        delta_buffer.extend_from_slice("好\n\n".as_bytes());
        let delta_frame = take_sse_frame(&mut delta_buffer).unwrap();
        match parse_sse_frame(&delta_frame).unwrap().unwrap() {
            ParsedSseEvent::Delta(text) => assert_eq!(text, "你好"),
            ParsedSseEvent::Final(_) => panic!("expected delta event"),
        }

        let mut final_buffer = format!(
            "event: final\ndata: {}\n\n",
            serde_json::json!({
                "ok": true,
                "text": "完成",
                "markdown": "# 完成",
                "handled": true,
                "error": null,
            })
        )
        .into_bytes();
        let final_frame = take_sse_frame(&mut final_buffer).unwrap();
        match parse_sse_frame(&final_frame).unwrap().unwrap() {
            ParsedSseEvent::Final(response) => {
                assert!(response.ok);
                assert_eq!(response.text.as_deref(), Some("完成"));
                assert_eq!(response.markdown.as_deref(), Some("# 完成"));
            }
            ParsedSseEvent::Delta(_) => panic!("expected final event"),
        }
    }

    #[tokio::test]
    async fn respond_c2c_preserves_markdown_field_from_json_response() {
        let url = spawn_one_response_server(
            "HTTP/1.1 200 OK
Content-Type: application/json
Connection: close

{\"ok\":true,\"text\":\"标题\\n· hello\",\"markdown\":\"# 标题\\n- hello\",\"handled\":true}"
                .as_bytes(),
        );
        let client = RespondClient::new(reqwest::Client::new(), url);
        let message = C2cMessage {
            message_id: "msg-1".to_owned(),
            user_openid: "user-1".to_owned(),
            content: "你好".to_owned(),
            reply: None,
            timestamp: None,
            attachments: Vec::new(),
        };

        let response = client
            .respond_c2c(&message, build_respond_content(&message), None)
            .await
            .unwrap();

        match response {
            RespondTransport::Json(response) => {
                assert_eq!(response.text.as_deref(), Some("标题\n· hello"));
                assert_eq!(response.markdown.as_deref(), Some("# 标题\n- hello"));
            }
            RespondTransport::Stream(_) => panic!("expected json response"),
        }
    }

    #[tokio::test]
    async fn respond_group_preserves_markdown_field_from_json_response() {
        let url = spawn_one_response_server(
            "HTTP/1.1 200 OK
Content-Type: application/json
Connection: close

{\"ok\":true,\"text\":\"标题\\n· hello\",\"markdown\":\"# 标题\\n- hello\",\"handled\":true}"
                .as_bytes(),
        );
        let client = RespondClient::new(reqwest::Client::new(), url);
        let message = GroupMessage {
            message_id: "msg-1".to_owned(),
            group_openid: "group-1".to_owned(),
            member_openid: Some("user-1".to_owned()),
            content: "你好".to_owned(),
            reply: None,
            timestamp: None,
            attachments: Vec::new(),
            event_type: crate::event::GroupEventType::GroupAtMessage,
            author_is_bot: false,
            author_is_self: false,
        };

        let response = client
            .respond_group(&message, build_group_respond_content(&message), None)
            .await
            .unwrap();

        match response {
            RespondTransport::Json(response) => {
                assert_eq!(response.text.as_deref(), Some("标题\n· hello"));
                assert_eq!(response.markdown.as_deref(), Some("# 标题\n- hello"));
            }
            RespondTransport::Stream(_) => panic!("expected json response"),
        }
    }

    #[tokio::test]
    async fn qq_visible_kind_classifies_timeout() {
        let url = spawn_hanging_server();
        let error = reqwest::Client::builder()
            .timeout(Duration::from_millis(10))
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .expect_err("request should time out");

        assert_eq!(RespondError::Http(error).qq_visible_kind(), "timeout");
    }

    #[tokio::test]
    async fn qq_visible_kind_classifies_connect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let error = request_error_for(&format!("http://{addr}")).await;

        assert_eq!(RespondError::Http(error).qq_visible_kind(), "connect");
    }

    #[tokio::test]
    async fn qq_visible_kind_classifies_decode() {
        let url = spawn_one_response_server(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 8\r\n\r\nnot json",
        );
        let error = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .unwrap()
            .json::<RespondResponse>()
            .await
            .expect_err("response JSON should fail to decode");

        assert_eq!(RespondError::Http(error).qq_visible_kind(), "decode");
    }

    #[test]
    fn qq_visible_kind_classifies_http_status_without_body() {
        let error = RespondError::Status {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "upstream body with token secret and response details".to_owned(),
        };

        assert_eq!(error.qq_visible_kind(), "http status 500");
        let qq_text = respond_error_to_qq_text(&error);
        assert_eq!(qq_text, "LLM 服务暂时不可用：http status 500，请稍后再试");
        assert!(!qq_text.contains("upstream body"));
        assert!(!qq_text.contains("token"));
        assert!(!qq_text.contains("secret"));
    }

    #[test]
    fn status_error_prefers_structured_backend_error_message() {
        let error = RespondError::Status {
            status: StatusCode::BAD_REQUEST,
            body: serde_json::json!({
                "ok": false,
                "error": {
                    "code": "config",
                    "message": "OPENAI_API_KEY is required",
                    "stage": "config",
                }
            })
            .to_string(),
        };

        assert_eq!(
            respond_error_to_qq_text(&error),
            "LLM 服务配置未完成，请联系维护者处理"
        );
    }

    #[test]
    fn status_error_structured_message_with_sensitive_detail_falls_back_to_safe_text() {
        let error = RespondError::Status {
            status: StatusCode::BAD_GATEWAY,
            body: serde_json::json!({
                "ok": false,
                "error": {
                    "code": "provider_error",
                    "message": "request failed with Authorization: Bearer sk-secret-token",
                    "stage": "provider",
                }
            })
            .to_string(),
        };

        assert_eq!(
            respond_error_to_qq_text(&error),
            "上游服务暂时不可用，请稍后再试"
        );
    }

    #[test]
    fn status_error_safety_blocked_uses_fixed_user_message() {
        let error = RespondError::Status {
            status: StatusCode::OK,
            body: serde_json::json!({
                "ok": false,
                "error": {
                    "code": "safety_blocked",
                    "message": "Chat Completions returned HTTP 400: {\"error\":{\"message\":\"request blocked by moderation policy\",\"type\":\"prompt_blocked\"}}",
                    "stage": "http",
                }
            })
            .to_string(),
        };

        let qq_text = respond_error_to_qq_text(&error);
        assert_eq!(
            qq_text,
            "这条消息触发了上游安全拦截，我没法按原样继续。可以换个说法再试。"
        );
        assert!(!qq_text.contains("请求格式有误"));
        assert!(!qq_text.contains("prompt_blocked"));
    }

    #[test]
    fn response_error_prefers_structured_message_for_qq_text() {
        let response = RespondResponse {
            ok: false,
            text: None,
            markdown: None,
            handled: Some(false),
            session_id: None,
            command: None,
            diagnostics: None,
            error: Some(serde_json::json!({
                "code": "bad_request",
                "message": "query must not be empty",
                "stage": "request",
            })),
        };

        assert_eq!(
            respond_response_error_to_qq_text(&response).as_deref(),
            Some("请求格式有误：query must not be empty")
        );
        assert_eq!(
            respond_response_error_summary(&response),
            "bad_request@request"
        );
    }

    #[test]
    fn response_error_redacts_sensitive_message_before_showing_to_qq() {
        let response = RespondResponse {
            ok: false,
            text: None,
            markdown: None,
            handled: Some(false),
            session_id: None,
            command: None,
            diagnostics: None,
            error: Some(serde_json::json!({
                "code": "http_error",
                "message": "OpenAI request failed: https://example.test/v1/respond?token=abc",
                "stage": "http",
            })),
        };

        assert_eq!(
            respond_response_error_to_qq_text(&response).as_deref(),
            Some("上游服务暂时不可用，请稍后再试")
        );
    }

    #[test]
    fn respond_not_ok_falls_back_to_generic_text_when_error_payload_missing() {
        let response = RespondResponse {
            ok: false,
            text: Some("internal debug text".to_owned()),
            markdown: None,
            handled: Some(false),
            session_id: None,
            command: None,
            diagnostics: None,
            error: None,
        };

        assert_eq!(respond_not_ok_to_qq_text(&response), "处理失败，请稍后再试");
    }

    #[tokio::test]
    async fn qq_visible_kind_classifies_other_request_failure() {
        let error = request_error_for("http://").await;

        assert_eq!(
            RespondError::Http(error).qq_visible_kind(),
            "request failed"
        );
    }
}

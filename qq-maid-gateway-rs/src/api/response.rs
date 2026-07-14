//! QQ OpenAPI 响应体解析。
//!
//! 这里仅负责从兼容的响应结构中提取发送结果和流式错误字段，不参与请求发送、
//! 重试或 fallback 决策，避免协议兼容路径继续堆积在 API 客户端主模块中。

use serde::Deserialize;
use serde_json::Value;

use super::SendMessageIds;

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

pub(super) fn extract_c2c_text_stream_id(body: &str) -> Option<String> {
    let response = serde_json::from_str::<C2cStreamSendResponse>(body).ok()?;
    response
        .id
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
}

pub(super) fn qq_api_error_fields(body: &str) -> (Option<String>, Option<String>) {
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

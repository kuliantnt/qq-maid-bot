//! OneBot 11 一期文本 sender。
//!
//! sender 只负责把平台原始目标转换成 OneBot action，并以 API response 的真实结果
//! 判定发送成功。消息正文始终使用 segment 数组，Core 和主动推送层不接触 CQ 码。

use std::time::Instant;

use serde::Deserialize;
use serde_json::{Number, Value, json};
use thiserror::Error;
use tracing::{info, warn};

use crate::gateway::logging::mask_identifier;
use crate::media::ImagePayload;

use super::{
    OneBotCallError, OneBotConnectionContext,
    protocol::{ActionResponse, OneBotId},
};

const SEND_PRIVATE_MSG: &str = "send_private_msg";
const SEND_GROUP_MSG: &str = "send_group_msg";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneBotSendResult {
    /// OneBot 平台返回的真实消息 ID；不能与 Todo 等内部业务 ID 混用。
    pub message_id: String,
}

#[derive(Debug, Error)]
pub enum OneBotSendError {
    #[error(transparent)]
    Transport(#[from] OneBotCallError),
    #[error(
        "OneBot API rejected the send action: status={status}, retcode={retcode}, remote_message_present={remote_message_present}"
    )]
    Rejected {
        status: String,
        retcode: i64,
        /// 不保存服务端错误正文，只记录其是否存在，避免后续 Outbox 日志泄漏响应内容。
        remote_message_present: bool,
    },
    #[error("OneBot API send response is missing a valid data.message_id")]
    InvalidData,
    #[error("invalid OneBot target ID: expected a decimal unsigned 64-bit integer")]
    InvalidTargetId,
    #[error("invalid OneBot image payload: missing URL, absolute local path, or base64 data")]
    InvalidImage,
}

impl OneBotSendError {
    fn retcode(&self) -> Option<i64> {
        match self {
            Self::Rejected { retcode, .. } => Some(*retcode),
            Self::Transport(_) | Self::InvalidData | Self::InvalidTargetId | Self::InvalidImage => {
                None
            }
        }
    }
}

#[derive(Clone)]
pub struct OneBotSender {
    connection: OneBotConnectionContext,
}

impl OneBotSender {
    pub fn new(connection: OneBotConnectionContext) -> Self {
        Self { connection }
    }

    pub fn connected_account_id(&self) -> Option<String> {
        self.connection
            .connected_self_id()
            .map(|self_id| self_id.as_str().to_owned())
    }

    pub async fn send_private_text(
        &self,
        user_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        self.send_text(SEND_PRIVATE_MSG, "user_id", user_id, text)
            .await
    }

    pub async fn send_group_text(
        &self,
        group_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        self.send_text(SEND_GROUP_MSG, "group_id", group_id, text)
            .await
    }

    pub async fn send_private_image(
        &self,
        user_id: &str,
        image: &ImagePayload,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        self.send_image(SEND_PRIVATE_MSG, "user_id", user_id, image)
            .await
    }

    pub async fn send_group_image(
        &self,
        group_id: &str,
        image: &ImagePayload,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        self.send_image(SEND_GROUP_MSG, "group_id", group_id, image)
            .await
    }

    async fn send_text(
        &self,
        action: &'static str,
        target_key: &'static str,
        target_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        let started = Instant::now();
        let target_id = parse_target_id(target_id)?;
        let params = json!({
            // OneBot 11 的发送 action 要求 user_id/group_id 为 JSON number；这里直接从
            // 十进制字符串解析为 u64，不能经过 f64，否则较大 ID 会丢失精度。
            target_key: Value::Number(Number::from(target_id)),
            "message": [{"type": "text", "data": {"text": text}}]
        });
        let result = self
            .connection
            .call(action, params)
            .await
            .map_err(OneBotSendError::from)
            .and_then(validate_send_response);
        let elapsed_ms = started.elapsed().as_millis();
        let target = mask_identifier(&target_id.to_string());
        match &result {
            Ok(_) => info!(
                action,
                retcode = 0,
                elapsed_ms,
                target = %target,
                "OneBot 11 text sent"
            ),
            Err(error) => warn!(
                action,
                retcode = ?error.retcode(),
                elapsed_ms,
                target = %target,
                "OneBot 11 text send failed"
            ),
        }
        result
    }

    async fn send_image(
        &self,
        action: &'static str,
        target_key: &'static str,
        target_id: &str,
        image: &ImagePayload,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        let started = Instant::now();
        let target_id = parse_target_id(target_id)?;
        let params = build_image_params(target_key, target_id, image)?;
        let result = self
            .connection
            .call(action, params)
            .await
            .map_err(OneBotSendError::from)
            .and_then(validate_send_response);
        let elapsed_ms = started.elapsed().as_millis();
        let target = mask_identifier(&target_id.to_string());
        match &result {
            Ok(_) => info!(action, elapsed_ms, target = %target, "OneBot 11 image sent"),
            Err(error) => {
                warn!(action, retcode = ?error.retcode(), elapsed_ms, target = %target, "OneBot 11 image send failed")
            }
        }
        result
    }
}

fn onebot_image_file(image: &ImagePayload) -> Result<String, OneBotSendError> {
    if let Some(url) = image
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(url.to_owned());
    }
    if let Some(path) = image
        .local_path
        .as_deref()
        .map(str::trim)
        .filter(|value| std::path::Path::new(value).is_absolute())
    {
        return Ok(format!("file://{path}"));
    }
    image
        .data_base64
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|data| format!("base64://{data}"))
        .ok_or(OneBotSendError::InvalidImage)
}

fn build_image_params(
    target_key: &'static str,
    target_id: u64,
    image: &ImagePayload,
) -> Result<Value, OneBotSendError> {
    Ok(json!({
        target_key: Value::Number(Number::from(target_id)),
        "message": [{"type": "image", "data": {"file": onebot_image_file(image)?}}]
    }))
}

fn parse_target_id(target_id: &str) -> Result<u64, OneBotSendError> {
    if target_id.is_empty() || !target_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OneBotSendError::InvalidTargetId);
    }
    target_id
        .parse::<u64>()
        .map_err(|_| OneBotSendError::InvalidTargetId)
}

#[derive(Deserialize)]
struct SendMessageData {
    message_id: OneBotId,
}

fn validate_send_response(response: ActionResponse) -> Result<OneBotSendResult, OneBotSendError> {
    if response.status != "ok" || response.retcode != 0 {
        return Err(OneBotSendError::Rejected {
            // status 由远端提供，错误链可能进入 Outbox 日志；只保留协议分类，
            // 不回显任意字符串或完整 response envelope。
            status: if response.status == "ok" {
                "ok".to_owned()
            } else {
                "non-ok".to_owned()
            },
            retcode: response.retcode,
            remote_message_present: response
                .wording
                .as_deref()
                .or(response.message.as_deref())
                .is_some_and(|message| !message.trim().is_empty()),
        });
    }
    let data: SendMessageData =
        serde_json::from_value(response.data).map_err(|_| OneBotSendError::InvalidData)?;
    Ok(OneBotSendResult {
        message_id: data.message_id.as_str().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::gateway::onebot11::protocol::Echo;

    fn response(status: &str, retcode: i64, data: serde_json::Value) -> ActionResponse {
        ActionResponse {
            status: status.to_owned(),
            retcode,
            data,
            message: None,
            wording: None,
            echo: Some(Echo(json!("echo"))),
        }
    }

    #[test]
    fn accepts_numeric_or_string_message_id() {
        let numeric =
            validate_send_response(response("ok", 0, json!({"message_id": 123}))).unwrap();
        let text = validate_send_response(response("ok", 0, json!({"message_id": "456"}))).unwrap();

        assert_eq!(numeric.message_id, "123");
        assert_eq!(text.message_id, "456");
    }

    #[test]
    fn target_id_requires_decimal_u64_without_float_conversion() {
        assert_eq!(parse_target_id("18446744073709551615").unwrap(), u64::MAX);

        for invalid in ["", "abc", "-1", "+1", " 1", "18446744073709551616"] {
            assert!(matches!(
                parse_target_id(invalid),
                Err(OneBotSendError::InvalidTargetId)
            ));
        }
    }

    #[test]
    fn rejects_failed_status_nonzero_retcode_and_missing_message_id() {
        let status =
            validate_send_response(response("failed", 0, json!({"message_id": 1}))).unwrap_err();
        let retcode = validate_send_response(response("failed", 1404, json!(null))).unwrap_err();
        let missing = validate_send_response(response("ok", 0, json!({}))).unwrap_err();

        assert!(matches!(
            status,
            OneBotSendError::Rejected { retcode: 0, .. }
        ));
        assert!(matches!(
            retcode,
            OneBotSendError::Rejected { retcode: 1404, .. }
        ));
        assert!(matches!(missing, OneBotSendError::InvalidData));
    }

    #[test]
    fn image_segment_file_supports_url_and_base64() {
        assert_eq!(
            onebot_image_file(&ImagePayload::from_url("https://example.test/image.png")).unwrap(),
            "https://example.test/image.png"
        );
        assert_eq!(
            onebot_image_file(&ImagePayload::from_base64("aGVsbG8=")).unwrap(),
            "base64://aGVsbG8="
        );
        assert_eq!(
            onebot_image_file(&ImagePayload::from_local_path("/tmp/generated.png")).unwrap(),
            "file:///tmp/generated.png"
        );
        assert!(matches!(
            onebot_image_file(&ImagePayload::new("qq-file-info")),
            Err(OneBotSendError::InvalidImage)
        ));
    }

    #[test]
    fn group_image_params_use_standard_onebot_image_segment() {
        let params =
            build_image_params("group_id", 123, &ImagePayload::from_base64("aGVsbG8=")).unwrap();

        assert_eq!(params["group_id"], 123);
        assert_eq!(params["message"][0]["type"], "image");
        assert_eq!(params["message"][0]["data"]["file"], "base64://aGVsbG8=");
    }
}

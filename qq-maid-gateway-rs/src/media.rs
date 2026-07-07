//! QQ 富媒体图片出站载荷。
//!
//! `ImagePayload` 只携带图片来源 URL；发送层（`QqApiClient`）负责先调用 QQ 富媒体
//! 上传接口（`/v2/users/{openid}/files`、`/v2/groups/{group_openid}/files`）换取
//! `file_info`，再用 `msg_type=7 + media.file_info` 发送。render 层只做纯转换，
//! 不发 HTTP 请求，也不持有上传后的 `file_info`。

use serde::Serialize;
use serde_json::Value;

/// 出站图片的来源 URL。
///
/// 该值来自 `OutputMedia::url`，是 QQ 上传接口 `url` 字段的来源；发送层据此上传
/// 换取 `file_info` 后再发送。注意：入站媒体的 `media_id` / `file_id` 不能直接
/// 当作发送用 `file_info`，必须经上传接口获取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePayload {
    pub url: String,
}

impl ImagePayload {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

/// 发送图片消息（`msg_type=7`）的载荷。`media.file_info` 必须来自上传接口返回值。
#[derive(Debug, Serialize)]
struct ImageMessagePayload<'a> {
    msg_type: u8,
    media: ImageMediaRef<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

#[derive(Debug, Serialize)]
struct ImageMediaRef<'a> {
    file_info: &'a str,
}

/// 构建 C2C 图片消息载荷。`file_info` 必须由上传接口返回，不能使用入站媒体标识。
pub fn build_c2c_image_payload(file_info: &str, msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(ImageMessagePayload {
        msg_type: 7,
        media: ImageMediaRef { file_info },
        msg_id,
        msg_seq,
    })
    .expect("C2C image payload should serialize")
}

/// 构建群图片消息载荷。语义与 C2C 一致，区别仅在群发送端点。
pub fn build_group_image_payload(file_info: &str, msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(ImageMessagePayload {
        msg_type: 7,
        media: ImageMediaRef { file_info },
        msg_id,
        msg_seq,
    })
    .expect("group image payload should serialize")
}

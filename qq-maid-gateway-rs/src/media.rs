use std::fmt;

use serde::Serialize;
use serde_json::Value;

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ImagePayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    #[serde(skip)]
    pub local_path: Option<String>,
}

impl fmt::Debug for ImagePayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ImagePayload")
            .field("has_file_info", &self.file_info.is_some())
            // 图片 URL 可能包含临时签名，base64 则是完整图片内容，日志中均不得展开。
            .field("has_url", &self.url.is_some())
            .field("has_data_base64", &self.data_base64.is_some())
            .field("has_local_path", &self.local_path.is_some())
            .finish()
    }
}

#[derive(Debug, Serialize)]
struct C2cImagePayload<'a> {
    msg_type: u8,
    media: &'a ImagePayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

impl ImagePayload {
    pub fn new(file_info: impl Into<String>) -> Self {
        Self {
            file_info: Some(file_info.into()),
            url: None,
            data_base64: None,
            local_path: None,
        }
    }

    pub fn from_url(url: impl Into<String>) -> Self {
        Self {
            file_info: None,
            url: Some(url.into()),
            data_base64: None,
            local_path: None,
        }
    }

    pub fn from_base64(data_base64: impl Into<String>) -> Self {
        Self {
            file_info: None,
            url: None,
            data_base64: Some(data_base64.into()),
            local_path: None,
        }
    }

    pub fn from_local_path(local_path: impl Into<String>) -> Self {
        Self {
            file_info: None,
            url: None,
            data_base64: None,
            local_path: Some(local_path.into()),
        }
    }
}

pub fn build_c2c_image_payload(image: &ImagePayload, msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(C2cImagePayload {
        msg_type: 7,
        media: image,
        msg_id,
        msg_seq,
    })
    .expect("C2C image payload should serialize")
}

pub fn build_group_image_payload(
    image: &ImagePayload,
    msg_id: Option<&str>,
    msg_seq: u32,
) -> Value {
    build_c2c_image_payload(image, msg_id, msg_seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qq_group_image_payload_uses_uploaded_file_info() {
        let payload = build_group_image_payload(
            &ImagePayload::new("uploaded-file-info"),
            Some("source-message"),
            9,
        );

        assert_eq!(payload["msg_type"], 7);
        assert_eq!(payload["media"]["file_info"], "uploaded-file-info");
        assert!(payload["media"].get("url").is_none());
        assert!(payload["media"].get("data_base64").is_none());
        assert_eq!(payload["msg_id"], "source-message");
        assert_eq!(payload["msg_seq"], 9);
    }
}

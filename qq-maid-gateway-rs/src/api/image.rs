//! QQ 官方富媒体图片上传与发送。
//!
//! C2C 和群聊都必须先调用各自的 `/files` 接口换取 `file_info`，再通过消息接口
//! 发送 `msg_type=7`。这里禁止上传接口隐式发送，确保沿用统一的 `msg_id/msg_seq`
//! 编排和真实错误处理。

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::{ApiError, QqApiClient, SendResult};
use crate::media::{ImagePayload, build_c2c_image_payload, build_group_image_payload};

#[derive(Debug, Serialize)]
struct UploadImagePayload<'a> {
    file_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_data: Option<&'a str>,
    srv_send_msg: bool,
}

#[derive(Debug, Deserialize)]
struct UploadMediaResponse {
    file_info: String,
}

impl QqApiClient {
    pub async fn send_c2c_image(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        image: &ImagePayload,
    ) -> SendResult {
        let image = self.resolve_c2c_image(user_openid, image).await?;
        let payload = build_c2c_image_payload(&image, msg_id, self.next_msg_seq());
        self.post_c2c_message(user_openid, msg_id, "image", &payload)
            .await
    }

    pub async fn send_group_image(
        &self,
        group_openid: &str,
        msg_id: Option<&str>,
        image: &ImagePayload,
    ) -> SendResult {
        let image = self.resolve_group_image(group_openid, image).await?;
        let payload = build_group_image_payload(&image, msg_id, self.next_msg_seq());
        self.post_group_message(group_openid, msg_id, "image", &payload)
            .await
    }

    async fn resolve_c2c_image(
        &self,
        user_openid: &str,
        image: &ImagePayload,
    ) -> Result<ImagePayload, ApiError> {
        if image
            .file_info
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Ok(image.clone());
        }
        let endpoint = format!("{}/v2/users/{user_openid}/files", self.api_base);
        self.upload_image(endpoint, image, "private").await
    }

    async fn resolve_group_image(
        &self,
        group_openid: &str,
        image: &ImagePayload,
    ) -> Result<ImagePayload, ApiError> {
        if image
            .file_info
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Ok(image.clone());
        }
        let endpoint = format!("{}/v2/groups/{group_openid}/files", self.api_base);
        self.upload_image(endpoint, image, "group").await
    }

    async fn upload_image(
        &self,
        endpoint: String,
        image: &ImagePayload,
        scope: &'static str,
    ) -> Result<ImagePayload, ApiError> {
        let url = image
            .url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let file_data = image
            .data_base64
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if url.is_none() && file_data.is_none() {
            return Err(ApiError::InvalidMedia(
                "missing file_info, URL, or base64 data supported by QQ official",
            ));
        }
        let payload = UploadImagePayload {
            file_type: 1,
            url,
            file_data,
            // 禁止上传接口隐式发送；拿到 file_info 后统一走消息发送接口。
            srv_send_msg: false,
        };
        let response = self
            .client
            .post(endpoint)
            .header("Authorization", self.auth.authorization_header().await?)
            .json(&payload)
            .send()
            .await
            .map_err(ApiError::Http)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            warn!(scope, status = %status, "QQ image upload returned non-success status");
            return Err(ApiError::Status { status, body });
        }
        let uploaded: UploadMediaResponse = response.json().await.map_err(ApiError::Http)?;
        let file_info = uploaded.file_info.trim();
        if file_info.is_empty() {
            return Err(ApiError::InvalidMedia("upload response missing file_info"));
        }
        info!(scope, "QQ image upload succeeded");
        Ok(ImagePayload::new(file_info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_payload_uses_base64_file_data_without_implicit_send() {
        let payload = serde_json::to_value(UploadImagePayload {
            file_type: 1,
            url: None,
            file_data: Some("aGVsbG8="),
            srv_send_msg: false,
        })
        .unwrap();

        assert_eq!(payload["file_type"], 1);
        assert_eq!(payload["file_data"], "aGVsbG8=");
        assert_eq!(payload["srv_send_msg"], false);
        assert!(payload.get("url").is_none());
    }
}

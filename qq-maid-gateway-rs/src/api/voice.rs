//! QQ 官方语音 URL 上传与发送。
//!
//! 第一版只接受 Provider 给出的 HTTP(S) URL：在当前私聊/群聊目标下用
//! `file_type=3` 上传，再把同一目标返回的 `file_info` 作为 `msg_type=7` 发送。
//! 不下载音频、不发送 `file_data`，也不缓存或跨场景复用 `file_info`。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, warn};

use super::{ApiError, QqApiClient, SendResult};

#[derive(Clone, Serialize, PartialEq, Eq)]
pub struct VoiceMedia {
    pub file_info: String,
}

impl std::fmt::Debug for VoiceMedia {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VoiceMedia")
            .field("file_info_present", &!self.file_info.trim().is_empty())
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
enum VoiceScene {
    C2c,
    Group,
}

impl VoiceScene {
    fn endpoint_prefix(self) -> &'static str {
        match self {
            Self::C2c => "users",
            Self::Group => "groups",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::C2c => "c2c",
            Self::Group => "group",
        }
    }
}

#[derive(Serialize)]
struct VoiceUploadPayload<'a> {
    file_type: u8,
    url: &'a str,
    srv_send_msg: bool,
}

#[derive(Deserialize)]
struct VoiceUploadResponse {
    #[serde(default)]
    file_info: String,
}

#[derive(Serialize)]
struct VoiceMessagePayload<'a> {
    msg_type: u8,
    media: &'a VoiceMedia,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

pub fn build_c2c_voice_payload(media: &VoiceMedia, msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(VoiceMessagePayload {
        msg_type: 7,
        media,
        msg_id,
        msg_seq,
    })
    .expect("C2C voice payload should serialize")
}

pub fn build_group_voice_payload(media: &VoiceMedia, msg_id: Option<&str>, msg_seq: u32) -> Value {
    build_c2c_voice_payload(media, msg_id, msg_seq)
}

impl QqApiClient {
    pub async fn send_c2c_voice_url(
        &self,
        user_openid: &str,
        msg_id: Option<&str>,
        audio_url: &str,
    ) -> SendResult {
        let authorization = self
            .auth
            .authorization_header()
            .await
            .map_err(ApiError::from)
            .map_err(|error| ApiError::VoiceUpload(Box::new(error)))?;
        let media = upload_voice_url(
            &self.client,
            &self.api_base,
            VoiceScene::C2c,
            user_openid,
            &authorization,
            audio_url,
        )
        .await
        .map_err(|error| ApiError::VoiceUpload(Box::new(error)))?;
        let payload = build_c2c_voice_payload(&media, msg_id, self.next_msg_seq());
        self.post_c2c_message(user_openid, msg_id, "voice", &payload)
            .await
            .map_err(|error| ApiError::VoiceSend(Box::new(error)))
    }

    pub async fn send_group_voice_url(
        &self,
        group_openid: &str,
        msg_id: Option<&str>,
        audio_url: &str,
    ) -> SendResult {
        let authorization = self
            .auth
            .authorization_header()
            .await
            .map_err(ApiError::from)
            .map_err(|error| ApiError::VoiceUpload(Box::new(error)))?;
        let media = upload_voice_url(
            &self.client,
            &self.api_base,
            VoiceScene::Group,
            group_openid,
            &authorization,
            audio_url,
        )
        .await
        .map_err(|error| ApiError::VoiceUpload(Box::new(error)))?;
        let payload = build_group_voice_payload(&media, msg_id, self.next_msg_seq());
        self.post_group_message(group_openid, msg_id, "voice", &payload)
            .await
            .map_err(|error| ApiError::VoiceSend(Box::new(error)))
    }
}

async fn upload_voice_url(
    client: &reqwest::Client,
    api_base: &str,
    scene: VoiceScene,
    peer_id: &str,
    authorization: &str,
    audio_url: &str,
) -> Result<VoiceMedia, ApiError> {
    validate_audio_url(audio_url)?;
    let endpoint = format!(
        "{}/v2/{}/{}/files",
        api_base.trim_end_matches('/'),
        scene.endpoint_prefix(),
        peer_id
    );
    let response = client
        .post(endpoint)
        .header("Authorization", authorization)
        .json(&VoiceUploadPayload {
            file_type: 3,
            url: audio_url,
            srv_send_msg: false,
        })
        .send()
        .await
        .map_err(ApiError::Http)?;
    let status = response.status();
    if !status.is_success() {
        let _ = response.bytes().await;
        warn!(scene = scene.label(), status = %status, "QQ voice URL upload returned non-success status");
        return Err(ApiError::Status {
            status,
            body: String::new(),
        });
    }
    let uploaded = response
        .json::<VoiceUploadResponse>()
        .await
        .map_err(ApiError::Http)?;
    let file_info = uploaded.file_info.trim();
    if file_info.is_empty() {
        return Err(ApiError::InvalidMedia(
            "voice upload response missing file_info",
        ));
    }
    debug!(scene = scene.label(), "QQ voice URL upload completed");
    info!(scene = scene.label(), "QQ voice upload succeeded");
    Ok(VoiceMedia {
        file_info: file_info.to_owned(),
    })
}

fn validate_audio_url(value: &str) -> Result<(), ApiError> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| ApiError::InvalidMedia("voice URL must be valid HTTP or HTTPS"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(ApiError::InvalidMedia(
            "voice URL must be valid HTTP or HTTPS",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        Json, Router,
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
        routing::post,
    };
    use serde_json::{Value, json};
    use tokio::net::TcpListener;

    use super::*;

    #[derive(Clone, Default)]
    struct MockState {
        uploads: Arc<Mutex<Vec<(String, Value)>>>,
    }

    async fn files_handler(
        State(state): State<MockState>,
        Path((scene, peer)): Path<(String, String)>,
        Json(payload): Json<Value>,
    ) -> impl IntoResponse {
        state
            .uploads
            .lock()
            .unwrap()
            .push((format!("{scene}/{peer}"), payload));
        (
            StatusCode::OK,
            Json(json!({"file_info": "voice-file-info"})),
        )
    }

    async fn mock_server() -> (String, MockState, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = MockState::default();
        let app = Router::new()
            .route("/v2/{scene}/{peer}/files", post(files_handler))
            .with_state(state.clone());
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), state, task)
    }

    #[tokio::test]
    async fn wav_url_is_uploaded_unchanged_for_c2c_and_group_without_file_data() {
        let (base, state, task) = mock_server().await;
        let signed_url = "https://audio.example.test/result.wav?Expires=1&Signature=secret";
        for (scene, peer, expected_path) in [
            (VoiceScene::C2c, "user-1", "users/user-1"),
            (VoiceScene::Group, "group-1", "groups/group-1"),
        ] {
            let media = upload_voice_url(
                &qq_maid_common::http_client::client(),
                &base,
                scene,
                peer,
                "QQBot test-token",
                signed_url,
            )
            .await
            .unwrap();
            assert_eq!(media.file_info, "voice-file-info");
            let uploads = state.uploads.lock().unwrap();
            let (path, payload) = uploads.last().unwrap();
            assert_eq!(path, expected_path);
            assert_eq!(payload["file_type"], 3);
            assert_eq!(payload["url"], signed_url);
            assert_eq!(payload["srv_send_msg"], false);
            assert!(payload.get("file_data").is_none());
            drop(uploads);
        }
        task.abort();
    }

    #[test]
    fn voice_message_payload_uses_uploaded_file_info_and_msg_type_seven() {
        let media = VoiceMedia {
            file_info: "voice-file-info".to_owned(),
        };
        for payload in [
            build_c2c_voice_payload(&media, Some("source-message"), 7),
            build_group_voice_payload(&media, Some("source-message"), 8),
        ] {
            assert_eq!(payload["msg_type"], 7);
            assert_eq!(payload["media"], json!({"file_info": "voice-file-info"}));
            assert_eq!(payload["msg_id"], "source-message");
        }
    }

    #[test]
    fn voice_url_rejects_non_http_schemes_before_upload() {
        for value in ["", "file:///tmp/voice.wav", "data:audio/wav;base64,AAAA"] {
            assert!(validate_audio_url(value).is_err());
        }
    }
}

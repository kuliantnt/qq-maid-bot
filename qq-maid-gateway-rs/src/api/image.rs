//! QQ 官方富媒体图片上传与发送。
//!
//! URL 继续走场景对应的 `/files` 接口；本地文件和 Base64 则使用官方分片协议：
//! `upload_prepare -> PUT presigned_url -> upload_part_finish -> /files merge`。
//! 上传接口不会隐式发送消息，成功后仍由普通消息接口携带 `media.file_info` 投递。

use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use md5::Md5;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tracing::{debug, info, warn};

use super::{ApiError, QqApiClient, SendResult};
use crate::media::{ImagePayload, build_c2c_image_payload, build_group_image_payload};

/// 当前仅发送图片，但让上传层显式保留 QQ 的 file_type，便于以后在完整验证后扩展。
#[derive(Debug, Clone, Copy)]
enum MediaFileType {
    Image = 1,
}

impl MediaFileType {
    fn code(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy)]
enum UploadScene {
    C2c,
    Group,
}

impl UploadScene {
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

#[derive(Debug, Serialize)]
struct UploadMergePayload<'a> {
    file_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
    srv_send_msg: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upload_id: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct UploadPreparePayload<'a> {
    file_type: u8,
    file_size: String,
    file_name: &'a str,
    md5: String,
    sha1: String,
    md5_10m: String,
}

#[derive(Debug, Serialize)]
struct UploadPartFinishPayload<'a> {
    upload_id: &'a str,
    part_index: u32,
    block_size: String,
    md5: String,
}

#[derive(Debug, Deserialize)]
struct UploadMediaResponse {
    #[serde(default)]
    file_info: String,
    #[serde(default)]
    file_uuid: Option<String>,
    #[serde(default)]
    ttl: Option<u64>,
    #[serde(default)]
    raw_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UploadPrepareResponse {
    #[serde(default)]
    upload_id: String,
    #[serde(default)]
    block_size: String,
    #[serde(default)]
    parts: Vec<UploadPart>,
    #[serde(default)]
    upload_config: Option<UploadConfig>,
}

#[derive(Debug, Deserialize)]
struct UploadPart {
    index: u32,
    presigned_url: String,
    #[serde(default)]
    block_size: String,
}

#[derive(Debug, Deserialize)]
struct UploadConfig {
    #[serde(default)]
    concurrency: Option<u32>,
    #[serde(default)]
    retry_timeout: Option<u64>,
    #[serde(default)]
    retry_delay: Option<u64>,
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
        self.upload_image(UploadScene::C2c, user_openid, image)
            .await
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
        self.upload_image(UploadScene::Group, group_openid, image)
            .await
    }

    async fn upload_image(
        &self,
        scene: UploadScene,
        peer_id: &str,
        image: &ImagePayload,
    ) -> Result<ImagePayload, ApiError> {
        let url = image
            .url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let authorization = self.auth.authorization_header().await?;
        if let Some(url) = url {
            return upload_url_media(
                &self.client,
                &self.api_base,
                scene,
                peer_id,
                &authorization,
                url,
            )
            .await;
        }

        let bytes = image_bytes(image)?;
        if bytes.is_empty() {
            return Err(ApiError::InvalidMedia(
                "local image or base64 data is empty",
            ));
        }
        let file_name = image_file_name(image);
        upload_bytes_media(
            &self.client,
            &self.api_base,
            scene,
            peer_id,
            &authorization,
            &bytes,
            &file_name,
        )
        .await
    }
}

fn image_bytes(image: &ImagePayload) -> Result<Vec<u8>, ApiError> {
    if let Some(value) = image
        .data_base64
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let encoded = value
            .rsplit_once(',')
            .map(|(_, data)| data)
            .unwrap_or(value);
        return STANDARD
            .decode(encoded)
            .map_err(|_| ApiError::InvalidMedia("invalid image base64 data"));
    }
    let Some(path) = image
        .local_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(ApiError::InvalidMedia(
            "missing file_info, URL, base64 data, or local image path",
        ));
    };
    std::fs::read(path).map_err(|_| ApiError::InvalidMedia("failed to read local image file"))
}

fn image_file_name(image: &ImagePayload) -> String {
    image
        .local_path
        .as_deref()
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| "image.bin".to_owned())
}

async fn upload_url_media(
    client: &reqwest::Client,
    api_base: &str,
    scene: UploadScene,
    peer_id: &str,
    authorization: &str,
    url: &str,
) -> Result<ImagePayload, ApiError> {
    let endpoint = upload_endpoint(api_base, scene, peer_id, "files");
    let payload = UploadMergePayload {
        file_type: MediaFileType::Image.code(),
        url: Some(url),
        srv_send_msg: false,
        file_name: None,
        upload_id: None,
    };
    let uploaded = post_upload_json(client, endpoint, authorization, &payload, scene).await?;
    uploaded_image_payload(uploaded, scene)
}

async fn upload_bytes_media(
    client: &reqwest::Client,
    api_base: &str,
    scene: UploadScene,
    peer_id: &str,
    authorization: &str,
    bytes: &[u8],
    file_name: &str,
) -> Result<ImagePayload, ApiError> {
    let prepare_endpoint = upload_endpoint(api_base, scene, peer_id, "upload_prepare");
    let prepare = UploadPreparePayload {
        file_type: MediaFileType::Image.code(),
        file_size: bytes.len().to_string(),
        file_name,
        md5: hex_digest::<Md5>(bytes),
        sha1: hex_digest::<Sha1>(bytes),
        md5_10m: hex_digest::<Md5>(&bytes[..bytes.len().min(10_002_432)]),
    };
    let prepared: UploadPrepareResponse =
        post_upload_json(client, prepare_endpoint, authorization, &prepare, scene).await?;
    let upload_id = prepared.upload_id.trim();
    if upload_id.is_empty() || prepared.parts.is_empty() {
        return Err(ApiError::InvalidMedia(
            "upload prepare response missing upload_id or parts",
        ));
    }
    if let Some(config) = prepared.upload_config.as_ref() {
        // 当前按服务端给出的 parts 顺序串行上传；配置仅作脱敏诊断，避免把预签名 URL 写入日志。
        debug!(
            scene = scene.label(),
            concurrency = config.concurrency.unwrap_or(1),
            retry_timeout_seconds = config.retry_timeout.unwrap_or_default(),
            retry_delay_seconds = config.retry_delay.unwrap_or_default(),
            "QQ media upload prepared"
        );
    }

    let default_block_size = parse_block_size(&prepared.block_size)?;
    let mut parts = prepared.parts;
    parts.sort_by_key(|part| part.index);
    let mut offset = 0usize;
    for part in parts {
        let part_size = if part.block_size.trim().is_empty() {
            default_block_size
        } else {
            parse_block_size(&part.block_size)?
        };
        if offset >= bytes.len() || part_size == 0 {
            return Err(ApiError::InvalidMedia("invalid upload part layout"));
        }
        let end = offset.saturating_add(part_size).min(bytes.len());
        let block = &bytes[offset..end];
        put_upload_part(client, &part.presigned_url, block).await?;
        let finish_endpoint = upload_endpoint(api_base, scene, peer_id, "upload_part_finish");
        let finish = UploadPartFinishPayload {
            upload_id,
            part_index: part.index,
            block_size: block.len().to_string(),
            md5: hex_digest::<Md5>(block),
        };
        let _: serde_json::Value =
            post_upload_json(client, finish_endpoint, authorization, &finish, scene).await?;
        offset = end;
    }
    if offset != bytes.len() {
        return Err(ApiError::InvalidMedia(
            "upload prepare parts do not cover the complete image",
        ));
    }

    let merge_endpoint = upload_endpoint(api_base, scene, peer_id, "files");
    let merge = UploadMergePayload {
        file_type: MediaFileType::Image.code(),
        url: None,
        srv_send_msg: false,
        file_name: Some(file_name),
        upload_id: Some(upload_id),
    };
    let uploaded = post_upload_json(client, merge_endpoint, authorization, &merge, scene).await?;
    uploaded_image_payload(uploaded, scene)
}

fn upload_endpoint(api_base: &str, scene: UploadScene, peer_id: &str, action: &str) -> String {
    format!(
        "{}/v2/{}/{}/{}",
        api_base.trim_end_matches('/'),
        scene.endpoint_prefix(),
        peer_id,
        action
    )
}

async fn post_upload_json<T: Serialize, R: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    endpoint: String,
    authorization: &str,
    payload: &T,
    scene: UploadScene,
) -> Result<R, ApiError> {
    let response = client
        .post(endpoint)
        .header("Authorization", authorization)
        .json(payload)
        .send()
        .await
        .map_err(ApiError::Http)?;
    let status = response.status();
    if !status.is_success() {
        let _ = response.text().await;
        warn!(scene = scene.label(), status = %status, "QQ media upload API returned non-success status");
        // 上传服务可能在错误体回显临时地址或预签名参数；错误继续向上传层传播时只保留状态码。
        return Err(ApiError::Status {
            status,
            body: String::new(),
        });
    }
    response.json().await.map_err(ApiError::Http)
}

async fn put_upload_part(
    client: &reqwest::Client,
    presigned_url: &str,
    bytes: &[u8],
) -> Result<(), ApiError> {
    let response = client
        .put(presigned_url)
        .body(bytes.to_vec())
        .send()
        .await
        .map_err(ApiError::Http)?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let _ = response.text().await;
    // 预签名 URL 和响应体都可能带临时凭证，诊断只记录阶段和状态。
    warn!(status = %status, "QQ media upload part PUT returned non-success status");
    Err(ApiError::Status {
        status,
        body: String::new(),
    })
}

fn uploaded_image_payload(
    uploaded: UploadMediaResponse,
    scene: UploadScene,
) -> Result<ImagePayload, ApiError> {
    let file_info = uploaded.file_info.trim();
    if file_info.is_empty() {
        return Err(ApiError::InvalidMedia("upload response missing file_info"));
    }
    // `file_info` 只交给当前发送调用；这里不缓存。ttl=0 表示平台允许长期使用，其余值
    // 只能用于未来受 ttl 约束的缓存实现。raw_url 是预签名 URL，绝不写日志或返回上层。
    debug!(
        scene = scene.label(),
        has_file_uuid = uploaded.file_uuid.is_some(),
        ttl_seconds = uploaded.ttl.unwrap_or_default(),
        has_raw_url = uploaded.raw_url.is_some(),
        "QQ media upload completed"
    );
    info!(scene = scene.label(), "QQ image upload succeeded");
    Ok(ImagePayload::new(file_info))
}

fn parse_block_size(value: &str) -> Result<usize, ApiError> {
    value
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|size| *size > 0)
        .ok_or(ApiError::InvalidMedia("invalid upload block_size"))
}

fn hex_digest<D: Digest + Default>(bytes: &[u8]) -> String {
    let mut digest = D::default();
    digest.update(bytes);
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use axum::{
        Json, Router,
        body::Bytes,
        extract::State,
        http::StatusCode,
        response::IntoResponse,
        routing::{post, put},
    };
    use serde_json::{Value, json};
    use tokio::net::TcpListener;

    #[derive(Clone)]
    struct UploadTestState {
        base_url: String,
        failure_phase: Arc<Mutex<Option<&'static str>>>,
        file_payloads: Arc<Mutex<Vec<Value>>>,
        put_bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    async fn files_handler(
        State(state): State<UploadTestState>,
        Json(payload): Json<Value>,
    ) -> impl IntoResponse {
        let is_merge = payload.get("upload_id").is_some();
        state.file_payloads.lock().unwrap().push(payload);
        if is_merge && *state.failure_phase.lock().unwrap() == Some("merge") {
            return (
                StatusCode::BAD_GATEWAY,
                "merge failed https://temporary.test/?auth_token=secret",
            )
                .into_response();
        }
        Json(json!({
            "file_info": "file-info",
            "file_uuid": "file-uuid",
            "ttl": 1,
            "raw_url": format!("{}/temporary?auth_token=secret", state.base_url)
        }))
        .into_response()
    }

    async fn prepare_handler(State(state): State<UploadTestState>) -> impl IntoResponse {
        if *state.failure_phase.lock().unwrap() == Some("prepare") {
            return (
                StatusCode::BAD_GATEWAY,
                "prepare failed https://temporary.test/?auth_token=secret",
            )
                .into_response();
        }
        Json(json!({
            "upload_id": "upload-1",
            "block_size": "5",
            "parts": [{"index": 0, "block_size": "5", "presigned_url": format!("{}/presigned/0?signature=secret", state.base_url)}],
            "upload_config": {"concurrency": 1, "retry_timeout": 1, "retry_delay": 0}
        }))
        .into_response()
    }

    async fn part_handler(State(state): State<UploadTestState>) -> impl IntoResponse {
        if *state.failure_phase.lock().unwrap() == Some("part_finish") {
            return (
                StatusCode::BAD_GATEWAY,
                "part finish failed https://temporary.test/?auth_token=secret",
            )
                .into_response();
        }
        Json(json!({})).into_response()
    }

    async fn put_handler(State(state): State<UploadTestState>, body: Bytes) -> impl IntoResponse {
        state.put_bodies.lock().unwrap().push(body.to_vec());
        if *state.failure_phase.lock().unwrap() == Some("put") {
            return (
                StatusCode::BAD_GATEWAY,
                "put failed https://temporary.test/?auth_token=secret",
            )
                .into_response();
        }
        StatusCode::OK.into_response()
    }

    async fn upload_test_server() -> (UploadTestState, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let state = UploadTestState {
            base_url,
            failure_phase: Arc::new(Mutex::new(None)),
            file_payloads: Arc::new(Mutex::new(Vec::new())),
            put_bodies: Arc::new(Mutex::new(Vec::new())),
        };
        let app = Router::new()
            .route("/v2/users/user/files", post(files_handler))
            .route("/v2/users/user/upload_prepare", post(prepare_handler))
            .route("/v2/users/user/upload_part_finish", post(part_handler))
            .route("/presigned/0", put(put_handler))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (state, task)
    }

    #[test]
    fn url_upload_payload_uses_only_documented_fields() {
        let payload = serde_json::to_value(UploadMergePayload {
            file_type: 1,
            url: Some("https://example.test/image.png"),
            srv_send_msg: false,
            file_name: None,
            upload_id: None,
        })
        .unwrap();

        assert_eq!(payload["file_type"], 1);
        assert_eq!(payload["url"], "https://example.test/image.png");
        assert_eq!(payload["srv_send_msg"], false);
        assert!(payload.get("file_data").is_none());
    }

    #[test]
    fn local_path_and_base64_resolve_to_upload_bytes() {
        let path = std::env::temp_dir().join(format!("qq-maid-upload-{}.bin", fastrand::u64(..)));
        std::fs::write(&path, b"local-image").unwrap();
        let local = ImagePayload::from_local_path(path.to_string_lossy());
        assert_eq!(image_bytes(&local).unwrap(), b"local-image");
        let _ = std::fs::remove_file(path);

        let base64 = ImagePayload::from_base64("aGVsbG8=");
        assert_eq!(image_bytes(&base64).unwrap(), b"hello");
    }

    #[test]
    fn c2c_and_group_upload_endpoints_are_never_interchanged() {
        assert_eq!(
            upload_endpoint(
                "https://api.example.test",
                UploadScene::C2c,
                "user",
                "files"
            ),
            "https://api.example.test/v2/users/user/files"
        );
        assert_eq!(
            upload_endpoint(
                "https://api.example.test",
                UploadScene::Group,
                "group",
                "files"
            ),
            "https://api.example.test/v2/groups/group/files"
        );
    }

    #[tokio::test]
    async fn url_upload_uses_files_endpoint_and_never_sends_file_data() {
        let (state, task) = upload_test_server().await;
        let image = upload_url_media(
            &qq_maid_common::http_client::client(),
            &state.base_url,
            UploadScene::C2c,
            "user",
            "QQBot test",
            "https://example.test/image.png",
        )
        .await
        .unwrap();
        task.abort();

        assert_eq!(image.file_info.as_deref(), Some("file-info"));
        let payloads = state.file_payloads.lock().unwrap();
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0]["url"], "https://example.test/image.png");
        assert!(payloads[0].get("file_data").is_none());
        assert!(payloads[0].get("upload_id").is_none());
    }

    #[tokio::test]
    async fn base64_and_local_images_use_prepare_put_finish_and_merge_without_cache() {
        let (state, task) = upload_test_server().await;
        let client = qq_maid_common::http_client::client();
        let base64 = ImagePayload::from_base64("aGVsbG8=");
        let bytes = image_bytes(&base64).unwrap();
        upload_bytes_media(
            &client,
            &state.base_url,
            UploadScene::C2c,
            "user",
            "QQBot test",
            &bytes,
            "image.bin",
        )
        .await
        .unwrap();

        let path = std::env::temp_dir().join(format!("qq-maid-upload-{}.bin", fastrand::u64(..)));
        std::fs::write(&path, b"local").unwrap();
        let local = ImagePayload::from_local_path(path.to_string_lossy());
        let local_bytes = image_bytes(&local).unwrap();
        upload_bytes_media(
            &client,
            &state.base_url,
            UploadScene::C2c,
            "user",
            "QQBot test",
            &local_bytes,
            "local.bin",
        )
        .await
        .unwrap();
        let _ = std::fs::remove_file(path);
        task.abort();

        assert_eq!(
            state.put_bodies.lock().unwrap().as_slice(),
            &[b"hello".to_vec(), b"local".to_vec()]
        );
        let payloads = state.file_payloads.lock().unwrap();
        assert_eq!(
            payloads.len(),
            2,
            "ttl=1 response must not create an unbounded file_info cache"
        );
        assert!(
            payloads
                .iter()
                .all(|payload| payload.get("upload_id").is_some())
        );
        assert!(
            payloads
                .iter()
                .all(|payload| payload.get("file_data").is_none())
        );
    }

    #[tokio::test]
    async fn prepare_put_part_finish_and_merge_failures_are_propagated() {
        for phase in ["prepare", "put", "part_finish", "merge"] {
            let (state, task) = upload_test_server().await;
            *state.failure_phase.lock().unwrap() = Some(phase);
            let error = upload_bytes_media(
                &qq_maid_common::http_client::client(),
                &state.base_url,
                UploadScene::C2c,
                "user",
                "QQBot test",
                b"hello",
                "image.bin",
            )
            .await
            .unwrap_err();
            task.abort();
            assert!(matches!(error, ApiError::Status { .. }), "phase={phase}");
            let summary = error.log_summary();
            assert!(!summary.contains("auth_token"), "phase={phase}");
            assert!(!summary.contains("temporary.test"), "phase={phase}");
        }
    }
}

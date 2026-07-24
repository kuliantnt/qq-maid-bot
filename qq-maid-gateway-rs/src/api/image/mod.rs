//! QQ 官方富媒体图片上传与发送。
//!
//! URL 继续走场景对应的 `/files` 接口；本地文件和 Base64 则使用官方分片协议：
//! `upload_prepare -> PUT presigned_url -> upload_part_finish -> /files merge`。
//! 上传接口不会隐式发送消息，成功后仍由普通消息接口携带 `media.file_info` 投递。

use std::{collections::HashSet, path::Path};

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

/// QQ 当前图片上传只接受 PNG 与 JPEG；格式同时决定传给上传接口的安全文件名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageFormat {
    Png,
    Jpeg,
}

impl ImageFormat {
    fn from_data_url_mime(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "image/png" => Some(Self::Png),
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            _ => None,
        }
    }

    fn from_file_extension(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            _ => None,
        }
    }

    fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else {
            None
        }
    }

    fn safe_file_name(self) -> &'static str {
        match self {
            Self::Png => "image.png",
            Self::Jpeg => "image.jpg",
        }
    }
}

struct ResolvedImageUpload {
    bytes: Vec<u8>,
    file_name: String,
}

struct UploadPartLayout {
    index: u32,
    presigned_url: String,
    offset: usize,
    end: usize,
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
struct UploadUrlPayload<'a> {
    file_type: u8,
    url: &'a str,
    srv_send_msg: bool,
}

#[derive(Debug, Serialize)]
struct UploadCompletePayload<'a> {
    upload_id: &'a str,
}

#[derive(Debug, Serialize)]
struct UploadPreparePayload<'a> {
    file_type: u8,
    file_size: u64,
    file_name: &'a str,
    md5: String,
    sha1: String,
    md5_10m: String,
}

#[derive(Debug, Serialize)]
struct UploadPartFinishPayload<'a> {
    upload_id: &'a str,
    part_index: u32,
    block_size: u64,
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
    block_size: u64,
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
    block_size: u64,
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

        let upload = resolve_image_upload(image)?;
        if upload.bytes.is_empty() {
            return Err(ApiError::InvalidMedia(
                "local image or base64 data is empty",
            ));
        }
        upload_bytes_media(
            &self.client,
            &self.api_base,
            scene,
            peer_id,
            &authorization,
            &upload.bytes,
            &upload.file_name,
        )
        .await
    }
}

fn resolve_image_upload(image: &ImagePayload) -> Result<ResolvedImageUpload, ApiError> {
    if let Some(value) = image
        .data_base64
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return resolve_base64_image(value);
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
    let bytes = std::fs::read(path)
        .map_err(|_| ApiError::InvalidMedia("failed to read local image file"))?;
    let format = ImageFormat::detect(&bytes).ok_or(ApiError::InvalidMedia(
        "local image file is not a supported PNG or JPEG",
    ))?;
    let file_name = safe_local_image_name(path, format)?;
    Ok(ResolvedImageUpload { bytes, file_name })
}

fn resolve_base64_image(value: &str) -> Result<ResolvedImageUpload, ApiError> {
    let (encoded, declared_format) = if value
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
    {
        let (header, encoded) = value
            .split_once(',')
            .ok_or(ApiError::InvalidMedia("invalid image data URL"))?;
        let metadata = &header[5..];
        let mut sections = metadata.split(';');
        let mime = sections
            .next()
            .ok_or(ApiError::InvalidMedia("invalid image data URL"))?;
        let format = ImageFormat::from_data_url_mime(mime).ok_or(ApiError::InvalidMedia(
            "unsupported image data URL MIME type",
        ))?;
        if !sections.any(|section| section.trim().eq_ignore_ascii_case("base64")) {
            return Err(ApiError::InvalidMedia(
                "image data URL is not base64 encoded",
            ));
        }
        (encoded, Some(format))
    } else {
        (value, None)
    };
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::InvalidMedia("invalid image base64 data"))?;
    let detected_format = ImageFormat::detect(&bytes).ok_or(ApiError::InvalidMedia(
        "base64 data is not a supported PNG or JPEG",
    ))?;
    if declared_format.is_some_and(|format| format != detected_format) {
        return Err(ApiError::InvalidMedia(
            "image data URL MIME type does not match image data",
        ));
    }
    Ok(ResolvedImageUpload {
        bytes,
        file_name: detected_format.safe_file_name().to_owned(),
    })
}

fn safe_local_image_name(path: &str, format: ImageFormat) -> Result<String, ApiError> {
    // Windows 路径可能通过跨平台调用传入；只传 basename，避免把本地目录暴露给 QQ 上传接口。
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit(['/', '\\']).next())
        .filter(|name| !name.trim().is_empty())
        .ok_or(ApiError::InvalidMedia(
            "local image file has no valid basename",
        ))?;
    let extension = Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .and_then(ImageFormat::from_file_extension)
        .ok_or(ApiError::InvalidMedia(
            "local image file has unsupported extension",
        ))?;
    if extension != format {
        return Err(ApiError::InvalidMedia(
            "local image file extension does not match image data",
        ));
    }
    Ok(file_name.to_owned())
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
    let payload = UploadUrlPayload {
        file_type: MediaFileType::Image.code(),
        url,
        srv_send_msg: false,
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
    let file_size = u64::try_from(bytes.len())
        .map_err(|_| ApiError::InvalidMedia("image is too large to upload"))?;
    let prepare = UploadPreparePayload {
        file_type: MediaFileType::Image.code(),
        file_size,
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
        // 当前按校验后的 1-based index 顺序串行上传；配置仅作脱敏诊断，避免把预签名 URL 写入日志。
        debug!(
            scene = scene.label(),
            concurrency = config.concurrency.unwrap_or(1),
            retry_timeout_seconds = config.retry_timeout.unwrap_or_default(),
            retry_delay_seconds = config.retry_delay.unwrap_or_default(),
            "QQ media upload prepared"
        );
    }

    let parts = resolve_upload_part_layout(prepared.parts, prepared.block_size, bytes.len())?;
    for part in parts {
        let block = &bytes[part.offset..part.end];
        put_upload_part(client, &part.presigned_url, block).await?;
        let finish_endpoint = upload_endpoint(api_base, scene, peer_id, "upload_part_finish");
        let finish = UploadPartFinishPayload {
            upload_id,
            part_index: part.index,
            block_size: u64::try_from(block.len())
                .map_err(|_| ApiError::InvalidMedia("upload part is too large"))?,
            md5: hex_digest::<Md5>(block),
        };
        let _: serde_json::Value =
            post_upload_json(client, finish_endpoint, authorization, &finish, scene).await?;
    }

    let merge_endpoint = upload_endpoint(api_base, scene, peer_id, "files");
    // 官方完成上传请求只接受 upload_id，不复用 URL 上传字段。
    let merge = UploadCompletePayload { upload_id };
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

fn resolve_upload_part_layout(
    mut parts: Vec<UploadPart>,
    stride: u64,
    file_size: usize,
) -> Result<Vec<UploadPartLayout>, ApiError> {
    if file_size == 0 {
        return Err(ApiError::InvalidMedia("cannot upload an empty image"));
    }
    if stride == 0 {
        return Err(ApiError::InvalidMedia("invalid upload block_size"));
    }

    let mut indexes = HashSet::with_capacity(parts.len());
    if parts.iter().any(|part| part.index == 0) {
        return Err(ApiError::InvalidMedia(
            "upload prepare response has invalid part index",
        ));
    }
    if parts.iter().any(|part| !indexes.insert(part.index)) {
        return Err(ApiError::InvalidMedia(
            "upload prepare response has duplicate part index",
        ));
    }
    parts.sort_by_key(|part| part.index);
    if parts
        .iter()
        .enumerate()
        .any(|(expected, part)| part.index != expected as u32 + 1)
    {
        return Err(ApiError::InvalidMedia(
            "upload prepare response has missing part index",
        ));
    }

    let file_size = u64::try_from(file_size)
        .map_err(|_| ApiError::InvalidMedia("image is too large to upload"))?;
    let mut covered_end = 0u64;
    let mut layout = Vec::with_capacity(parts.len());
    for part in parts {
        if part.presigned_url.trim().is_empty() {
            return Err(ApiError::InvalidMedia(
                "upload prepare response has empty part URL",
            ));
        }
        // QQ 的 part.index 从 1 开始，offset 只由顶层 block_size 决定；part.block_size
        // 只描述本片实际长度，不能参与后续分片的偏移累计。
        let offset = u64::from(part.index - 1)
            .checked_mul(stride)
            .ok_or(ApiError::InvalidMedia("invalid upload part layout"))?;
        let part_size = if part.block_size > 0 {
            part.block_size
        } else {
            stride
        };
        let end = offset
            .checked_add(part_size)
            .ok_or(ApiError::InvalidMedia("invalid upload part layout"))?;
        if offset != covered_end || offset >= file_size || end > file_size {
            return Err(ApiError::InvalidMedia("invalid upload part layout"));
        }
        layout.push(UploadPartLayout {
            index: part.index,
            presigned_url: part.presigned_url,
            offset: usize::try_from(offset)
                .map_err(|_| ApiError::InvalidMedia("invalid upload part layout"))?,
            end: usize::try_from(end)
                .map_err(|_| ApiError::InvalidMedia("invalid upload part layout"))?,
        });
        covered_end = end;
    }
    if covered_end != file_size {
        return Err(ApiError::InvalidMedia(
            "upload prepare parts do not cover the complete image",
        ));
    }
    Ok(layout)
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
mod tests;

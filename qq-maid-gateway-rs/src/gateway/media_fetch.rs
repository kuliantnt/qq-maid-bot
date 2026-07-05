//! QQ 官方入站媒体取回。
//!
//! 这里只处理平台事件中明确给出的远端附件 URL，不访问 `file://` 或用户本机路径。
//! 下载结果写入本地媒体缓存后，通过 `MessageMedia.local_path` 交给 LLM provider 读取。

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures_util::future::join_all;
use qq_maid_common::input_part::{MediaStatus, MessageInputPart, MessageMedia};
use tracing::{debug, warn};

use super::event::Attachment;

static MEDIA_FILE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub(crate) struct MediaFetchContext {
    pub(crate) platform: &'static str,
    pub(crate) app_id: String,
    pub(crate) peer_id: String,
    pub(crate) root_dir: PathBuf,
    pub(crate) timeout: Duration,
}

pub(crate) async fn fetch_qq_official_image_attachments(
    client: &reqwest::Client,
    context: &MediaFetchContext,
    message_id: &str,
    input_parts: &mut [MessageInputPart],
    attachments: &[Attachment],
) {
    if attachments.is_empty() {
        mark_unreadable_image_parts(input_parts);
        return;
    }

    let fetches =
        attachments
            .iter()
            .filter(|attachment| looks_like_image_attachment(attachment))
            .cloned()
            .map(|attachment| {
                let client = client.clone();
                let context = context.clone();
                async move {
                    let Some(url) = attachment.url.as_deref() else {
                        return AttachmentFetchOutcome {
                            attachment,
                            result: AttachmentFetchResult::MissingReadableUrl,
                        };
                    };
                    let Some(normalized_url) = normalize_download_url(url) else {
                        return AttachmentFetchOutcome {
                            attachment,
                            result: AttachmentFetchResult::MissingReadableUrl,
                        };
                    };
                    let url_scheme = normalized_url_scheme(&normalized_url);
                    let result =
                        match download_attachment(&client, &context, &attachment, &normalized_url)
                            .await
                        {
                            Ok(downloaded) => AttachmentFetchResult::Downloaded {
                                downloaded,
                                url_scheme,
                            },
                            Err(error) => AttachmentFetchResult::Failed { error, url_scheme },
                        };
                    AttachmentFetchOutcome { attachment, result }
                }
            })
            .collect::<Vec<_>>();

    if fetches.is_empty() {
        mark_unreadable_image_parts(input_parts);
        return;
    }

    for outcome in join_all(fetches).await {
        match outcome.result {
            AttachmentFetchResult::MissingReadableUrl => {
                update_matching_image_part(input_parts, &outcome.attachment, |media| {
                    media.status = MediaStatus::MissingReadableUrl;
                });
            }
            AttachmentFetchResult::Downloaded {
                downloaded,
                url_scheme,
            } => {
                update_matching_image_part(input_parts, &outcome.attachment, |media| {
                    media.local_path = Some(downloaded.local_path.to_string_lossy().to_string());
                    if media.mime_type.as_deref().is_none_or(str::is_empty) {
                        media.mime_type = downloaded.mime_type.clone();
                    }
                    media.status = MediaStatus::Available;
                });
                debug!(
                    message_id,
                    platform = context.platform,
                    media_status = "available",
                    image_url_scheme = url_scheme,
                    "QQ official image attachment downloaded"
                );
            }
            AttachmentFetchResult::Failed { error, url_scheme } => {
                update_matching_image_part(input_parts, &outcome.attachment, |media| {
                    media.status = MediaStatus::DownloadFailed;
                });
                warn!(
                    message_id,
                    platform = context.platform,
                    media_status = "download_failed",
                    image_url_scheme = url_scheme,
                    error = %error.safe_summary(),
                    "QQ official image attachment download failed"
                );
            }
        }
    }

    mark_unreadable_image_parts(input_parts);
}

pub(crate) fn normalize_download_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(value.to_owned());
    }
    if value.starts_with("//") {
        return Some(format!("https:{value}"));
    }
    None
}

#[derive(Debug)]
struct DownloadedMedia {
    local_path: PathBuf,
    mime_type: Option<String>,
}

#[derive(Debug)]
struct AttachmentFetchOutcome {
    attachment: Attachment,
    result: AttachmentFetchResult,
}

#[derive(Debug)]
enum AttachmentFetchResult {
    MissingReadableUrl,
    Downloaded {
        downloaded: DownloadedMedia,
        url_scheme: &'static str,
    },
    Failed {
        error: MediaDownloadError,
        url_scheme: &'static str,
    },
}

#[derive(Debug)]
enum MediaDownloadError {
    Http(reqwest::Error),
    Status(reqwest::StatusCode),
    Io,
}

impl MediaDownloadError {
    fn safe_summary(&self) -> String {
        match self {
            Self::Http(err) if err.is_timeout() => "timeout".to_owned(),
            Self::Http(_) => "http_error".to_owned(),
            Self::Status(status) => format!("http_status_{}", status.as_u16()),
            Self::Io => "io_error".to_owned(),
        }
    }
}

async fn download_attachment(
    client: &reqwest::Client,
    context: &MediaFetchContext,
    attachment: &Attachment,
    url: &str,
) -> Result<DownloadedMedia, MediaDownloadError> {
    let response = client
        .get(url)
        .timeout(context.timeout)
        .send()
        .await
        .map_err(MediaDownloadError::Http)?;
    if !response.status().is_success() {
        return Err(MediaDownloadError::Status(response.status()));
    }
    let response_mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(clean_mime_type);
    let bytes = response.bytes().await.map_err(MediaDownloadError::Http)?;
    let dir = media_dir(context);
    std::fs::create_dir_all(&dir).map_err(|_| MediaDownloadError::Io)?;
    let local_path = dir.join(unique_filename(attachment, response_mime.as_deref()));
    std::fs::write(&local_path, &bytes).map_err(|_| MediaDownloadError::Io)?;
    Ok(DownloadedMedia {
        local_path,
        mime_type: attachment.content_type.clone().or(response_mime),
    })
}

fn media_dir(context: &MediaFetchContext) -> PathBuf {
    context
        .root_dir
        .join(safe_path_segment(context.platform))
        .join(safe_path_segment(&context.app_id))
        .join(safe_path_segment(&context.peer_id))
}

fn unique_filename(attachment: &Attachment, response_mime: Option<&str>) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let counter = MEDIA_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let fallback = filename_for_mime(
        attachment.content_type.as_deref().or(response_mime),
        attachment.filename.as_deref(),
    );
    format!("{timestamp}-{counter}-{}", safe_filename(&fallback))
}

fn filename_for_mime(content_type: Option<&str>, filename: Option<&str>) -> String {
    let filename = filename
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("image");
    if Path::new(filename).extension().is_some() {
        return filename.to_owned();
    }
    let extension = match content_type
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "bin",
    };
    format!("{filename}.{extension}")
}

fn safe_filename(value: &str) -> String {
    let candidate = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("image")
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let candidate = candidate.trim_matches(['.', '_', '-']);
    if candidate.is_empty() {
        "image.bin".to_owned()
    } else {
        candidate.to_owned()
    }
}

fn safe_path_segment(value: &str) -> String {
    let candidate = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if candidate.trim_matches('_').is_empty() {
        "-".to_owned()
    } else {
        candidate
    }
}

fn clean_mime_type(value: &str) -> Option<String> {
    value
        .split(';')
        .next()
        .map(str::trim)
        .filter(|value| value.starts_with("image/"))
        .map(str::to_owned)
}

fn normalized_url_scheme(url: &str) -> &'static str {
    if url.starts_with("https://") {
        "https"
    } else if url.starts_with("http://") {
        "http"
    } else {
        "other"
    }
}

fn looks_like_image_attachment(attachment: &Attachment) -> bool {
    let content_type = attachment
        .content_type
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if content_type.starts_with("image/") || content_type == "image" {
        return true;
    }
    attachment
        .filename
        .as_deref()
        .map(|filename| filename.trim().to_ascii_lowercase())
        .and_then(|filename| filename.rsplit('.').next().map(str::to_owned))
        .is_some_and(|extension| {
            matches!(
                extension.as_str(),
                "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp"
            )
        })
}

fn update_matching_image_part(
    parts: &mut [MessageInputPart],
    attachment: &Attachment,
    mut update: impl FnMut(&mut MessageMedia),
) {
    let mut updated = false;
    for part in parts.iter_mut() {
        let MessageInputPart::Image { media } = part else {
            continue;
        };
        if media_matches_attachment(media, attachment) {
            update(media);
            updated = true;
        }
    }
    if !updated {
        for part in parts.iter_mut() {
            let MessageInputPart::Image { media } = part else {
                continue;
            };
            if media.local_path.is_none() && media.status != MediaStatus::Available {
                update(media);
                break;
            }
        }
    }
}

fn media_matches_attachment(media: &MessageMedia, attachment: &Attachment) -> bool {
    attachment
        .url
        .as_deref()
        .zip(media.url.as_deref())
        .is_some_and(|(left, right)| left.trim() == right.trim())
        || attachment
            .filename
            .as_deref()
            .zip(media.filename.as_deref())
            .is_some_and(|(left, right)| left.trim() == right.trim())
}

fn mark_unreadable_image_parts(parts: &mut [MessageInputPart]) {
    for part in parts {
        let MessageInputPart::Image { media } = part else {
            continue;
        };
        if matches!(
            media.status,
            MediaStatus::Available | MediaStatus::MissingReadableUrl
        ) {
            media.status = media.inferred_readability_status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Bytes, routing::get};
    use qq_maid_common::input_part::MessageInputPart;
    use std::time::Instant;
    use tokio::net::TcpListener;

    #[test]
    fn normalize_protocol_relative_url_to_https() {
        assert_eq!(
            normalize_download_url("//multimedia.nt.qq.com.cn/test.jpg").as_deref(),
            Some("https://multimedia.nt.qq.com.cn/test.jpg")
        );
        assert_eq!(
            normalize_download_url("https://example.test/a.jpg").as_deref(),
            Some("https://example.test/a.jpg")
        );
        assert!(normalize_download_url("file://C:\\Users\\a.jpg").is_none());
        assert!(normalize_download_url("C:\\Users\\a.jpg").is_none());
    }

    #[tokio::test]
    async fn downloads_http_image_attachment_to_local_path() {
        let app = Router::new().route(
            "/a.jpg",
            get(|| async {
                (
                    [(reqwest::header::CONTENT_TYPE.as_str(), "image/jpeg")],
                    Bytes::from_static(b"fake-jpeg"),
                )
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let root_dir = std::env::temp_dir().join(format!(
            "qq-maid-media-fetch-test-{}",
            MEDIA_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let attachment = Attachment {
            content_type: Some("image/jpeg".to_owned()),
            filename: Some("a.jpg".to_owned()),
            url: Some(format!("http://{addr}/a.jpg")),
            size_bytes: None,
            media_id: None,
            file_id: None,
            attachment_id: None,
        };
        let mut parts = vec![
            MessageInputPart::text("先看这张"),
            MessageInputPart::image(MessageMedia {
                mime_type: attachment.content_type.clone(),
                filename: attachment.filename.clone(),
                url: attachment.url.clone(),
                status: MediaStatus::MissingReadableUrl,
                ..Default::default()
            }),
            MessageInputPart::text("再解释"),
        ];
        let context = MediaFetchContext {
            platform: "qq_official",
            app_id: "app".to_owned(),
            peer_id: "peer".to_owned(),
            root_dir,
            timeout: Duration::from_secs(3),
        };

        fetch_qq_official_image_attachments(
            &reqwest::Client::new(),
            &context,
            "msg-1",
            &mut parts,
            &[attachment],
        )
        .await;

        assert_eq!(parts[0].text_content(), Some("先看这张"));
        assert_eq!(parts[2].text_content(), Some("再解释"));
        let MessageInputPart::Image { media } = &parts[1] else {
            panic!("expected image part");
        };
        let local_path = media.local_path.as_deref().unwrap();
        assert_eq!(media.status, MediaStatus::Available);
        assert_eq!(std::fs::read(local_path).unwrap(), b"fake-jpeg");
    }

    #[tokio::test]
    async fn file_url_attachment_is_rejected_without_path_leak() {
        let attachment = Attachment {
            content_type: Some("image/jpeg".to_owned()),
            filename: Some("a.jpg".to_owned()),
            url: Some("file://C:\\Users\\ThinkPad\\Pictures\\a.jpg".to_owned()),
            size_bytes: None,
            media_id: None,
            file_id: None,
            attachment_id: None,
        };
        let mut parts = vec![MessageInputPart::image(MessageMedia {
            mime_type: attachment.content_type.clone(),
            filename: attachment.filename.clone(),
            url: attachment.url.clone(),
            status: MediaStatus::MissingReadableUrl,
            ..Default::default()
        })];
        let context = MediaFetchContext {
            platform: "qq_official",
            app_id: "app".to_owned(),
            peer_id: "peer".to_owned(),
            root_dir: std::env::temp_dir(),
            timeout: Duration::from_secs(3),
        };

        fetch_qq_official_image_attachments(
            &reqwest::Client::new(),
            &context,
            "msg-1",
            &mut parts,
            &[attachment],
        )
        .await;

        let MessageInputPart::Image { media } = &parts[0] else {
            panic!("expected image part");
        };
        assert_eq!(media.local_path, None);
        assert_eq!(media.remote_url(), None);
        assert_eq!(media.status, MediaStatus::MissingReadableUrl);
        assert!(
            !MessageInputPart::image(media.clone())
                .fallback_text()
                .contains("C:\\Users")
        );
    }

    #[tokio::test]
    async fn downloads_multiple_image_attachments_concurrently() {
        let app = Router::new()
            .route(
                "/a.jpg",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    (
                        [(reqwest::header::CONTENT_TYPE.as_str(), "image/jpeg")],
                        Bytes::from_static(b"a"),
                    )
                }),
            )
            .route(
                "/b.jpg",
                get(|| async {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    (
                        [(reqwest::header::CONTENT_TYPE.as_str(), "image/jpeg")],
                        Bytes::from_static(b"b"),
                    )
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let attachments = ["a.jpg", "b.jpg"]
            .into_iter()
            .map(|filename| Attachment {
                content_type: Some("image/jpeg".to_owned()),
                filename: Some(filename.to_owned()),
                url: Some(format!("http://{addr}/{filename}")),
                size_bytes: None,
                media_id: None,
                file_id: None,
                attachment_id: None,
            })
            .collect::<Vec<_>>();
        let mut parts = attachments
            .iter()
            .map(|attachment| {
                MessageInputPart::image(MessageMedia {
                    mime_type: attachment.content_type.clone(),
                    filename: attachment.filename.clone(),
                    url: attachment.url.clone(),
                    status: MediaStatus::MissingReadableUrl,
                    ..Default::default()
                })
            })
            .collect::<Vec<_>>();
        let context = MediaFetchContext {
            platform: "qq_official",
            app_id: "app".to_owned(),
            peer_id: "peer".to_owned(),
            root_dir: std::env::temp_dir().join(format!(
                "qq-maid-media-fetch-parallel-test-{}",
                MEDIA_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
            )),
            timeout: Duration::from_secs(3),
        };

        let started = Instant::now();
        fetch_qq_official_image_attachments(
            &reqwest::Client::new(),
            &context,
            "msg-1",
            &mut parts,
            &attachments,
        )
        .await;

        assert!(
            started.elapsed() < Duration::from_millis(350),
            "downloads should overlap instead of running sequentially"
        );
        assert!(parts.iter().all(|part| matches!(
            part,
            MessageInputPart::Image { media }
                if media.status == MediaStatus::Available && media.local_path.is_some()
        )));
    }
}

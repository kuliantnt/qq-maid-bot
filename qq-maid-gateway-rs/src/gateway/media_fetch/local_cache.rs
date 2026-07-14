//! QQ 官方媒体的本地缓存路径、文件名、MIME 与清理策略。
//!
//! 网络下载仍由父模块负责；本模块只处理本地文件布局和缓存上限，避免文件系统
//! 细节与附件下载状态机混在同一文件中。

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use tracing::warn;

use super::{
    Attachment, MEDIA_CACHE_MAX_BYTES, MEDIA_CACHE_TTL, MEDIA_FILE_COUNTER, MediaFetchContext,
};

pub(super) fn cleanup_media_cache_best_effort(root: &Path) {
    if let Err(error) = cleanup_media_cache_with_limits(
        root,
        SystemTime::now(),
        MEDIA_CACHE_TTL,
        MEDIA_CACHE_MAX_BYTES,
    ) {
        warn!(
            error = %error,
            "QQ official media cache cleanup failed"
        );
    }
}

pub(super) fn cleanup_media_cache_with_limits(
    root: &Path,
    now: SystemTime,
    ttl: Duration,
    max_bytes: u64,
) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    let mut files = Vec::new();
    collect_media_cache_files(root, &mut files)?;

    let mut retained = Vec::new();
    let mut total_bytes = 0_u64;
    for file in files {
        let expired = now.duration_since(file.modified).is_ok_and(|age| age > ttl);
        if expired {
            let _ = fs::remove_file(&file.path);
            continue;
        }
        total_bytes = total_bytes.saturating_add(file.len);
        retained.push(file);
    }

    retained.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.path.cmp(&right.path))
    });
    for file in retained {
        if total_bytes <= max_bytes {
            break;
        }
        if fs::remove_file(&file.path).is_ok() {
            total_bytes = total_bytes.saturating_sub(file.len);
        }
    }
    Ok(())
}

#[derive(Debug)]
struct MediaCacheFile {
    path: PathBuf,
    modified: SystemTime,
    len: u64,
}

fn collect_media_cache_files(root: &Path, output: &mut Vec<MediaCacheFile>) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_media_cache_files(&path, output)?;
        } else if metadata.is_file() {
            output.push(MediaCacheFile {
                path,
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                len: metadata.len(),
            });
        }
    }
    Ok(())
}

pub(super) fn media_dir(context: &MediaFetchContext) -> PathBuf {
    context
        .root_dir
        .join(safe_path_segment(context.platform))
        .join(safe_path_segment(&context.app_id))
        .join(safe_path_segment(&context.peer_id))
}

pub(super) fn unique_filename(attachment: &Attachment, response_mime: Option<&str>) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let counter = MEDIA_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let fallback = filename_for_mime(response_mime, attachment.filename.as_deref());
    format!("{timestamp}-{counter}-{}", safe_filename(&fallback))
}

pub(super) fn partial_download_path(local_path: &Path) -> PathBuf {
    let filename = local_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("image.bin");
    local_path.with_file_name(format!(".{filename}.part"))
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

pub(super) fn clean_mime_type(value: &str) -> Option<String> {
    canonical_image_mime(value.split(';').next().map(str::trim).unwrap_or_default())
        .map(str::to_owned)
}

pub(super) fn preferred_image_mime(
    attachment_content_type: Option<&str>,
    response_mime: Option<&str>,
    filename: Option<&str>,
) -> Option<String> {
    let attachment_mime = attachment_content_type.and_then(canonical_image_mime);
    if attachment_mime.is_some() {
        return attachment_mime.map(str::to_owned);
    }
    response_mime
        .and_then(canonical_image_mime)
        .or_else(|| filename.and_then(infer_image_mime_type_from_filename))
        .map(str::to_owned)
}

fn canonical_image_mime(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/png" => Some("image/png"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "image/bmp" => Some("image/bmp"),
        _ => None,
    }
}

fn infer_image_mime_type_from_filename(filename: &str) -> Option<&'static str> {
    match filename
        .trim()
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("bmp") => Some("image/bmp"),
        _ => None,
    }
}

pub(super) fn normalized_url_scheme(url: &str) -> &'static str {
    if url.starts_with("https://") {
        "https"
    } else if url.starts_with("http://") {
        "http"
    } else {
        "other"
    }
}

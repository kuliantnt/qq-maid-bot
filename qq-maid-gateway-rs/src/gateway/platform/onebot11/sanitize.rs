//! OneBot 消息段字段清洗与媒体元数据校验。
//!
//! 客户端上报值只在这里转换为可进入统一入站模型的安全字段；本机路径、data URL
//! 和不可信 MIME 等内容不得绕过该边界进入 Core。

use std::collections::BTreeMap;

use qq_maid_common::input_part::MediaStatus;
use serde_json::Value;

use super::{OneBotMediaKind, id_from_value};

pub(super) fn clean_data_string(data: &BTreeMap<String, Value>, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .filter_map(|field| data.get(*field).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

pub(super) fn clean_data_id(data: &BTreeMap<String, Value>, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| data.get(*field).and_then(id_from_value))
}

pub(super) fn clean_data_u64(data: &BTreeMap<String, Value>, fields: &[&str]) -> Option<u64> {
    fields.iter().find_map(|field| match data.get(*field) {
        Some(Value::Number(value)) => value.as_u64(),
        Some(Value::String(value)) => value.trim().parse::<u64>().ok(),
        _ => None,
    })
}

pub(super) fn explicit_media_status(data: &BTreeMap<String, Value>) -> Option<MediaStatus> {
    let status = clean_data_string(data, &["download_status", "status"])?.to_ascii_lowercase();
    match status.as_str() {
        "missing" | "missing_readable_url" => Some(MediaStatus::MissingReadableUrl),
        "size_exceeded" | "too_large" => Some(MediaStatus::SizeExceeded),
        "unsupported" | "unsupported_type" => Some(MediaStatus::UnsupportedType),
        "download_failed" | "failed" => Some(MediaStatus::DownloadFailed),
        "expired" | "url_expired" => Some(MediaStatus::Expired),
        // `ok`/`available` 仍需通过 URL 安全判定，不能让客户端状态绕过 scheme 校验。
        _ => None,
    }
}

pub(super) fn safe_remote_url(value: &str) -> Option<String> {
    let value = value.trim();
    let parsed = reqwest::Url::parse(value).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some())
        .then(|| value.to_owned())
}

pub(super) fn safe_filename(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.len() <= 255
        && !value.contains(['/', '\\', ':'])
        && !value.to_ascii_lowercase().starts_with("base64"))
    .then(|| value.to_owned())
}

pub(super) fn safe_opaque_reference(value: &str) -> Option<String> {
    safe_filename(value)
}

pub(super) fn safe_mime_type(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    (!value.is_empty()
        && value.len() <= 127
        && value.is_ascii()
        && value.contains('/')
        && !value.contains(char::is_whitespace))
    .then_some(value)
}

pub(super) fn infer_image_mime(filename: Option<&str>, kind: OneBotMediaKind) -> Option<String> {
    if !matches!(kind, OneBotMediaKind::Image) {
        return None;
    }
    let extension = filename?.rsplit('.').next()?.to_ascii_lowercase();
    let mime = match extension.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => return None,
    };
    Some(mime.to_owned())
}

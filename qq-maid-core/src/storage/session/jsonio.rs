//! Session JSON 编码/解码 helper。
//!
//! 把 `SessionRecord` 的各 JSON 字段（state / extra / pending / 最近查询与最近操作
//! 快照）序列化与反序列化集中在此，便于行映射与 upsert 复用。失败统一封装为
//! `SessionError` 的 encode_error / decode_error，不改变持久化格式与兼容旧数据语义。

use serde::{Serialize, de::DeserializeOwned};

use super::SessionError;

/// 序列化为 JSON 字符串，失败封装成 encode_error。
pub(super) fn encode_json<T: Serialize>(value: &T, field: &str) -> Result<String, SessionError> {
    serde_json::to_string(value)
        .map_err(|err| SessionError::encode(format!("failed to encode {field}: {err}")))
}

/// 可选值序列化：None 直接返回 None，Some 才编码。
pub(super) fn encode_optional_json<T: Serialize>(
    value: &Option<T>,
    field: &str,
) -> Result<Option<String>, SessionError> {
    value
        .as_ref()
        .map(|value| encode_json(value, field))
        .transpose()
}

/// 反序列化字段；空串视为缺失（兼容旧数据），失败封装成 decode_error。
pub(super) fn decode_json<T: DeserializeOwned>(text: &str, field: &str) -> Result<T, SessionError> {
    serde_json::from_str(text)
        .map_err(|err| SessionError::decode(format!("failed to decode {field}: {err}")))
}

/// 反序列化可选字段：None 或空白视为缺失，避免把空串误判成非法 JSON。
pub(super) fn decode_optional_json<T: DeserializeOwned>(
    text: Option<&str>,
    field: &str,
) -> Result<Option<T>, SessionError> {
    let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
        return Ok(None);
    };
    serde_json::from_str(text)
        .map(Some)
        .map_err(|err| SessionError::decode(format!("failed to decode {field}: {err}")))
}

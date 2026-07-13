//! OneBot 11 最小协议模型。
//!
//! 一期消费生命周期、心跳、API response 与消息 segment 数组；具体入站映射由平台
//! adapter 完成，不能把原始 JSON 泄漏到 Core。

use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Visitor};
use serde_json::Value;

/// OneBot 的账号、群和用户 ID 允许使用 JSON 数字或字符串。
///
/// 内部统一保存十进制/原始字符串，避免经过 `f64` 导致大整数精度损失。通用序列化仍
/// 保留字符串形式；发送 action 的 `user_id`/`group_id` 必须在 sender 边界转换为 JSON
/// number，不能直接依赖本类型的序列化结果。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OneBotId(String);

impl OneBotId {
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err("OneBot ID must not be empty");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OneBotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OneBotId(REDACTED)")
    }
}

impl Serialize for OneBotId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for OneBotId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct IdVisitor;

        impl Visitor<'_> for IdVisitor {
            type Value = OneBotId;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a non-empty string or integer OneBot ID")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                OneBotId::new(value).map_err(E::custom)
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                OneBotId::new(value).map_err(E::custom)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(OneBotId(value.to_string()))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(OneBotId(value.to_string()))
            }
        }

        deserializer.deserialize_any(IdVisitor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessageSegment {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub data: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum OneBotMessage {
    Segments(Vec<MessageSegment>),
    CqCode(String),
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct OneBotEvent {
    #[serde(default)]
    pub time: Option<i64>,
    pub self_id: OneBotId,
    pub post_type: String,
    #[serde(default)]
    pub message_type: Option<String>,
    #[serde(default)]
    pub notice_type: Option<String>,
    #[serde(default)]
    pub request_type: Option<String>,
    #[serde(default)]
    pub meta_event_type: Option<String>,
    #[serde(default)]
    pub sub_type: Option<String>,
    #[serde(default)]
    pub interval: Option<u64>,
    #[serde(default)]
    pub status: Option<Value>,
    #[serde(default)]
    pub message: Option<OneBotMessage>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl OneBotEvent {
    pub fn is_heartbeat(&self) -> bool {
        self.post_type == "meta_event" && self.meta_event_type.as_deref() == Some("heartbeat")
    }

    pub fn is_lifecycle(&self) -> bool {
        self.post_type == "meta_event" && self.meta_event_type.as_deref() == Some("lifecycle")
    }
}

/// `echo` 在 OneBot 11 中可以是任意 JSON 值，必须原样关联请求与响应。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct Echo(pub Value);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionRequest {
    pub action: String,
    #[serde(default)]
    pub params: Value,
    pub echo: Echo,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionResponse {
    pub status: String,
    pub retcode: i64,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub wording: Option<String>,
    #[serde(default)]
    pub echo: Option<Echo>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn id_accepts_string_or_integer_without_precision_loss() {
        let numeric: OneBotId = serde_json::from_str("18446744073709551615").unwrap();
        let text: OneBotId = serde_json::from_str("\"18446744073709551615\"").unwrap();

        assert_eq!(numeric.as_str(), "18446744073709551615");
        assert_eq!(numeric, text);
        assert_eq!(
            serde_json::to_value(numeric).unwrap(),
            json!("18446744073709551615")
        );
    }

    #[test]
    fn parses_lifecycle_heartbeat_and_message_segments() {
        let heartbeat: OneBotEvent = serde_json::from_value(json!({
            "time": 1,
            "self_id": 123456789012345678_u64,
            "post_type": "meta_event",
            "meta_event_type": "heartbeat",
            "status": {"online": true},
            "interval": 5000
        }))
        .unwrap();
        assert!(heartbeat.is_heartbeat());
        assert_eq!(heartbeat.self_id.as_str(), "123456789012345678");

        let message: OneBotEvent = serde_json::from_value(json!({
            "self_id": "42",
            "post_type": "message",
            "message_type": "private",
            "message": [{"type": "text", "data": {"text": "hello"}}]
        }))
        .unwrap();
        let Some(OneBotMessage::Segments(segments)) = message.message else {
            panic!("expected array message segments");
        };
        assert_eq!(segments[0].kind, "text");

        let cq_message: OneBotEvent = serde_json::from_value(json!({
            "self_id": "42",
            "post_type": "message",
            "message_type": "private",
            "message": "hello[CQ:image,file=test.jpg]"
        }))
        .unwrap();
        assert!(matches!(cq_message.message, Some(OneBotMessage::CqCode(_))));
    }

    #[test]
    fn action_response_preserves_echo_value() {
        let response: ActionResponse = serde_json::from_value(json!({
            "status": "ok",
            "retcode": 0,
            "data": {"user_id": "42"},
            "echo": {"request": 7}
        }))
        .unwrap();

        assert_eq!(response.echo, Some(Echo(json!({"request": 7}))));
    }
}

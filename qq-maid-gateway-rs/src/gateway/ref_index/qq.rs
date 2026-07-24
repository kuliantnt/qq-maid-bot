//! QQ 官方引用索引字段解析。
//!
//! QQ 官方在 `message_scene.ext` 中分别下发当前消息与被引用消息的索引。

use serde::Deserialize;
use serde_json::{Map, Value};

pub(crate) const MSG_TYPE_QUOTE: u64 = 103;

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RawMessageScene {
    #[serde(default)]
    pub(crate) ext: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RawMsgElement {
    /// 元素的 `msg_idx` 仅用于反序列化；按 QQ 最新文档，引用内容解析不再以
    /// `msg_idx == ref_msg_idx` 筛选元素，因此本字段不再被业务代码读取。
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) msg_idx: Option<String>,
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) message_type: Option<u64>,
    #[serde(default)]
    pub(crate) attachments: Vec<crate::gateway::event::Attachment>,
    #[serde(default)]
    pub(crate) ark_data: Option<RawArkData>,
    /// 引用根元素可能继续包含有序子元素；只有根元素及其后代属于被引用消息。
    #[serde(default)]
    pub(crate) msg_elements: Vec<RawMsgElement>,
}

/// QQ ARK 卡片的最小安全 DTO。`fields` 保持为动态对象，只读取明确白名单字段，
/// 避免平台新增的任意嵌套结构直接进入 Gateway 或模型上下文。
#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RawArkData {
    #[serde(default)]
    pub(crate) prompt: Option<String>,
    #[serde(default, rename = "type")]
    pub(crate) ark_type: Option<Value>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) ark_name: Option<String>,
    #[serde(default)]
    pub(crate) fields: Map<String, Value>,
    /// 官方卡片的展示字段可能直接位于 `ark_data` 顶层；仍只由 normalizer 读取白名单键。
    #[serde(default, flatten)]
    pub(crate) extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct QqRefIndices {
    pub(crate) msg_idx: Option<String>,
    pub(crate) ref_msg_idx: Option<String>,
}

pub(crate) fn parse_ref_indices(scene: Option<&RawMessageScene>) -> QqRefIndices {
    let mut indices = QqRefIndices::default();
    if let Some(scene) = scene {
        for item in &scene.ext {
            let item = item.trim();
            if let Some(value) = item.strip_prefix("msg_idx=") {
                indices.msg_idx = clean_idx(value);
            } else if let Some(value) = item.strip_prefix("ref_msg_idx=") {
                indices.ref_msg_idx = clean_idx(value);
            }
        }
    }
    indices
}

fn clean_idx(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_indices_from_message_scene_ext() {
        let scene = RawMessageScene {
            ext: vec![
                "msg_idx=REFIDX_current".to_owned(),
                "ref_msg_idx=REFIDX_old".to_owned(),
            ],
        };

        let indices = parse_ref_indices(Some(&scene));

        assert_eq!(indices.msg_idx.as_deref(), Some("REFIDX_current"));
        assert_eq!(indices.ref_msg_idx.as_deref(), Some("REFIDX_old"));
    }

    #[test]
    fn missing_scene_reference_is_not_inferred_from_elements() {
        let scene = RawMessageScene {
            ext: vec!["msg_idx=REFIDX_current".to_owned()],
        };

        let indices = parse_ref_indices(Some(&scene));

        assert_eq!(indices.msg_idx.as_deref(), Some("REFIDX_current"));
        assert_eq!(indices.ref_msg_idx, None);
    }
}

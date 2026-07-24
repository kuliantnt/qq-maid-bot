//! QQ 官方引用索引字段解析。
//!
//! QQ 官方在 `message_scene.ext` 中分别下发当前消息与被引用消息的索引。

use serde::Deserialize;

pub(crate) const MSG_TYPE_QUOTE: u64 = 103;

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RawMessageScene {
    #[serde(default)]
    pub(crate) ext: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct RawMsgElement {
    #[serde(default)]
    pub(crate) msg_idx: Option<String>,
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[serde(default)]
    pub(crate) attachments: Vec<crate::gateway::event::Attachment>,
    /// 引用根元素可能继续包含有序子元素；只有根元素及其后代属于被引用消息。
    #[serde(default)]
    pub(crate) msg_elements: Vec<RawMsgElement>,
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

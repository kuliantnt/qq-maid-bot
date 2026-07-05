//! QQ 官方引用索引字段解析。
//!
//! QQ 引用消息常见只下发 `REFIDX_*`，字段位于 `message_scene.ext`；
//! 引用消息类型下 `msg_elements[0].msg_idx` 更接近被引用消息索引。

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
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct QqRefIndices {
    pub(crate) msg_idx: Option<String>,
    pub(crate) ref_msg_idx: Option<String>,
}

pub(crate) fn parse_ref_indices(
    scene: Option<&RawMessageScene>,
    message_type: Option<u64>,
    msg_elements: &[RawMsgElement],
) -> QqRefIndices {
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
    if message_type == Some(MSG_TYPE_QUOTE)
        && let Some(value) = msg_elements
            .first()
            .and_then(|item| item.msg_idx.as_deref())
    {
        indices.ref_msg_idx = clean_idx(value);
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

        let indices = parse_ref_indices(Some(&scene), None, &[]);

        assert_eq!(indices.msg_idx.as_deref(), Some("REFIDX_current"));
        assert_eq!(indices.ref_msg_idx.as_deref(), Some("REFIDX_old"));
    }

    #[test]
    fn quote_message_type_uses_first_msg_element_as_reference() {
        let scene = RawMessageScene {
            ext: vec!["ref_msg_idx=REFIDX_ext".to_owned()],
        };
        let elements = vec![RawMsgElement {
            msg_idx: Some("REFIDX_element".to_owned()),
            content: None,
            attachments: Vec::new(),
        }];

        let indices = parse_ref_indices(Some(&scene), Some(MSG_TYPE_QUOTE), &elements);

        assert_eq!(indices.ref_msg_idx.as_deref(), Some("REFIDX_element"));
    }
}

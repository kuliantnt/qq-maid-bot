use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MarkdownPayload {
    pub content: String,
}

#[derive(Debug, Serialize)]
struct C2cMarkdownPayload<'a> {
    msg_type: u8,
    markdown: &'a MarkdownPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

#[derive(Debug, Serialize)]
struct GroupMarkdownPayload<'a> {
    msg_type: u8,
    markdown: &'a MarkdownPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

impl MarkdownPayload {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }
}

pub fn build_c2c_markdown_payload(
    markdown: &MarkdownPayload,
    msg_id: Option<&str>,
    msg_seq: u32,
) -> Value {
    serde_json::to_value(C2cMarkdownPayload {
        msg_type: 2,
        markdown,
        msg_id,
        msg_seq,
    })
    .expect("C2C markdown payload should serialize")
}

pub fn build_group_markdown_payload(
    markdown: &MarkdownPayload,
    msg_id: Option<&str>,
    msg_seq: u32,
) -> Value {
    serde_json::to_value(GroupMarkdownPayload {
        msg_type: 2,
        markdown,
        msg_id,
        msg_seq,
    })
    .expect("group markdown payload should serialize")
}

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImagePayload {
    pub file_info: String,
}

#[derive(Debug, Serialize)]
struct C2cImagePayload<'a> {
    msg_type: u8,
    media: &'a ImagePayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<&'a str>,
    msg_seq: u32,
}

impl ImagePayload {
    pub fn new(file_info: impl Into<String>) -> Self {
        Self {
            file_info: file_info.into(),
        }
    }
}

pub fn build_c2c_image_payload(image: &ImagePayload, msg_id: Option<&str>, msg_seq: u32) -> Value {
    serde_json::to_value(C2cImagePayload {
        msg_type: 7,
        media: image,
        msg_id,
        msg_seq,
    })
    .expect("C2C image payload should serialize")
}

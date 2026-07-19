//! OpenAI Responses 响应提取逻辑。
//!
//! 兼容网关返回的 `response.completed` 事件并不总是完全一致：有的把完整响应放在
//! `response` 字段里，有的直接把完整结构内联到事件顶层。这里统一做提取，避免流式
//! 和非流式调用点分别兼容不同形态。

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use qq_maid_common::output_part::{OutputMedia, OutputPart};
use serde_json::Value;

use crate::provider::types::TokenUsage;

/// 从 OpenAI Responses API 响应中提取回复文本。
pub(crate) fn extract_response_output_text(body: &Value) -> Option<String> {
    if let Some(text) = body
        .get("output_text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some(text.to_owned());
    }

    let output = body.get("output").and_then(Value::as_array)?;
    let mut parts = Vec::new();
    for output_item in output {
        let Some(content_items) = output_item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for content_item in content_items {
            let item_type = content_item.get("type").and_then(Value::as_str);
            let text = match item_type {
                Some("refusal") => content_item.get("refusal").and_then(Value::as_str),
                Some("output_text") | None => content_item.get("text").and_then(Value::as_str),
                _ => None,
            };
            let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else {
                continue;
            };
            parts.push(text.to_owned());
        }
    }

    let answer = parts.join("\n\n");
    let answer = answer.trim();
    if answer.is_empty() {
        None
    } else {
        Some(answer.to_owned())
    }
}

/// 按 Responses `output` 顺序提取文本与最终图片。
///
/// 官方图片工具的最终结果是 `image_generation_call.result` base64；流式
/// `partial_image_b64` 只是预览，不作为最终出站图片，避免重复发送和额外占用内存。
pub(crate) fn extract_response_output_parts(body: &Value) -> Vec<OutputPart> {
    let Some(output) = body.get("output").and_then(Value::as_array) else {
        return extract_response_output_text(body)
            .map(|text| vec![OutputPart::Text { text }])
            .unwrap_or_default();
    };
    let mut parts = Vec::new();
    for output_item in output {
        let output_type = output_item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("");
        tracing::debug!(output_type, "observed OpenAI Responses output item");
        match output_type {
            "image_generation_call" => {
                let Some(result) = output_item
                    .get("result")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|result| !result.is_empty())
                else {
                    continue;
                };
                // 只接受可解码的标准 base64，避免把未知 result 字段误当作图片。
                if BASE64_STANDARD.decode(result).is_err() {
                    tracing::warn!(
                        output_type = "image_generation_call",
                        result_chars = result.len(),
                        "ignored invalid base64 OpenAI image generation result"
                    );
                    continue;
                }
                parts.push(OutputPart::Image {
                    media: OutputMedia {
                        mime_type: Some("image/png".to_owned()),
                        filename: Some("generated-image.png".to_owned()),
                        data_base64: Some(result.to_owned()),
                        fallback_text: Some("图片已生成，但发送失败。".to_owned()),
                        ..OutputMedia::default()
                    },
                });
            }
            _ => {
                let Some(content) = output_item.get("content").and_then(Value::as_array) else {
                    continue;
                };
                for item in content {
                    let text = match item.get("type").and_then(Value::as_str) {
                        Some("refusal") => item.get("refusal").and_then(Value::as_str),
                        Some("output_text") | None => item.get("text").and_then(Value::as_str),
                        _ => None,
                    };
                    if let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) {
                        parts.push(OutputPart::Text {
                            text: text.to_owned(),
                        });
                    }
                }
            }
        }
    }
    parts
}

/// 从 OpenAI Responses API 响应中提取 token usage。
pub(crate) fn extract_response_usage(body: &Value) -> Option<TokenUsage> {
    let usage = body.get("usage")?;
    let input_tokens = usage.get("input_tokens").and_then(Value::as_u64);
    let cached_input_tokens = usage
        .get("input_tokens_details")
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64);
    let output_tokens = usage.get("output_tokens").and_then(Value::as_u64);
    let total_tokens = usage.get("total_tokens").and_then(Value::as_u64);
    if matches!(
        (
            input_tokens,
            output_tokens,
            total_tokens,
            cached_input_tokens
        ),
        (None | Some(0), None | Some(0), None | Some(0), None)
    ) {
        return None;
    }
    Some(TokenUsage {
        input_tokens,
        cached_input_tokens,
        output_tokens,
        total_tokens,
    })
}

/// 从 `response.completed` SSE 事件里提取最终响应体。
pub(crate) fn extract_completed_response(value: &Value) -> Option<Value> {
    value
        .get("response")
        .cloned()
        .or_else(|| Some(value.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_response_output_text_from_various_shapes() {
        struct Case {
            name: &'static str,
            body: Value,
            expected: Option<&'static str>,
        }

        let cases = [
            Case {
                name: "top_level_output_text",
                body: serde_json::json!({"output_text": " answer "}),
                expected: Some("answer"),
            },
            Case {
                name: "nested_output_text",
                body: serde_json::json!({
                    "output": [{
                        "type": "message",
                        "content": [{"type": "output_text", "text": " nested answer "}]
                    }]
                }),
                expected: Some("nested answer"),
            },
            Case {
                name: "nested_refusal",
                body: serde_json::json!({
                    "output": [{
                        "type": "message",
                        "content": [{"type": "refusal", "refusal": " no "}]
                    }]
                }),
                expected: Some("no"),
            },
            Case {
                name: "empty",
                body: serde_json::json!({"output": []}),
                expected: None,
            },
        ];

        for case in &cases {
            assert_eq!(
                extract_response_output_text(&case.body).as_deref(),
                case.expected,
                "case '{}' failed",
                case.name
            );
        }
    }

    #[test]
    fn extracts_text_and_image_generation_result_in_output_order() {
        let body = serde_json::json!({
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "先看图"}]},
                {"type": "image_generation_call", "status": "completed", "result": "aGVsbG8="},
                {"type": "message", "content": [{"type": "output_text", "text": "完成"}]}
            ]
        });

        let parts = extract_response_output_parts(&body);
        assert!(matches!(&parts[0], OutputPart::Text { text } if text == "先看图"));
        assert!(
            matches!(&parts[1], OutputPart::Image { media } if media.data_base64.as_deref() == Some("aGVsbG8="))
        );
        assert!(matches!(&parts[2], OutputPart::Text { text } if text == "完成"));
    }

    #[test]
    fn extracts_response_usage() {
        let body = serde_json::json!({
            "usage": {
                "input_tokens": 10,
                "output_tokens": 4,
                "total_tokens": 14
            }
        });

        assert_eq!(
            extract_response_usage(&body),
            Some(TokenUsage {
                input_tokens: Some(10),
                cached_input_tokens: None,
                output_tokens: Some(4),
                total_tokens: Some(14),
            })
        );
    }

    #[test]
    fn extracts_response_cached_input_tokens() {
        let body = serde_json::json!({
            "usage": {
                "input_tokens": 10,
                "input_tokens_details": {
                    "cached_tokens": 6
                },
                "output_tokens": 4,
                "total_tokens": 14
            }
        });

        assert_eq!(
            extract_response_usage(&body),
            Some(TokenUsage {
                input_tokens: Some(10),
                cached_input_tokens: Some(6),
                output_tokens: Some(4),
                total_tokens: Some(14),
            })
        );
    }

    #[test]
    fn response_cached_input_tokens_missing_stays_compatible() {
        let body = serde_json::json!({
            "usage": {
                "input_tokens": 10,
                "output_tokens": 4,
                "total_tokens": 14
            }
        });

        assert_eq!(
            extract_response_usage(&body),
            Some(TokenUsage {
                input_tokens: Some(10),
                cached_input_tokens: None,
                output_tokens: Some(4),
                total_tokens: Some(14),
            })
        );
    }

    #[test]
    fn extract_completed_response_prefers_nested_response() {
        let body = serde_json::json!({
            "type": "response.completed",
            "response": {"output_text": "nested"}
        });
        assert_eq!(
            extract_completed_response(&body),
            Some(serde_json::json!({"output_text": "nested"}))
        );
    }
}

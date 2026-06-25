//! OpenAI Responses 响应提取逻辑。
//!
//! 兼容网关返回的 `response.completed` 事件并不总是完全一致：有的把完整响应放在
//! `response` 字段里，有的直接把完整结构内联到事件顶层。这里统一做提取，避免流式
//! 和非流式调用点分别兼容不同形态。

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

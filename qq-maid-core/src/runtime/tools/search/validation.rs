//! Web Search Tool 参数解析与错误投影。
//!
//! 参数错误必须在构造 `WebSearchRequest` 前被识别；该模块只保留字段级、低敏的
//! 诊断信息，不把 query、raw_question 或完整参数对象带入日志或模型错误。

use serde_json::{Value, json};

use qq_maid_llm::{tool::ToolContext, web_search::WebSearchBackend};

use crate::error::LlmError;

use super::{WEB_SEARCH_MAX_RESULTS_LIMIT, WEB_SEARCH_QUERY_MAX_LENGTH, WebSearchToolRequest};

/// Web Search 参数校验失败时使用的本地结构化错误。
///
/// 该错误只保留字段、稳定原因和值类型；`safe_value` 仅允许短的低敏标量，
/// 查询正文和原始问题始终不进入错误或日志。显式调用最终仍转换为原有
/// `LlmError`，Agent 调用则把同一份诊断投影为可纠正的 ToolOutput。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WebSearchArgumentError {
    pub(super) field: String,
    pub(super) reason: &'static str,
    pub(super) message: String,
    pub(super) value_kind: &'static str,
    pub(super) safe_value: Option<String>,
    pub(super) query_chars: Option<usize>,
}

impl WebSearchArgumentError {
    pub(super) fn new(
        field: impl Into<String>,
        reason: &'static str,
        message: impl Into<String>,
        value: Option<&Value>,
        safe_value: Option<String>,
        query_chars: Option<usize>,
    ) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
            reason,
            value_kind: argument_value_kind(value),
            safe_value,
            query_chars,
        }
    }

    pub(super) fn into_llm_error(self) -> LlmError {
        LlmError::new("bad_tool_arguments", self.message, "tool")
    }

    pub(super) fn agent_output(&self, backend: &str) -> Value {
        json!({
            "ok": false,
            "execution_succeeded": false,
            "backend": backend,
            "answer": "",
            "sources": [],
            "result_count": 0,
            "error": {
                "code": "invalid_arguments",
                "stage": "tool",
                "argument": self.field,
                "reason": self.reason,
                "value_kind": self.value_kind,
                "message": self.message,
                "kind": "invalid_arguments",
                "retriable": false,
                "retryable_by_model": true,
            },
        })
    }
}

/// 研究模式会在一次 Tool 调用中先校验共享选项，再执行多个搜索请求；保留参数
/// 错误与运行时 LLM 错误的边界，避免 Agent 把网络失败误判成可纠正参数错误。
pub(super) enum WebSearchToolError {
    Argument(WebSearchArgumentError),
    Execution(LlmError),
}

impl From<WebSearchArgumentError> for WebSearchToolError {
    fn from(error: WebSearchArgumentError) -> Self {
        Self::Argument(error)
    }
}

impl From<LlmError> for WebSearchToolError {
    fn from(error: LlmError) -> Self {
        Self::Execution(error)
    }
}

pub(super) fn request_from_arguments(
    context: &ToolContext,
    arguments: &Value,
    server_backend_override: Option<WebSearchBackend>,
    server_model_override: Option<String>,
) -> Result<WebSearchToolRequest, WebSearchArgumentError> {
    // 搜索模型路由只允许 `/查` 等服务端直接执行入口注入；模型 Tool Loop 调用
    // 会带稳定 tool_call_id，此时忽略任何伪造的 model_override 参数。
    let model_override = server_model_override.or_else(|| {
        context
            .tool_call_id
            .is_none()
            .then(|| optional_string_field(arguments, "model_override"))
            .flatten()
    });
    Ok(WebSearchToolRequest {
        query: parse_query(arguments)?,
        raw_question: optional_string_field(arguments, "raw_question"),
        max_results: parse_max_results(arguments.get("max_results"))?,
        context_size: parse_context_size(arguments.get("context_size"))?,
        topic: parse_topic(arguments.get("topic"))?,
        time_range: parse_time_range(arguments.get("time_range"))?,
        backend_override: server_backend_override,
        model_override,
    })
}

pub(super) fn parse_query(arguments: &Value) -> Result<String, WebSearchArgumentError> {
    let Some(value) = arguments.get("query") else {
        return Err(WebSearchArgumentError::new(
            "query",
            "missing_or_empty",
            "web_search requires non-empty query",
            None,
            None,
            Some(0),
        ));
    };
    let Some(text) = value.as_str() else {
        return Err(WebSearchArgumentError::new(
            "query",
            if value.is_null() {
                "missing_or_empty"
            } else {
                "invalid_type"
            },
            "web_search requires non-empty query",
            Some(value),
            None,
            None,
        ));
    };
    let query = text.trim();
    if query.is_empty() {
        return Err(WebSearchArgumentError::new(
            "query",
            "missing_or_empty",
            "web_search requires non-empty query",
            Some(value),
            None,
            Some(0),
        ));
    }
    if query.chars().count() > WEB_SEARCH_QUERY_MAX_LENGTH {
        return Err(WebSearchArgumentError::new(
            "query",
            "too_long",
            "query is too long",
            Some(value),
            None,
            Some(query.chars().count()),
        ));
    }
    Ok(query.to_owned())
}

pub(super) fn parse_max_results(
    value: Option<&Value>,
) -> Result<Option<u8>, WebSearchArgumentError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) if !number.is_f64() => match number.as_u64() {
            Some(value) if (1..=WEB_SEARCH_MAX_RESULTS_LIMIT as u64).contains(&value) => {
                Ok(Some(value as u8))
            }
            _ => Err(WebSearchArgumentError::new(
                "max_results",
                "out_of_range",
                "max_results must be an integer between 1 and 10 or null",
                value,
                low_sensitivity_safe_value(value),
                None,
            )),
        },
        Some(value) => Err(WebSearchArgumentError::new(
            "max_results",
            "invalid_type",
            "max_results must be an integer between 1 and 10 or null",
            Some(value),
            low_sensitivity_safe_value(Some(value)),
            None,
        )),
    }
}

pub(super) fn parse_context_size(
    value: Option<&Value>,
) -> Result<Option<String>, WebSearchArgumentError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim();
            if matches!(text, "low" | "medium" | "high") {
                Ok(Some(text.to_owned()))
            } else {
                Err(WebSearchArgumentError::new(
                    "context_size",
                    "unsupported_value",
                    "context_size must be low, medium, high, or null",
                    value,
                    low_sensitivity_safe_value(value),
                    None,
                ))
            }
        }
        Some(value) => Err(WebSearchArgumentError::new(
            "context_size",
            "invalid_type",
            "context_size must be low, medium, high, or null",
            Some(value),
            low_sensitivity_safe_value(Some(value)),
            None,
        )),
    }
}

pub(super) fn parse_topic(value: Option<&Value>) -> Result<Option<String>, WebSearchArgumentError> {
    parse_optional_enum(value, "topic", &["general", "news", "finance"])
}

pub(super) fn parse_time_range(
    value: Option<&Value>,
) -> Result<Option<String>, WebSearchArgumentError> {
    parse_optional_enum(value, "time_range", &["day", "week", "month", "year"])
}

fn parse_optional_enum(
    value: Option<&Value>,
    name: &str,
    allowed: &[&str],
) -> Result<Option<String>, WebSearchArgumentError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim().to_ascii_lowercase();
            if text == "null" {
                // 部分模型会把 schema 中的 null 序列化成字符串；可选枚举字段
                // 按未设置处理，避免把兼容性问题误报成上游搜索故障。
                Ok(None)
            } else if allowed.contains(&text.as_str()) {
                Ok(Some(text))
            } else {
                Err(WebSearchArgumentError::new(
                    name,
                    "unsupported_value",
                    format!("{name} must be one of {} or null", allowed.join(", ")),
                    value,
                    low_sensitivity_safe_value(value),
                    None,
                ))
            }
        }
        Some(value) => Err(WebSearchArgumentError::new(
            name,
            "invalid_type",
            format!("{name} must be a string or null"),
            Some(value),
            low_sensitivity_safe_value(Some(value)),
            None,
        )),
    }
}

fn argument_value_kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(number)) if number.is_i64() || number.is_u64() => "integer",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

/// 受限参数只保留短、低敏的标量候选，避免把模型偶然放入枚举字段的正文写入日志。
///
/// 调用方必须仅限于 `topic`、`time_range`、`context_size`、`max_results` 等受限
/// 参数；query、raw_question 和研究正文不得复用此投影。
fn low_sensitivity_safe_value(value: Option<&Value>) -> Option<String> {
    let text = match value {
        Some(Value::String(value)) => value.trim().to_owned(),
        Some(Value::Number(value)) => value.to_string(),
        _ => return None,
    };
    if text.chars().count() > 32
        || text.is_empty()
        || !text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_+".contains(character))
    {
        return None;
    }
    Some(text)
}

pub(super) fn optional_string_field(arguments: &Value, key: &str) -> Option<String> {
    match arguments.get(key) {
        Some(Value::String(value)) => {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        }
        _ => None,
    }
}

pub(super) fn normalize_dedup_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

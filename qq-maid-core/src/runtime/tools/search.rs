//! 联网搜索 Tool。
//!
//! 该 Tool 复用现有 `QueryExecutor`，把 OpenAI Responses web_search 能力纳入
//! 服务端白名单 ToolRegistry。`/查` 只作为显式触发入口，仍在 respond/search_flow.rs
//! 负责参数兼容、session 记录和用户可见错误文案。

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use qq_maid_llm::tool::{Tool, ToolContext, ToolMetadata, ToolOutput};

use crate::{
    error::LlmError,
    runtime::query::{DynQueryExecutor, QueryOutcome, QueryRequest, QuerySource},
};

pub(crate) const WEB_SEARCH_TOOL_NAME: &str = "web_search";
pub(crate) const WEB_SEARCH_QUERY_MAX_LENGTH: usize = 200;
const WEB_SEARCH_MAX_RESULTS_LIMIT: u8 = 10;

/// 服务端显式触发联网搜索 Tool 时使用的 typed request。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchToolRequest {
    pub query: String,
    pub raw_question: Option<String>,
    pub max_results: Option<u8>,
    pub context_size: Option<String>,
    pub model_override: Option<String>,
}

/// 模型可调用的联网搜索 Tool。
#[derive(Clone)]
pub struct WebSearchTool {
    executor: DynQueryExecutor,
}

impl WebSearchTool {
    pub fn new(executor: DynQueryExecutor) -> Self {
        Self { executor }
    }

    pub async fn query(&self, req: WebSearchToolRequest) -> Result<QueryOutcome, LlmError> {
        self.executor.query(query_request(req)).await
    }

    pub async fn query_stream(
        &self,
        req: WebSearchToolRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<QueryOutcome, LlmError> {
        self.executor
            .query_stream(query_request(req), delta_tx)
            .await
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: WEB_SEARCH_TOOL_NAME.to_owned(),
            description: "联网查询和搜索公开网页信息。用于回答需要实时信息、新闻、网页资料、最新版本、公开资料核实的问题；不用于查询本地待办、天气、火车时刻或 RSS 本地记录。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "要搜索的关键词或问题，保持具体明确"
                    },
                    "raw_question": {
                        "type": ["string", "null"],
                        "description": "用户原始问题；不确定时传 null"
                    },
                    "max_results": {
                        "type": ["integer", "null"],
                        "description": "期望返回的搜索结果数量，1 到 10；不确定时传 null"
                    },
                    "context_size": {
                        "type": ["string", "null"],
                        "description": "搜索上下文大小，可选 low、medium、high；不确定时传 null",
                        "enum": ["low", "medium", "high", null]
                    }
                },
                "required": ["query", "raw_question", "max_results", "context_size"],
                "additionalProperties": false
            }),
        }
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let outcome = self
            .query(request_from_arguments(&context, &arguments)?)
            .await?;
        Ok(ToolOutput::json(web_search_tool_output(&outcome)))
    }
}

fn request_from_arguments(
    context: &ToolContext,
    arguments: &Value,
) -> Result<WebSearchToolRequest, LlmError> {
    // 搜索模型路由只允许 `/查` 等服务端直接执行入口注入；模型 Tool Loop 调用
    // 会带稳定 tool_call_id，此时忽略任何伪造的 model_override 参数。
    let model_override = if context.tool_call_id.is_none() {
        optional_string_field(arguments, "model_override")
    } else {
        None
    };
    Ok(WebSearchToolRequest {
        query: parse_query(arguments)?,
        raw_question: optional_string_field(arguments, "raw_question"),
        max_results: parse_max_results(arguments.get("max_results"))?,
        context_size: parse_context_size(arguments.get("context_size"))?,
        model_override,
    })
}

fn query_request(req: WebSearchToolRequest) -> QueryRequest {
    QueryRequest {
        query: req.query,
        raw_question: req.raw_question,
        max_results: req.max_results,
        context_size: req.context_size,
        model_override: req.model_override,
    }
}

fn parse_query(arguments: &Value) -> Result<String, LlmError> {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            LlmError::new(
                "bad_tool_arguments",
                "web_search requires non-empty query",
                "tool",
            )
        })?;
    if query.chars().count() > WEB_SEARCH_QUERY_MAX_LENGTH {
        return Err(LlmError::new(
            "bad_tool_arguments",
            "query is too long",
            "tool",
        ));
    }
    Ok(query.to_owned())
}

fn parse_max_results(value: Option<&Value>) -> Result<Option<u8>, LlmError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) if !number.is_f64() => match number.as_u64() {
            Some(value) if (1..=WEB_SEARCH_MAX_RESULTS_LIMIT as u64).contains(&value) => {
                Ok(Some(value as u8))
            }
            _ => reject_invalid_max_results(),
        },
        _ => reject_invalid_max_results(),
    }
}

fn reject_invalid_max_results() -> Result<Option<u8>, LlmError> {
    tracing::warn!(
        tool = WEB_SEARCH_TOOL_NAME,
        error_code = "bad_tool_arguments",
        argument = "max_results",
        "invalid web search max_results argument rejected",
    );
    Err(LlmError::new(
        "bad_tool_arguments",
        "max_results must be an integer between 1 and 10 or null",
        "tool",
    ))
}

fn parse_context_size(value: Option<&Value>) -> Result<Option<String>, LlmError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim();
            if matches!(text, "low" | "medium" | "high") {
                Ok(Some(text.to_owned()))
            } else {
                reject_invalid_context_size()
            }
        }
        _ => reject_invalid_context_size(),
    }
}

fn reject_invalid_context_size() -> Result<Option<String>, LlmError> {
    tracing::warn!(
        tool = WEB_SEARCH_TOOL_NAME,
        error_code = "bad_tool_arguments",
        argument = "context_size",
        "invalid web search context_size argument rejected",
    );
    Err(LlmError::new(
        "bad_tool_arguments",
        "context_size must be low, medium, high, or null",
        "tool",
    ))
}

fn optional_string_field(arguments: &Value, key: &str) -> Option<String> {
    match arguments.get(key) {
        Some(Value::String(value)) => {
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        }
        _ => None,
    }
}

fn web_search_tool_output(outcome: &QueryOutcome) -> Value {
    json!({
        "provider": outcome.provider,
        "answer": outcome.answer,
        "sources": outcome.sources.iter().map(web_search_source_json).collect::<Vec<_>>(),
        "elapsed_ms": outcome.elapsed_ms,
    })
}

fn web_search_source_json(source: &QuerySource) -> Value {
    json!({
        "title": source.title,
        "url": source.url,
        "snippet": source.snippet,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use crate::runtime::query::QueryExecutor;

    use super::*;

    #[derive(Clone, Default)]
    struct MockQueryExecutor {
        requests: Arc<Mutex<Vec<QueryRequest>>>,
    }

    #[async_trait]
    impl QueryExecutor for MockQueryExecutor {
        async fn query(&self, req: QueryRequest) -> Result<QueryOutcome, LlmError> {
            self.requests.lock().unwrap().push(req.clone());
            Ok(QueryOutcome {
                answer: format!("answer: {}", req.query),
                sources: vec![QuerySource {
                    title: "source title".to_owned(),
                    url: "https://example.com".to_owned(),
                    snippet: "source snippet".to_owned(),
                }],
                provider: "mock-query".to_owned(),
                elapsed_ms: 12,
            })
        }

        fn provider_name(&self) -> &'static str {
            "mock-query"
        }
    }

    fn test_context() -> ToolContext {
        ToolContext {
            task_id: "task-1".to_owned(),
            user_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            tool_call_id: None,
        }
    }

    #[tokio::test]
    async fn web_search_tool_reuses_query_executor() {
        let executor = MockQueryExecutor::default();
        let requests = executor.requests.clone();
        let tool = WebSearchTool::new(Arc::new(executor));

        let output = tool
            .execute(
                test_context(),
                json!({
                    "query": "Rust 新闻",
                    "raw_question": "/查 Rust 新闻",
                    "max_results": 3,
                    "context_size": "medium",
                    "model_override": "gpt-search",
                }),
            )
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].query, "Rust 新闻");
        assert_eq!(requests[0].raw_question.as_deref(), Some("/查 Rust 新闻"));
        assert_eq!(requests[0].max_results, Some(3));
        assert_eq!(requests[0].context_size.as_deref(), Some("medium"));
        assert_eq!(requests[0].model_override.as_deref(), Some("gpt-search"));
        assert_eq!(output.value["answer"], "answer: Rust 新闻");
        assert_eq!(output.value["sources"][0]["url"], "https://example.com");
    }

    #[tokio::test]
    async fn web_search_tool_rejects_empty_query_without_calling_executor() {
        let executor = MockQueryExecutor::default();
        let requests = executor.requests.clone();
        let tool = WebSearchTool::new(Arc::new(executor));

        let err = tool
            .execute(
                test_context(),
                json!({
                    "query": " ",
                    "raw_question": null,
                    "max_results": null,
                    "context_size": null,
                    "model_override": null,
                }),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, "bad_tool_arguments");
        assert_eq!(requests.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn web_search_tool_rejects_invalid_options() {
        let tool = WebSearchTool::new(Arc::new(MockQueryExecutor::default()));

        let err = tool
            .execute(
                test_context(),
                json!({
                    "query": "Rust",
                    "raw_question": null,
                    "max_results": 99,
                    "context_size": null,
                    "model_override": null,
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "bad_tool_arguments");

        let err = tool
            .execute(
                test_context(),
                json!({
                    "query": "Rust",
                    "raw_question": null,
                    "max_results": null,
                    "context_size": "huge",
                    "model_override": null,
                }),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "bad_tool_arguments");
    }
}

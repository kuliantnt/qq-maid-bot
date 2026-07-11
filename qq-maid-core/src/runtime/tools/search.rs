//! 联网搜索 Tool。
//!
//! 该 Tool 复用 `qq-maid-llm` 的 WebSearchExecutor，把 OpenAI Responses web_search 能力纳入
//! 服务端白名单 ToolRegistry。`/查` 只作为显式触发入口，仍在 respond/search_flow.rs
//! 负责参数兼容、session 记录和用户可见错误文案。

use std::{future::Future, pin::Pin, time::Duration};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{sync::mpsc, time::timeout};

#[cfg(test)]
use qq_maid_common::identity_context::{
    ConversationKind, ExecutionActorContext, ExecutionConversationContext,
};
use qq_maid_llm::{
    tool::{Tool, ToolContext, ToolMetadata, ToolOutput, ToolTimeoutPolicy},
    web_search::{DynWebSearchExecutor, WebSearchOutcome, WebSearchRequest, WebSearchSource},
};

use crate::{config::DEFAULT_REQUEST_TIMEOUT_SECONDS, error::LlmError};

pub(crate) const WEB_SEARCH_TOOL_NAME: &str = "web_search";
pub(crate) const WEB_SEARCH_QUERY_MAX_LENGTH: usize = 200;
const WEB_SEARCH_MAX_RESULTS_LIMIT: u8 = 10;

pub(crate) mod route {
    //! 联网搜索普通消息 Agent Chat 路由判断。
    //!
    //! 本模块只表达 Search 域的显式搜索词；本地文本整理的排除规则由 respond
    //! 通用 status_semantics 先行判断后传入，避免 Search 域依赖 respond。

    pub(crate) fn has_search_intent(
        text: &str,
        lower: &str,
        local_text_processing_intent: bool,
    ) -> bool {
        if local_text_processing_intent {
            return false;
        }

        lower.contains("search")
            || has_explicit_search_phrase(text)
            || contains_any(
                text,
                &[
                    "联网",
                    "上网查",
                    "网上查",
                    "网络查询",
                    "搜索",
                    "搜一下",
                    "网上有没有",
                    "查 GitHub",
                    "查 github",
                    "查资料",
                    "查新闻",
                    "最新的",
                    "最新消息",
                    "最新进展",
                ],
            )
    }

    fn has_explicit_search_phrase(text: &str) -> bool {
        contains_any(text, &["查一下", "查下", "查查", "查询一下"])
            && contains_any(
                text,
                &[
                    "新闻",
                    "资料",
                    "网上",
                    "网络",
                    "互联网",
                    "GitHub",
                    "github",
                    "最新",
                    "进展",
                    "有没有",
                ],
            )
    }

    fn contains_any(text: &str, needles: &[&str]) -> bool {
        needles.iter().any(|needle| text.contains(needle))
    }
}

pub(crate) type WebSearchDeltaHandler<'a> = Box<
    dyn FnMut(String) -> Pin<Box<dyn Future<Output = Result<(), LlmError>> + Send>> + Send + 'a,
>;

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
    executor: DynWebSearchExecutor,
    first_activity_timeout: Duration,
}

impl WebSearchTool {
    pub fn new(executor: DynWebSearchExecutor) -> Self {
        Self {
            executor,
            first_activity_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECONDS),
        }
    }

    /// Agent 搜索首个非空增量沿用请求级超时，不使用通用 Tool 的 15 秒绝对超时。
    pub fn with_first_activity_timeout(mut self, timeout: Duration) -> Self {
        self.first_activity_timeout = timeout;
        self
    }

    pub async fn query(&self, req: WebSearchToolRequest) -> Result<WebSearchOutcome, LlmError> {
        self.executor.query(web_search_request(req)).await
    }

    pub async fn query_stream(
        &self,
        req: WebSearchToolRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        self.executor
            .query_stream(web_search_request(req), delta_tx)
            .await
    }

    pub async fn query_stream_with_handler(
        &self,
        req: WebSearchToolRequest,
        on_delta: Option<WebSearchDeltaHandler<'_>>,
    ) -> Result<WebSearchOutcome, LlmError> {
        let (delta_tx, mut delta_rx) = mpsc::channel(16);
        let tool = self.clone();
        let query_task = tokio::spawn(async move { tool.query_stream(req, delta_tx).await });
        let mut on_delta = on_delta;
        while let Some(delta) = delta_rx.recv().await {
            if !delta.is_empty()
                && let Some(handler) = on_delta.as_mut()
                && let Err(err) = handler(delta).await
            {
                query_task.abort();
                return Err(err);
            }
        }
        query_task.await.map_err(|err| {
            LlmError::provider(format!("web search stream task failed: {err}"), "internal")
        })?
    }

    async fn query_stream_for_agent(
        &self,
        req: WebSearchToolRequest,
    ) -> Result<WebSearchOutcome, LlmError> {
        let (delta_tx, mut delta_rx) = mpsc::channel(16);
        let query = self.query_stream(req, delta_tx);
        tokio::pin!(query);

        let first_event = timeout(self.first_activity_timeout, async {
            loop {
                tokio::select! {
                    result = &mut query => return Some(result),
                    delta = delta_rx.recv() => match delta {
                        Some(delta) if !delta.is_empty() => return None,
                        Some(_) => {}
                        None => return Some(query.as_mut().await),
                    }
                }
            }
        })
        .await
        .map_err(|_| LlmError::new("timeout", "web search first activity timed out", "tool"))?;
        if let Some(result) = first_event {
            return result;
        }

        // 首字之后继续排空 SSE 增量，避免发送端背压；搜索 provider 与 Core 的请求级
        // 超时继续负责最终兜底，不再套通用 Tool 的绝对 15 秒限制。
        loop {
            tokio::select! {
                result = &mut query => return result,
                delta = delta_rx.recv() => {
                    if delta.is_none() {
                        return query.as_mut().await;
                    }
                }
            }
        }
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

    fn timeout_policy(&self) -> ToolTimeoutPolicy {
        ToolTimeoutPolicy::ToolManaged
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let outcome = self
            // Agent 最终回复仍由模型统一生成，但搜索上游必须复用 `/查` 的 SSE 路径，
            // 不能因进入 Tool Loop 退化成完整非流请求。
            .query_stream_for_agent(request_from_arguments(&context, &arguments)?)
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

fn web_search_request(req: WebSearchToolRequest) -> WebSearchRequest {
    WebSearchRequest {
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

fn web_search_tool_output(outcome: &WebSearchOutcome) -> Value {
    json!({
        "provider": outcome.provider,
        "answer": outcome.answer,
        "sources": outcome.sources.iter().map(web_search_source_json).collect::<Vec<_>>(),
        "elapsed_ms": outcome.elapsed_ms,
    })
}

fn web_search_source_json(source: &WebSearchSource) -> Value {
    json!({
        "title": source.title,
        "url": source.url,
        "snippet": source.snippet,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;

    use qq_maid_llm::{
        tool::{DEFAULT_TOOL_OUTPUT_MAX_CHARS, ToolRegistry},
        web_search::WebSearchExecutor,
    };

    use super::*;

    #[derive(Clone, Default)]
    struct MockWebSearchExecutor {
        requests: Arc<Mutex<Vec<WebSearchRequest>>>,
        stream_calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl WebSearchExecutor for MockWebSearchExecutor {
        async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
            self.requests.lock().unwrap().push(req.clone());
            Ok(WebSearchOutcome {
                answer: format!("answer: {}", req.query),
                sources: vec![WebSearchSource {
                    title: "source title".to_owned(),
                    url: "https://example.com".to_owned(),
                    snippet: "source snippet".to_owned(),
                }],
                provider: "mock-query".to_owned(),
                elapsed_ms: 12,
            })
        }

        async fn query_stream(
            &self,
            req: WebSearchRequest,
            delta_tx: mpsc::Sender<String>,
        ) -> Result<WebSearchOutcome, LlmError> {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            let outcome = self.query(req).await?;
            let _ = delta_tx.send(outcome.answer.clone()).await;
            Ok(outcome)
        }

        fn provider_name(&self) -> &'static str {
            "mock-query"
        }
    }

    fn test_context() -> ToolContext {
        ToolContext {
            task_id: "task-1".to_owned(),
            actor: ExecutionActorContext {
                user_id: Some("u1".to_owned()),
                group_member_role: None,
            },
            conversation: ExecutionConversationContext {
                platform: "test".to_owned(),
                account_id: None,
                kind: ConversationKind::Private,
                target_id: Some("u1".to_owned()),
                scope_id: "private:u1".to_owned(),
                interaction_scope_id: "private:u1".to_owned(),
            },
            tool_call_id: None,
        }
    }

    #[tokio::test]
    async fn web_search_tool_reuses_query_executor() {
        let executor = MockWebSearchExecutor::default();
        let requests = executor.requests.clone();
        let stream_calls = executor.stream_calls.clone();
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
        assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(output.value["answer"], "answer: Rust 新闻");
        assert_eq!(output.value["sources"][0]["url"], "https://example.com");
    }

    struct DelayedStreamExecutor {
        first_delta_delay: Duration,
        completion_delay: Duration,
    }

    #[async_trait]
    impl WebSearchExecutor for DelayedStreamExecutor {
        async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
            Err(LlmError::provider(
                "agent web search must use streaming",
                "test",
            ))
        }

        async fn query_stream(
            &self,
            req: WebSearchRequest,
            delta_tx: mpsc::Sender<String>,
        ) -> Result<WebSearchOutcome, LlmError> {
            tokio::time::sleep(self.first_delta_delay).await;
            let _ = delta_tx.send("首字".to_owned()).await;
            tokio::time::sleep(self.completion_delay).await;
            Ok(WebSearchOutcome {
                answer: format!("answer: {}", req.query),
                sources: Vec::new(),
                provider: "delayed-stream".to_owned(),
                elapsed_ms: 0,
            })
        }

        fn provider_name(&self) -> &'static str {
            "delayed-stream"
        }
    }

    fn agent_search_arguments() -> &'static str {
        r#"{"query":"台风巴威","raw_question":"台风到哪里了","max_results":null,"context_size":null}"#
    }

    #[tokio::test]
    async fn agent_web_search_times_out_only_before_first_activity() {
        let tool = WebSearchTool::new(Arc::new(DelayedStreamExecutor {
            first_delta_delay: Duration::from_millis(5),
            completion_delay: Duration::from_millis(30),
        }))
        .with_first_activity_timeout(Duration::from_millis(10));
        let registry = ToolRegistry::new()
            .with_limits(Duration::from_millis(10), DEFAULT_TOOL_OUTPUT_MAX_CHARS)
            .register(tool)
            .unwrap();

        let output = registry
            .execute_json(
                &test_context(),
                WEB_SEARCH_TOOL_NAME,
                agent_search_arguments(),
            )
            .await
            .unwrap();

        assert!(output.contains("answer: 台风巴威"));
    }

    #[tokio::test]
    async fn agent_web_search_rejects_missing_first_activity() {
        let tool = WebSearchTool::new(Arc::new(DelayedStreamExecutor {
            first_delta_delay: Duration::from_millis(30),
            completion_delay: Duration::ZERO,
        }))
        .with_first_activity_timeout(Duration::from_millis(10));
        let registry = ToolRegistry::new().register(tool).unwrap();

        let err = registry
            .execute_json(
                &test_context(),
                WEB_SEARCH_TOOL_NAME,
                agent_search_arguments(),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, "timeout");
        assert_eq!(err.message, "web search first activity timed out");
    }

    #[tokio::test]
    async fn web_search_tool_rejects_empty_query_without_calling_executor() {
        let executor = MockWebSearchExecutor::default();
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
    async fn web_search_tool_rejects_overlong_query_without_calling_executor() {
        let executor = MockWebSearchExecutor::default();
        let requests = executor.requests.clone();
        let tool = WebSearchTool::new(Arc::new(executor));

        let err = tool
            .execute(
                test_context(),
                json!({
                    "query": "a".repeat(WEB_SEARCH_QUERY_MAX_LENGTH + 1),
                    "raw_question": null,
                    "max_results": null,
                    "context_size": null,
                    "model_override": null,
                }),
            )
            .await
            .unwrap_err();

        assert_eq!(err.code, "bad_tool_arguments");
        assert_eq!(err.message, "query is too long");
        assert_eq!(requests.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn web_search_tool_rejects_invalid_options() {
        let tool = WebSearchTool::new(Arc::new(MockWebSearchExecutor::default()));

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

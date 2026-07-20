//! 联网搜索 Tool。
//!
//! 该 Tool 复用 `qq-maid-llm` 的统一 WebSearchExecutor，把 Provider 原生搜索与 Tavily
//! 纳入服务端白名单 ToolRegistry。`/查` 只作为显式触发入口，仍在 respond/search_flow.rs
//! 负责参数兼容、session 记录和用户可见错误文案。

use std::{future::Future, pin::Pin, time::Duration};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep_until},
};

#[cfg(test)]
use qq_maid_common::identity_context::{
    ConversationKind, ExecutionActorContext, ExecutionConversationContext,
};
use qq_maid_llm::{
    tool::{Tool, ToolContext, ToolEffect, ToolMetadata, ToolOutput, ToolTimeoutPolicy},
    web_search::{
        DEFAULT_MAX_RESULTS, DynWebSearchExecutor, WebSearchBackend, WebSearchOutcome,
        WebSearchRequest, WebSearchSource,
    },
};

use crate::{
    config::{
        DEFAULT_WEB_SEARCH_ABSOLUTE_TIMEOUT_SECONDS,
        DEFAULT_WEB_SEARCH_FIRST_ACTIVITY_TIMEOUT_SECONDS, DEFAULT_WEB_SEARCH_IDLE_TIMEOUT_SECONDS,
    },
    error::LlmError,
};

pub(crate) const WEB_SEARCH_TOOL_NAME: &str = "web_search";
pub(crate) const WEB_SEARCH_QUERY_MAX_LENGTH: usize = 200;
const WEB_SEARCH_MAX_RESULTS_LIMIT: u8 = 10;
/// 搜索流三段超时的默认值；绝对上限独立于 90 秒整体请求预算。
pub const DEFAULT_WEB_SEARCH_FIRST_ACTIVITY_TIMEOUT: Duration =
    Duration::from_secs(DEFAULT_WEB_SEARCH_FIRST_ACTIVITY_TIMEOUT_SECONDS);
pub const DEFAULT_WEB_SEARCH_IDLE_TIMEOUT: Duration =
    Duration::from_secs(DEFAULT_WEB_SEARCH_IDLE_TIMEOUT_SECONDS);
pub const DEFAULT_WEB_SEARCH_ABSOLUTE_TIMEOUT: Duration =
    Duration::from_secs(DEFAULT_WEB_SEARCH_ABSOLUTE_TIMEOUT_SECONDS);

/// 联网搜索流的统一超时配置，Agent Tool 与显式 `/查` 共用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSearchTimeouts {
    pub first_activity: Duration,
    pub idle: Duration,
    pub absolute: Duration,
}

impl Default for WebSearchTimeouts {
    fn default() -> Self {
        Self {
            first_activity: DEFAULT_WEB_SEARCH_FIRST_ACTIVITY_TIMEOUT,
            idle: DEFAULT_WEB_SEARCH_IDLE_TIMEOUT,
            absolute: DEFAULT_WEB_SEARCH_ABSOLUTE_TIMEOUT,
        }
    }
}

mod ops;

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
    pub topic: Option<String>,
    pub time_range: Option<String>,
    pub backend_override: Option<WebSearchBackend>,
    pub model_override: Option<String>,
}

/// 模型可调用的联网搜索 Tool。
#[derive(Clone)]
pub struct WebSearchTool {
    executor: DynWebSearchExecutor,
    first_activity_timeout: Duration,
    idle_timeout: Duration,
    absolute_timeout: Duration,
    backend_override: Option<WebSearchBackend>,
    model_override: Option<String>,
}

impl WebSearchTool {
    pub fn new(executor: DynWebSearchExecutor) -> Self {
        let timeouts = WebSearchTimeouts::default();
        Self {
            executor,
            first_activity_timeout: timeouts.first_activity,
            idle_timeout: timeouts.idle,
            absolute_timeout: timeouts.absolute,
            backend_override: None,
            model_override: None,
        }
    }

    /// 覆盖统一搜索流超时；每一项仍会在运行时裁剪到 Agent 工具 deadline。
    pub fn with_timeouts(mut self, timeouts: WebSearchTimeouts) -> Self {
        self.first_activity_timeout = timeouts.first_activity;
        self.idle_timeout = timeouts.idle;
        self.absolute_timeout = timeouts.absolute;
        self
    }

    /// 自然语言 Tool Loop 必须使用服务端解析后的场景搜索路线，模型参数不能覆盖。
    pub fn with_model_override(mut self, model: String) -> Self {
        self.model_override = Some(model);
        self
    }

    /// 自然语言 Tool Loop 和显式 `/查` 只能使用服务端解析后的后端，模型参数不能覆盖。
    pub fn with_backend_override(mut self, backend: WebSearchBackend) -> Self {
        self.backend_override = Some(backend);
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
        self.query_stream_with_timeouts(req, None, on_delta).await
    }

    async fn query_stream_for_agent(
        &self,
        req: WebSearchToolRequest,
        execution_deadline: Option<Instant>,
    ) -> Result<WebSearchOutcome, LlmError> {
        self.query_stream_with_timeouts(req, execution_deadline, None)
            .await
    }

    async fn query_stream_with_timeouts(
        &self,
        req: WebSearchToolRequest,
        execution_deadline: Option<Instant>,
        mut on_delta: Option<WebSearchDeltaHandler<'_>>,
    ) -> Result<WebSearchOutcome, LlmError> {
        let (delta_tx, mut delta_rx) = mpsc::channel(16);
        let query = self.query_stream(req, delta_tx);
        tokio::pin!(query);
        let now = Instant::now();
        let configured_deadline = now + self.absolute_timeout;
        let absolute_deadline = execution_deadline
            .map(|deadline| std::cmp::min(deadline, configured_deadline))
            .unwrap_or(configured_deadline);
        if absolute_deadline <= now {
            return Err(web_search_timeout_error(
                "budget",
                "web search has no execution budget before final answer reserve",
            ));
        }
        let absolute_sleep = sleep_until(absolute_deadline);
        tokio::pin!(absolute_sleep);
        let activity_sleep = sleep_until(std::cmp::min(
            now + self.first_activity_timeout,
            absolute_deadline,
        ));
        tokio::pin!(activity_sleep);
        let mut saw_activity = false;
        let mut delta_open = true;
        let mut query_result = None;

        // 同时维护首活动、首活动后静默与绝对时长三条边界。非空 delta 才算活动，
        // 避免上游用空帧或 keepalive 无限延长搜索。
        loop {
            tokio::select! {
                result = &mut query, if query_result.is_none() => {
                    query_result = Some(result);
                    if !delta_open {
                        return query_result.expect("query result just recorded");
                    }
                }
                delta = delta_rx.recv(), if delta_open => {
                    match delta {
                        Some(delta) if !delta.is_empty() => {
                            saw_activity = true;
                            activity_sleep.as_mut().reset(std::cmp::min(
                                Instant::now() + self.idle_timeout,
                                absolute_deadline,
                            ));
                            if let Some(handler) = on_delta.as_mut() {
                                let handler_result = handler(delta);
                                tokio::pin!(handler_result);
                                tokio::select! {
                                    result = &mut handler_result => result?,
                                    _ = sleep_until(absolute_deadline) => {
                                        return Err(web_search_timeout_error(
                                            "absolute",
                                            "web search absolute timeout exceeded",
                                        ));
                                    }
                                }
                            }
                        }
                        Some(_) => {}
                        None => {
                            delta_open = false;
                            if let Some(result) = query_result.take() {
                                return result;
                            }
                        }
                    }
                }
                _ = &mut absolute_sleep => {
                    return Err(web_search_timeout_error(
                        "absolute",
                        "web search absolute timeout exceeded",
                    ));
                }
                _ = &mut activity_sleep => {
                    if Instant::now() >= absolute_deadline {
                        return Err(web_search_timeout_error(
                            "absolute",
                            "web search absolute timeout exceeded",
                        ));
                    }
                    let (phase, message) = if saw_activity {
                        ("idle", "web search became idle after first activity")
                    } else {
                        ("first_activity", "web search first activity timed out")
                    };
                    return Err(web_search_timeout_error(phase, message));
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
            description: "联网查询和搜索公开网页信息。用于回答需要实时信息、新闻、网页资料、最新版本、公开资料核实的问题；不用于查询本地待办、天气、火车时刻或 RSS 本地记录。调用前必须结合当前会话、引用消息、机器人身份和本地记忆补全省略的搜索主体，使 query 脱离聊天上下文后仍可独立理解；能够确定具体对象时，不要先搜索泛化问题。简单单实体问题使用 query；多实体对比或调研必须由你识别实体和统一比较维度，使用 research_targets 为每个实体提供独立 query，不要拼成一次长搜索。每项搜索只调查该实体的事实、来源和不确定项，跨实体对比与推荐留到工具返回后统一生成。名称有歧义时在 assumption 中保留消歧假设，确实无法合理判断时再向用户澄清。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": ["string", "null"],
                        "description": "单实体模式的搜索关键词或问题；多实体模式传 null"
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
                    },
                    "topic": {
                        "type": ["string", "null"],
                        "description": "搜索主题，可选 general、news、finance；不确定时传 null",
                        "enum": ["general", "news", "finance", null]
                    },
                    "time_range": {
                        "type": ["string", "null"],
                        "description": "相对时间范围，可选 day、week、month、year；不确定时传 null",
                        "enum": ["day", "week", "month", "year", null]
                    },
                    "comparison_dimensions": {
                        "type": ["array", "null"],
                        "description": "多实体模式下统一比较维度；单实体模式传 null",
                        "items": {"type": "string"},
                        "maxItems": 8
                    },
                    "research_targets": {
                        "type": ["array", "null"],
                        "description": "多实体调研任务，必须每个实体独立一项；单实体模式传 null",
                        "items": {
                            "type": "object",
                            "properties": {
                                "entity": {"type": "string", "description": "规范实体名称"},
                                "query": {"type": "string", "description": "只调查该实体且可独立理解的 query"},
                                "assumption": {"type": ["string", "null"], "description": "名称消歧假设；无歧义传 null"}
                            },
                            "required": ["entity", "query", "assumption"],
                            "additionalProperties": false
                        },
                        "minItems": 2,
                        "maxItems": ops::WEB_SEARCH_RESEARCH_MAX_TARGETS
                    }
                },
                "required": ["query", "raw_question", "max_results", "context_size", "topic", "time_range", "comparison_dimensions", "research_targets"],
                "additionalProperties": false
            }),
        }
    }

    fn timeout_policy(&self) -> ToolTimeoutPolicy {
        ToolTimeoutPolicy::ToolManaged
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn deduplication_key(&self, arguments: &Value) -> Option<String> {
        if let Ok(Some(targets)) = ops::parse_research_targets(arguments.get("research_targets")) {
            return serde_json::to_string(&json!({
                "research_targets": targets.iter().map(|target| json!({
                    "entity": normalize_dedup_text(&target.entity),
                    "query": normalize_dedup_text(&target.query),
                    "assumption": target.assumption.as_deref().map(normalize_dedup_text),
                })).collect::<Vec<_>>(),
                "comparison_dimensions": ops::parse_comparison_dimensions(
                    arguments.get("comparison_dimensions")
                ).ok()?,
                "max_results": parse_max_results(arguments.get("max_results")).ok()?,
                "context_size": parse_context_size(arguments.get("context_size")).ok()?,
                "topic": parse_topic(arguments.get("topic")).ok()?,
                "time_range": parse_time_range(arguments.get("time_range")).ok()?,
            }))
            .ok();
        }
        let query = parse_query(arguments).ok()?;
        let raw_question = optional_string_field(arguments, "raw_question");
        let max_results = parse_max_results(arguments.get("max_results")).ok()?;
        let context_size = parse_context_size(arguments.get("context_size")).ok()?;
        let topic = parse_topic(arguments.get("topic")).ok()?;
        let time_range = parse_time_range(arguments.get("time_range")).ok()?;
        let normalized_query = normalize_dedup_text(&query);
        (!normalized_query.is_empty()).then(|| {
            serde_json::to_string(&json!({
                "query": normalized_query,
                // raw_question 会进入搜索提示词；缺省时实际语义等价于 query。
                "raw_question": normalize_dedup_text(
                    raw_question.as_deref().unwrap_or(&query)
                ),
                "max_results": max_results.unwrap_or(DEFAULT_MAX_RESULTS),
                "context_size": context_size.as_deref().unwrap_or("low"),
                "topic": topic.as_deref().unwrap_or("general"),
                "time_range": time_range,
            }))
            .expect("web search deduplication key must serialize")
        })
    }

    async fn execute(
        &self,
        context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        if let Some(targets) = ops::parse_research_targets(arguments.get("research_targets"))? {
            return ops::execute_research(self, &context, &arguments, targets).await;
        }
        let outcome = self
            // Agent 最终回复仍由模型统一生成，但搜索上游必须复用 `/查` 的 SSE 路径，
            // 不能因进入 Tool Loop 退化成完整非流请求。
            .query_stream_for_agent(
                request_from_arguments(
                    &context,
                    &arguments,
                    self.backend_override,
                    self.model_override.clone(),
                )?,
                context.execution_deadline,
            )
            .await?;
        Ok(ToolOutput::json(web_search_tool_output(&outcome)))
    }
}

fn normalize_dedup_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn request_from_arguments(
    context: &ToolContext,
    arguments: &Value,
    server_backend_override: Option<WebSearchBackend>,
    server_model_override: Option<String>,
) -> Result<WebSearchToolRequest, LlmError> {
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

fn web_search_timeout_error(phase: &str, message: &str) -> LlmError {
    LlmError::new("timeout", message, format!("web_search_{phase}"))
}

fn web_search_request(req: WebSearchToolRequest) -> WebSearchRequest {
    WebSearchRequest {
        query: req.query,
        raw_question: req.raw_question,
        max_results: req.max_results,
        context_size: req.context_size,
        topic: req.topic,
        time_range: req.time_range,
        backend_override: req.backend_override,
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

fn parse_topic(value: Option<&Value>) -> Result<Option<String>, LlmError> {
    parse_optional_enum(value, "topic", &["general", "news", "finance"])
}

fn parse_time_range(value: Option<&Value>) -> Result<Option<String>, LlmError> {
    parse_optional_enum(value, "time_range", &["day", "week", "month", "year"])
}

fn parse_optional_enum(
    value: Option<&Value>,
    name: &str,
    allowed: &[&str],
) -> Result<Option<String>, LlmError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim().to_ascii_lowercase();
            if allowed.contains(&text.as_str()) {
                Ok(Some(text))
            } else {
                Err(LlmError::new(
                    "bad_tool_arguments",
                    format!("{name} must be one of {} or null", allowed.join(", ")),
                    "tool",
                ))
            }
        }
        _ => Err(LlmError::new(
            "bad_tool_arguments",
            format!("{name} must be a string or null"),
            "tool",
        )),
    }
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
mod tests;

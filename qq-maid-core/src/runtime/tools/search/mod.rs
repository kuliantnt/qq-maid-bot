//! 联网搜索 Tool。
//!
//! 该 Tool 复用 `qq-maid-llm` 的统一 WebSearchExecutor，把 Provider 原生搜索与 Tavily
//! 纳入服务端白名单 ToolRegistry。`/查` 只作为显式触发入口，仍在 respond/search_flow/mod.rs
//! 负责参数兼容、session 记录和用户可见错误文案。

use std::{future::Future, pin::Pin, time::Duration};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep_until},
};

use qq_maid_llm::{
    tool::{
        DEFAULT_TOOL_OUTPUT_MAX_CHARS, Tool, ToolContext, ToolEffect, ToolMetadata, ToolOutput,
        ToolTimeoutPolicy,
    },
    web_search::{DynWebSearchExecutor, WebSearchBackend, WebSearchOutcome, WebSearchRequest},
};

use crate::{
    config::{
        DEFAULT_WEB_SEARCH_ABSOLUTE_TIMEOUT_SECONDS,
        DEFAULT_WEB_SEARCH_FIRST_ACTIVITY_TIMEOUT_SECONDS, DEFAULT_WEB_SEARCH_IDLE_TIMEOUT_SECONDS,
    },
    error::LlmError,
};

pub(crate) const WEB_SEARCH_TOOL_NAME: &str = "web_search";
pub(super) const WEB_SEARCH_EMPTY_RESULT_MODEL_MESSAGE: &str =
    "本次搜索没有找到可用结果；请说明没有联网证据，不要把未经搜索验证的信息描述成实时搜索结果。";
pub(crate) const WEB_SEARCH_QUERY_MAX_LENGTH: usize = 200;
const WEB_SEARCH_MAX_RESULTS_LIMIT: u8 = 10;
const WEB_SEARCH_TOOL_SOURCE_LIMIT: usize = 4;
const WEB_SEARCH_TOOL_SOURCE_TITLE_MAX_CHARS: usize = 100;
const WEB_SEARCH_TOOL_SOURCE_SNIPPET_MAX_CHARS: usize = 160;
/// 联网搜索是只读操作；瞬时故障最多补发一次，仍受 Agent 工具绝对 deadline 约束。
const WEB_SEARCH_MAX_ATTEMPTS: usize = 2;
const WEB_SEARCH_RETRY_BACKOFF: Duration = Duration::from_millis(100);
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

pub(crate) mod agent_turn;
mod ops;
mod output;
pub(crate) mod status;
mod validation;

#[cfg(test)]
use output::serialized_value_chars;
use output::{web_search_failure_output, web_search_outcome_has_evidence, web_search_tool_output};
use validation::{
    WebSearchArgumentError, WebSearchToolError, normalize_dedup_text, optional_string_field,
    parse_context_size, parse_max_results, parse_query, parse_time_range, parse_topic,
    request_from_arguments,
};

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
    output_max_chars: usize,
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
            output_max_chars: DEFAULT_TOOL_OUTPUT_MAX_CHARS,
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

    /// 与 Tool Registry 使用相同结果预算，避免搜索先生成超限 JSON 后丢失结构化证据。
    pub(crate) fn with_output_max_chars(mut self, output_max_chars: usize) -> Self {
        self.output_max_chars = output_max_chars;
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

    pub(super) fn backend_label(&self) -> &'static str {
        self.backend_override
            .map(WebSearchBackend::as_str)
            .unwrap_or("configured_default")
    }

    fn normalize_max_results(
        &self,
        mut req: WebSearchToolRequest,
    ) -> (WebSearchToolRequest, Option<u8>, u8) {
        let requested = req.max_results;
        let effective = self.effective_max_results(requested);
        // None 仍交给路由器应用同一个配置上限；显式请求则在进入 executor 前完成 clamp。
        if requested.is_some() {
            req.max_results = Some(effective);
        }
        (req, requested, effective)
    }

    fn effective_max_results(&self, requested: Option<u8>) -> u8 {
        let configured_limit = self
            .executor
            .max_results_limit()
            .clamp(1, WEB_SEARCH_MAX_RESULTS_LIMIT);
        requested
            .unwrap_or(configured_limit)
            .min(configured_limit)
            .clamp(1, WEB_SEARCH_MAX_RESULTS_LIMIT)
    }

    fn handle_argument_error(
        &self,
        context: &ToolContext,
        error: WebSearchArgumentError,
    ) -> Result<ToolOutput, LlmError> {
        log_web_search_argument_error(self, context, &error);
        if context.tool_call_id.is_some() {
            return Ok(ToolOutput::json(error.agent_output(self.backend_label())));
        }
        Err(error.into_llm_error())
    }

    pub async fn query(&self, req: WebSearchToolRequest) -> Result<WebSearchOutcome, LlmError> {
        let (req, requested_max_results, effective_max_results) = self.normalize_max_results(req);
        let started = Instant::now();
        let outcome = self.executor.query(web_search_request(req.clone())).await;
        log_web_search_result(
            self,
            &req,
            requested_max_results,
            effective_max_results,
            started.elapsed(),
            &outcome,
        );
        outcome
    }

    pub async fn query_stream(
        &self,
        req: WebSearchToolRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        let (req, requested_max_results, effective_max_results) = self.normalize_max_results(req);
        let started = Instant::now();
        let outcome = self
            .executor
            .query_stream(web_search_request(req.clone()), delta_tx)
            .await;
        log_web_search_result(
            self,
            &req,
            requested_max_results,
            effective_max_results,
            started.elapsed(),
            &outcome,
        );
        outcome
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

    async fn query_stream_for_agent_with_retry(
        &self,
        req: WebSearchToolRequest,
        execution_deadline: Option<Instant>,
        context: &ToolContext,
    ) -> (Result<WebSearchOutcome, LlmError>, usize) {
        for attempt in 1..=WEB_SEARCH_MAX_ATTEMPTS {
            let started = Instant::now();
            let fallback_model = req
                .model_override
                .as_deref()
                .unwrap_or("configured_default")
                .to_owned();
            let outcome = self
                .query_stream_for_agent(req.clone(), execution_deadline)
                .await
                .map_err(|err| {
                    err.with_upstream_context(self.executor.provider_name(), fallback_model)
                });
            log_web_search_attempt(self, context, attempt, started.elapsed(), &outcome);
            let should_retry = outcome.as_ref().err().is_some_and(LlmError::retriable)
                && attempt < WEB_SEARCH_MAX_ATTEMPTS
                && execution_deadline
                    .is_none_or(|deadline| Instant::now() + WEB_SEARCH_RETRY_BACKOFF < deadline);
            if !should_retry {
                return (outcome, attempt);
            }
            tokio::time::sleep(WEB_SEARCH_RETRY_BACKOFF).await;
        }
        unreachable!("web search retry loop always returns")
    }

    async fn query_stream_with_timeouts(
        &self,
        req: WebSearchToolRequest,
        execution_deadline: Option<Instant>,
        on_delta: Option<WebSearchDeltaHandler<'_>>,
    ) -> Result<WebSearchOutcome, LlmError> {
        let (req, requested_max_results, effective_max_results) = self.normalize_max_results(req);
        let started = Instant::now();
        let outcome = self
            .query_stream_with_timeouts_inner(req.clone(), execution_deadline, on_delta)
            .await;
        log_web_search_result(
            self,
            &req,
            requested_max_results,
            effective_max_results,
            started.elapsed(),
            &outcome,
        );
        outcome
    }

    async fn query_stream_with_timeouts_inner(
        &self,
        req: WebSearchToolRequest,
        execution_deadline: Option<Instant>,
        mut on_delta: Option<WebSearchDeltaHandler<'_>>,
    ) -> Result<WebSearchOutcome, LlmError> {
        let (delta_tx, mut delta_rx) = mpsc::channel(16);
        let query = self
            .executor
            .query_stream(web_search_request(req), delta_tx);
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
                        "description": "每个底层搜索子请求期望返回的结果数量，1 到 10；多目标模式对每个 research_target 分别应用；不确定时传 null"
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

    fn cache_terminal_failures(&self) -> bool {
        // 搜索工具已在内部耗尽有限瞬时重试；其余参数、配置、认证等失败在同一
        // 请求内不会被业务写工具修复，允许阻止模型重复发起相同上游请求。
        true
    }

    fn deduplication_key(&self, arguments: &Value) -> Option<String> {
        if arguments
            .get("research_targets")
            .is_some_and(|value| !value.is_null())
        {
            let Some(targets) =
                ops::parse_research_targets(arguments.get("research_targets")).ok()?
            else {
                // 研究参数校验失败时不能退回单实体 query 的去重键，否则同一无效
                // Tool Call 会被当作终态失败缓存，模型就没有机会修正参数。
                return None;
            };
            return serde_json::to_string(&json!({
                "research_targets": targets.iter().map(|target| json!({
                    "entity": normalize_dedup_text(&target.entity),
                    "query": normalize_dedup_text(&target.query),
                    "assumption": target.assumption.as_deref().map(normalize_dedup_text),
                })).collect::<Vec<_>>(),
                "comparison_dimensions": ops::parse_comparison_dimensions(
                    arguments.get("comparison_dimensions")
                ).ok()?,
                "max_results": self.effective_max_results(
                    parse_max_results(arguments.get("max_results")).ok()?
                ),
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
                "max_results": self.effective_max_results(max_results),
                "context_size": context_size.as_deref().unwrap_or("low"),
                // Tavily 会根据未传 topic/time_range 的时效新闻请求改用 news/day。
                // 因此不能将缺省值与模型显式指定的 general 视为同一搜索。
                "topic": topic,
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
        let research_targets = match ops::parse_research_targets(arguments.get("research_targets"))
        {
            Ok(targets) => targets,
            Err(error) => return self.handle_argument_error(&context, error),
        };
        if let Some(targets) = research_targets {
            let output = match ops::execute_research(self, &context, &arguments, targets).await {
                Ok(output) => output,
                Err(WebSearchToolError::Argument(error)) => {
                    return self.handle_argument_error(&context, error);
                }
                Err(WebSearchToolError::Execution(error)) => return Err(error),
            };
            log_web_search_execution(&context, &arguments, &output.value, true);
            return Ok(output);
        }
        let request = match request_from_arguments(
            &context,
            &arguments,
            self.backend_override,
            self.model_override.clone(),
        ) {
            Ok(request) => request,
            Err(error) => return self.handle_argument_error(&context, error),
        };
        // Issue #361 诊断：联网查询前后只记录尺寸/计数与内存，不记录查询正文。
        // 进程内存采样放进 DEBUG 门控，默认级别不触碰 /proc 读取。
        if tracing::enabled!(tracing::Level::DEBUG) {
            let before_mem = qq_maid_common::process_mem::process_memory_sample();
            tracing::debug!(
                event = "before_web_search",
                tool = WEB_SEARCH_TOOL_NAME,
                query_chars = request.query.chars().count(),
                max_results = request.max_results,
                rss_kb = before_mem.rss_kb,
                vm_size_kb = before_mem.vm_size_kb,
                pss_kb = before_mem.pss_kb,
                private_dirty_kb = before_mem.private_dirty_kb,
                "执行联网搜索前的诊断"
            );
        }
        let (outcome, attempts) = self
            // Agent 最终回复仍由模型统一生成，但搜索上游必须复用 `/查` 的 SSE 路径，
            // 不能因进入 Tool Loop 退化成完整非流请求。
            .query_stream_for_agent_with_retry(request, context.execution_deadline, &context)
            .await;
        let value = match outcome {
            Ok(outcome) => {
                web_search_tool_output(&outcome, self.backend_label(), self.output_max_chars)
            }
            Err(err) => {
                let value = web_search_failure_output(self.backend_label(), attempts, &err);
                log_web_search_execution(&context, &arguments, &value, false);
                // 只有 Agent Tool Loop 需要把执行失败回填给模型，以便统一记录失败进度和
                // 保守重试；显式查询等非 Agent 兼容入口仍保留原始 Err 语义。
                if context.tool_call_id.is_some() {
                    return Ok(ToolOutput::json(value));
                }
                return Err(err);
            }
        };
        log_web_search_execution(&context, &arguments, &value, false);
        Ok(ToolOutput::json(value))
    }
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

fn log_web_search_attempt(
    tool: &WebSearchTool,
    context: &ToolContext,
    attempt: usize,
    duration: Duration,
    outcome: &Result<WebSearchOutcome, LlmError>,
) {
    let duration_ms = duration.as_millis().min(u128::from(u64::MAX)) as u64;
    match outcome {
        Ok(outcome) => tracing::info!(
            tool_name = WEB_SEARCH_TOOL_NAME,
            tool_call_id = context.tool_call_id.as_deref().unwrap_or("direct"),
            attempt,
            duration_ms,
            error_kind = "none",
            retriable = false,
            backend = tool.backend_label(),
            upstream_status = ?Option::<u16>::None,
            provider = outcome.provider.as_str(),
            model = tool.model_override.as_deref().unwrap_or("configured_default"),
            failure_layer = "none",
            "联网搜索尝试成功"
        ),
        Err(error) => tracing::warn!(
            tool_name = WEB_SEARCH_TOOL_NAME,
            tool_call_id = context.tool_call_id.as_deref().unwrap_or("direct"),
            attempt,
            duration_ms,
            error_kind = error.error_kind(),
            retriable = error.retriable(),
            backend = tool.backend_label(),
            upstream_status = ?error.upstream_status,
            provider = error
                .upstream_provider()
                .unwrap_or_else(|| tool.executor.provider_name()),
            model = error
                .upstream_model()
                .or(tool.model_override.as_deref())
                .unwrap_or("configured_default"),
            failure_layer = error.stage.as_str(),
            "联网搜索尝试失败"
        ),
    }
}

/// 每次真实搜索执行只记录安全结构化诊断，不输出 query、raw_question 或上游正文。
fn log_web_search_result(
    tool: &WebSearchTool,
    request: &WebSearchToolRequest,
    requested_max_results: Option<u8>,
    effective_max_results: u8,
    duration: Duration,
    outcome: &Result<WebSearchOutcome, LlmError>,
) {
    let elapsed_ms = duration.as_millis().min(u128::from(u64::MAX)) as u64;
    match outcome {
        Ok(outcome) => tracing::info!(
            tool_name = WEB_SEARCH_TOOL_NAME,
            backend = tool.backend_label(),
            provider = outcome.provider.as_str(),
            model = request
                .model_override
                .as_deref()
                .unwrap_or("configured_default"),
            requested_max_results = ?requested_max_results,
            effective_max_results,
            elapsed_ms,
            error_kind = "none",
            timeout_stage = "none",
            "联网搜索执行完成"
        ),
        Err(error) => tracing::warn!(
            tool_name = WEB_SEARCH_TOOL_NAME,
            backend = tool.backend_label(),
            provider = error
                .upstream_provider()
                .unwrap_or_else(|| tool.executor.provider_name()),
            model = error
                .upstream_model()
                .or(request.model_override.as_deref())
                .unwrap_or("configured_default"),
            requested_max_results = ?requested_max_results,
            effective_max_results,
            elapsed_ms,
            error_kind = error.error_kind(),
            timeout_stage = if error.code == "timeout" {
                error.stage.as_str()
            } else {
                "none"
            },
            "联网搜索执行失败"
        ),
    }
}

/// 参数校验失败发生在请求构造前，单独记录字段级诊断，避免被上游请求日志的
/// `duration_ms=0` 和通用 `invalid_arguments` 淹没。这里不记录 query/raw_question
/// 或完整参数；查询只保留字符数，受限参数才允许保留短 safe_value。
fn log_web_search_argument_error(
    tool: &WebSearchTool,
    context: &ToolContext,
    error: &WebSearchArgumentError,
) {
    tracing::warn!(
        tool = WEB_SEARCH_TOOL_NAME,
        backend = tool.backend_label(),
        error_code = "invalid_arguments",
        failure_layer = "tool",
        duration_ms = 0_u64,
        argument = error.field.as_str(),
        reason = error.reason,
        message = error.message.as_str(),
        value_kind = error.value_kind,
        safe_value = ?error.safe_value,
        query_chars = ?error.query_chars,
        task_id = context.task_id.as_str(),
        tool_call_id = context.tool_call_id.as_deref().unwrap_or("direct"),
        tool_round = ?context.tool_round,
        "联网搜索参数校验失败"
    );
}

/// 搜索诊断只保留可定位重试的结构化字段；不记录 query、raw_question、聊天历史或上游正文。
fn log_web_search_execution(
    context: &ToolContext,
    arguments: &Value,
    output: &Value,
    multi_entity_research: bool,
) {
    // 该函数只输出 DEBUG 诊断：默认级别不触碰输出正文计数、`/proc` 采样。
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let query_chars = if multi_entity_research {
        0
    } else {
        arguments
            .get("query")
            .and_then(Value::as_str)
            .map(|query| query.chars().count())
            .unwrap_or(0)
    };
    let source_count = output
        .get("sources")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_else(|| {
            output
                .get("results")
                .and_then(Value::as_array)
                .map(|results| {
                    results
                        .iter()
                        .filter_map(|result| result.get("sources").and_then(Value::as_array))
                        .map(Vec::len)
                        .sum()
                })
                .unwrap_or(0)
        });
    let execution_succeeded = output
        .get("execution_succeeded")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| output.get("ok").and_then(Value::as_bool).unwrap_or(false));
    let mem = qq_maid_common::process_mem::process_memory_sample();
    tracing::debug!(
        tool = WEB_SEARCH_TOOL_NAME,
        tool_call_id = context.tool_call_id.as_deref().unwrap_or("direct"),
        round = ?context.tool_round,
        backend = output
            .get("backend")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown"),
        query_chars,
        topic = ?arguments.get("topic"),
        time_range = ?arguments.get("time_range"),
        max_results = ?arguments.get("max_results"),
        multi_entity_research,
        answer_chars = output
            .get("answer")
            .and_then(|value| value.as_str())
            .map(|answer| answer.chars().count())
            .unwrap_or(0),
        source_count,
        result_count = output
            .get("result_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        ok = output
            .get("ok")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        execution_succeeded,
        rss_kb = mem.rss_kb,
        vm_size_kb = mem.vm_size_kb,
        pss_kb = mem.pss_kb,
        private_dirty_kb = mem.private_dirty_kb,
        error_code = output
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(|value| value.as_str())
            .unwrap_or(""),
        retry_of = ?context.retry_of,
        "联网搜索 Tool 执行完成"
    );
}

#[cfg(test)]
mod tests;

//! 联网搜索查询模块。
//!
//! 通过 OpenAI Responses API 的 `web_search` 工具执行联网搜索，
//! 返回搜索结果文本和来源列表。支持自定义搜索模型、结果数量和上下文大小。

use std::{env, sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{
    config::{AppConfig, OpenAiApiMode},
    error::LlmError,
    util::metrics::duration_ms,
    util::sse::{SseFrame, parse_sse_frame, take_sse_frame},
    util::time_context::{RequestTimeContext, request_time_context},
};

/// 默认搜索模型。
pub const DEFAULT_SEARCH_MODEL: &str = "gpt-5.5";
/// 默认搜索结果返回数量。
pub const DEFAULT_MAX_RESULTS: u8 = 5;
/// 搜索结果返回数量上限。
pub const MAX_RESULTS_LIMIT: u8 = 10;
/// 默认搜索上下文大小。
pub const DEFAULT_SEARCH_CONTEXT_SIZE: &str = "low";
/// OpenAI API 默认基础地址。
const OPENAI_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// 搜索查询请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryRequest {
    /// 搜索查询文本
    pub query: String,
    /// 用户的原始问题（用于构造给 LLM 的提示，比 query 更完整）
    #[serde(default)]
    pub raw_question: Option<String>,
    /// 期望返回的结果数量
    pub max_results: Option<u8>,
    /// 搜索上下文大小（"low"、"medium"、"high"）
    pub context_size: Option<String>,
}

/// 搜索结果的单个来源。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuerySource {
    /// 来源标题
    pub title: String,
    /// 来源 URL
    pub url: String,
    /// 摘要片段
    #[serde(default)]
    pub snippet: String,
}

/// 查询响应的 HTTP 传输结构。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryResponse {
    /// 是否成功
    pub ok: bool,
    /// 搜索结果回答文本
    pub answer: String,
    /// 来源列表
    pub sources: Vec<QuerySource>,
    /// 服务提供商名称
    pub provider: String,
    /// 耗时（毫秒）
    pub elapsed_ms: u64,
    /// 错误信息（成功时为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::error::ErrorInfo>,
}

/// 查询结果的内部表示，不包含序列化标记。
#[derive(Debug, Clone)]
pub struct QueryOutcome {
    /// 回答文本
    pub answer: String,
    /// 来源列表
    pub sources: Vec<QuerySource>,
    /// 提供商名称
    pub provider: String,
    /// 耗时（毫秒）
    pub elapsed_ms: u64,
}

impl QueryResponse {
    /// 创建成功的查询响应。
    pub fn ok(outcome: QueryOutcome) -> Self {
        Self {
            ok: true,
            answer: outcome.answer,
            sources: outcome.sources,
            provider: outcome.provider,
            elapsed_ms: outcome.elapsed_ms,
            error: None,
        }
    }

    /// 创建失败的查询响应。
    pub fn error(provider: impl Into<String>, elapsed_ms: u64, error: LlmError) -> Self {
        Self {
            ok: false,
            answer: String::new(),
            sources: Vec::new(),
            provider: provider.into(),
            elapsed_ms,
            error: Some(error.as_info()),
        }
    }
}

/// 搜索查询执行器 trait。
///
/// 不同实现可对接不同的搜索服务商。
#[async_trait]
pub trait QueryExecutor: Send + Sync {
    /// 执行搜索查询。
    async fn query(&self, req: QueryRequest) -> Result<QueryOutcome, LlmError>;
    /// 执行流式搜索查询。
    ///
    /// 默认实现保持兼容：仍然走完整查询，并把完整回答作为一个 delta 发出。
    /// 真正支持 SSE 的执行器可覆盖该方法，将上游增量逐段写入 `delta_tx`。
    async fn query_stream(
        &self,
        req: QueryRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<QueryOutcome, LlmError> {
        let outcome = self.query(req).await?;
        let _ = delta_tx.send(outcome.answer.clone()).await;
        Ok(outcome)
    }
    /// 返回服务商名称。
    fn provider_name(&self) -> &'static str;
}

/// 动态派发的搜索查询执行器。
pub type DynQueryExecutor = Arc<dyn QueryExecutor>;

/// 根据配置构建搜索查询执行器。
///
/// 如果未配置 API key，则返回 [`MissingQueryExecutor`]。
pub fn build_query_executor(config: &AppConfig) -> Result<DynQueryExecutor, LlmError> {
    if config.openai_api_key.is_none() {
        return Ok(Arc::new(MissingQueryExecutor));
    }
    if config.openai_api_mode == OpenAiApiMode::ChatOnly {
        return Ok(Arc::new(ChatOnlyQueryExecutor));
    }

    Ok(Arc::new(OpenAiWebSearchQueryExecutor::new(config)?))
}

/// 缺少 API key 时的占位执行器，收到查询请求时返回配置错误。
pub struct MissingQueryExecutor;

#[async_trait]
impl QueryExecutor for MissingQueryExecutor {
    async fn query(&self, _req: QueryRequest) -> Result<QueryOutcome, LlmError> {
        Err(LlmError::config(
            "OPENAI_API_KEY is required for Rust web query service",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "openai"
    }
}

pub struct ChatOnlyQueryExecutor;

#[async_trait]
impl QueryExecutor for ChatOnlyQueryExecutor {
    async fn query(&self, _req: QueryRequest) -> Result<QueryOutcome, LlmError> {
        Err(LlmError::config(
            "OPENAI_API_MODE=chat_only only supports chat completions; /查 requires an OpenAI Responses web_search compatible endpoint",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "openai"
    }
}

/// 基于 OpenAI Responses API 的联网搜索执行器。
pub struct OpenAiWebSearchQueryExecutor {
    /// HTTP 客户端
    client: reqwest::Client,
    /// OpenAI API 密钥
    api_key: String,
    /// 自定义 API 基础地址
    base_url: Option<String>,
    /// 搜索模型名称
    search_model: String,
}

impl OpenAiWebSearchQueryExecutor {
    /// 创建新的 OpenAI 搜索执行器。
    pub fn new(config: &AppConfig) -> Result<Self, LlmError> {
        let api_key = config
            .openai_api_key
            .clone()
            .ok_or_else(|| LlmError::config("OPENAI_API_KEY is required"))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.request_timeout_seconds))
            .build()
            .map_err(|err| {
                LlmError::config(format!("failed to build OpenAI query HTTP client: {err}"))
            })?;

        Ok(Self {
            client,
            api_key,
            base_url: config.openai_base_url.clone(),
            search_model: config.openai_search_model.clone(),
        })
    }
}

#[async_trait]
impl QueryExecutor for OpenAiWebSearchQueryExecutor {
    async fn query(&self, req: QueryRequest) -> Result<QueryOutcome, LlmError> {
        let query = req.query.trim();
        if query.is_empty() {
            return Err(LlmError::new(
                "bad_request",
                "query must not be empty",
                "request",
            ));
        }

        let started = std::time::Instant::now();
        let max_results = configured_max_results(req.max_results);
        let payload = openai_responses_payload(&req, query, max_results, &self.search_model, false);
        let url = openai_responses_url(self.base_url.as_deref());
        trace_openai_query_payload(&req, &url, &payload);

        let response = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|err| LlmError::http(format!("OpenAI web query request failed: {err}")))?;

        let status = response.status();
        if !status.is_success() {
            return Err(openai_status_error(status));
        }

        let body: Value = response.json().await.map_err(|err| {
            LlmError::provider(format!("invalid OpenAI query JSON: {err}"), "json")
        })?;
        let answer = extract_output_text(&body).ok_or_else(|| {
            LlmError::provider("OpenAI web query returned empty text output", "provider")
        })?;
        let sources = extract_sources(&body, usize::from(max_results));

        Ok(QueryOutcome {
            answer,
            sources,
            provider: "openai".to_owned(),
            elapsed_ms: duration_ms(started.elapsed()),
        })
    }

    async fn query_stream(
        &self,
        req: QueryRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<QueryOutcome, LlmError> {
        let query = req.query.trim();
        if query.is_empty() {
            return Err(LlmError::new(
                "bad_request",
                "query must not be empty",
                "request",
            ));
        }

        let started = std::time::Instant::now();
        let max_results = configured_max_results(req.max_results);
        let payload = openai_responses_payload(&req, query, max_results, &self.search_model, true);
        let url = openai_responses_url(self.base_url.as_deref());
        trace_openai_query_payload(&req, &url, &payload);

        let mut response = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|err| LlmError::http(format!("OpenAI web query request failed: {err}")))?;

        let status = response.status();
        if !status.is_success() {
            return Err(openai_status_error(status));
        }

        let mut frame_buffer = Vec::new();
        let mut answer = String::new();
        let mut completed_response: Option<Value> = None;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| LlmError::http(format!("OpenAI web query stream failed: {err}")))?
        {
            frame_buffer.extend_from_slice(&chunk);
            while let Some(frame) = take_sse_frame(&mut frame_buffer) {
                let Some(event) = parse_sse_frame(&frame)? else {
                    continue;
                };
                handle_openai_stream_event(event, &mut answer, &mut completed_response, &delta_tx)
                    .await?;
            }
        }
        if !frame_buffer.is_empty()
            && let Some(event) = parse_sse_frame(&frame_buffer)?
        {
            handle_openai_stream_event(event, &mut answer, &mut completed_response, &delta_tx)
                .await?;
        }

        if answer.trim().is_empty()
            && let Some(response) = completed_response.as_ref()
        {
            answer = extract_output_text(response).unwrap_or_default();
        }
        let answer = answer.trim().to_owned();
        if answer.is_empty() {
            return Err(LlmError::provider(
                "OpenAI web query returned empty text output",
                "provider",
            ));
        }
        let sources = completed_response
            .as_ref()
            .map(|response| extract_sources(response, usize::from(max_results)))
            .unwrap_or_default();

        Ok(QueryOutcome {
            answer,
            sources,
            provider: "openai".to_owned(),
            elapsed_ms: duration_ms(started.elapsed()),
        })
    }

    fn provider_name(&self) -> &'static str {
        "openai"
    }
}

/// 构造 OpenAI Responses API 完整 URL。
fn openai_responses_url(base_url: Option<&str>) -> String {
    let base_url = base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(OPENAI_DEFAULT_BASE_URL);
    format!("{}/responses", base_url.trim_end_matches('/'))
}

/// 获取配置的结果数量，确保在有效范围内。
fn configured_max_results(max_results: Option<u8>) -> u8 {
    max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, MAX_RESULTS_LIMIT)
}

/// 标准化搜索上下文大小为 "low"、"medium" 或 "high"。
fn normalized_context_size(value: Option<&str>) -> &str {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("low") => "low",
        Some("medium") => "medium",
        Some("high") => "high",
        _ => DEFAULT_SEARCH_CONTEXT_SIZE,
    }
}

/// 构造 OpenAI Responses API 请求体（含 web_search 工具配置）。
fn openai_responses_payload(
    req: &QueryRequest,
    query: &str,
    max_results: u8,
    search_model: &str,
    stream: bool,
) -> Value {
    let tool = json!({
        "type": "web_search",
        "search_context_size": normalized_context_size(req.context_size.as_deref())
    });

    let mut payload = json!({
        "model": search_model,
        "tools": [tool],
        "tool_choice": "required",
        "include": ["web_search_call.action.sources"],
        "input": build_query_prompt(
            query,
            req.raw_question.as_deref(),
            max_results,
            &request_time_context()
        ),
    });
    if stream {
        payload["stream"] = json!(true);
    }
    payload
}

/// 在 TRACE 级别输出搜索请求的 payload 摘要。
///
/// 如果环境变量 `LLM_TRACE_QUERY_INPUT` 开启，还会输出完整的 input 文本。
fn trace_openai_query_payload(req: &QueryRequest, url: &str, payload: &Value) {
    if !tracing::enabled!(tracing::Level::TRACE) {
        return;
    }

    let input = payload
        .get("input")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool_choice = payload
        .get("tool_choice")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tools = payload.get("tools").unwrap_or(&Value::Null).to_string();
    let include = payload.get("include").unwrap_or(&Value::Null).to_string();
    tracing::trace!(
        upstream_url = url,
        model = model,
        tool_choice = tool_choice,
        tools = %tools,
        include = %include,
        input_chars = input.chars().count(),
        query_chars = req.query.trim().chars().count(),
        "openai query request payload summary"
    );

    if trace_query_input_enabled() {
        tracing::trace!(
            upstream_url = url,
            input = %input,
            "openai query request input"
        );
    }
}

/// 检查是否启用了查询输入追踪（环境变量 `LLM_TRACE_QUERY_INPUT`）。
fn trace_query_input_enabled() -> bool {
    env::var("LLM_TRACE_QUERY_INPUT")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "yes" | "enabled"
            )
        })
        .unwrap_or(false)
}

async fn handle_openai_stream_event(
    event: SseFrame,
    answer: &mut String,
    completed_response: &mut Option<Value>,
    delta_tx: &mpsc::Sender<String>,
) -> Result<(), LlmError> {
    let value = serde_json::from_str::<Value>(&event.data)
        .map_err(|err| LlmError::provider(format!("invalid OpenAI stream JSON: {err}"), "sse"))?;
    let event_type = event
        .event
        .as_deref()
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or("");

    match event_type {
        "response.output_text.delta" => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str)
                && !delta.is_empty()
            {
                answer.push_str(delta);
                let _ = delta_tx.send(delta.to_owned()).await;
            }
        }
        "response.completed" => {
            *completed_response = value
                .get("response")
                .cloned()
                .or_else(|| Some(value.clone()));
        }
        "response.failed" | "response.incomplete" => {
            let message = stream_error_message(&value)
                .unwrap_or_else(|| format!("OpenAI web query stream event {event_type}"));
            return Err(LlmError::provider(message, "sse"));
        }
        _ => {}
    }

    Ok(())
}

fn stream_error_message(value: &Value) -> Option<String> {
    value
        .get("error")
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("error"))
        })
        .and_then(|error| error.get("message").or(Some(error)))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// 构造发送给 LLM 的查询提示词，包含时间上下文和查询要求。
fn build_query_prompt(
    query: &str,
    raw_question: Option<&str>,
    max_results: u8,
    time_context: &RequestTimeContext,
) -> String {
    let user_question = raw_question
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(query);
    format!(
        "请联网查询并用中文回答用户问题。\n\n{}\n\n要求：\n1. 不要自行猜测当前日期。\n2. 必须按程序传入的 current_date 和 timezone 理解相对时间。\n3. 查询时优先寻找程序解析出的明确日期或日期范围内发生或发布的信息。\n4. 如果搜索结果日期与用户所指日期不一致，请提醒用户“搜索结果日期与用户所指日期不一致”，不要直接把搜索结果当作目标日期事件回答。\n5. 优先基于搜索到的公开网页信息回答。\n6. 如果信息不足，请明确说明不确定。\n7. 尽量保留来源链接或引用信息。\n8. 回答使用中文。\n9. 参考来源最多列出 {max_results} 条。",
        time_context.query_time_block(user_question)
    )
}

/// 将 OpenAI 非成功 HTTP 状态码转换为 LlmError。
fn openai_status_error(status: StatusCode) -> LlmError {
    LlmError::http(format!(
        "OpenAI web query returned HTTP {}",
        status.as_u16()
    ))
}

/// 从 OpenAI Responses API 响应中提取回答文本。
///
/// 优先从顶级 `output_text` 字段获取，其次从 `output` 数组中的 output_text 内容项提取。
fn extract_output_text(body: &Value) -> Option<String> {
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
            if !matches!(item_type, Some("output_text") | None) {
                continue;
            }
            let Some(text) = content_item
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            else {
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

/// 从 OpenAI Responses API 响应中提取来源列表。
///
/// 来源可能位于 action.sources 或 content.annotations 中，去重后返回。
fn extract_sources(body: &Value, max_results: usize) -> Vec<QuerySource> {
    let mut sources = Vec::new();
    let mut seen_urls = std::collections::HashSet::new();

    if let Some(output) = body.get("output").and_then(Value::as_array) {
        for output_item in output {
            if let Some(action_sources) = output_item
                .get("action")
                .and_then(|action| action.get("sources"))
                .and_then(Value::as_array)
            {
                collect_sources(action_sources, &mut sources, &mut seen_urls, max_results);
            }

            if let Some(content_items) = output_item.get("content").and_then(Value::as_array) {
                for content_item in content_items {
                    if let Some(annotations) =
                        content_item.get("annotations").and_then(Value::as_array)
                    {
                        collect_sources(annotations, &mut sources, &mut seen_urls, max_results);
                    }
                }
            }

            if sources.len() >= max_results {
                break;
            }
        }
    }

    sources
}

/// 从 JSON 数组中收集来源，去重并限制数量。
fn collect_sources(
    values: &[Value],
    sources: &mut Vec<QuerySource>,
    seen_urls: &mut std::collections::HashSet<String>,
    max_results: usize,
) {
    for value in values {
        if sources.len() >= max_results {
            return;
        }
        let Some(url) = value.get("url").and_then(Value::as_str).map(str::trim) else {
            continue;
        };
        if url.is_empty() || seen_urls.contains(url) {
            continue;
        }
        let title = value
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or(url);
        let snippet = value
            .get("snippet")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        sources.push(QuerySource {
            title: title.to_owned(),
            url: url.to_owned(),
            snippet: snippet.to_owned(),
        });
        seen_urls.insert(url.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};

    fn test_config(openai_api_mode: OpenAiApiMode) -> AppConfig {
        AppConfig {
            provider: crate::config::ProviderMode::OpenAi,
            model: "openai:gpt-5.5".to_owned(),
            model_route: crate::provider::types::ModelRoute::parse_config(
                "openai:gpt-5.5",
                "LLM_MODEL",
            )
            .unwrap(),
            title_model: None,
            todo_model: None,
            memory_model: None,
            compact_model: None,
            translation_model: None,
            openai_search_model: "gpt-5.5".to_owned(),
            openai_api_key: Some("test-key".to_owned()),
            openai_base_url: Some("http://127.0.0.1:9/v1".to_owned()),
            openai_api_mode,
            deepseek_api_key: None,
            deepseek_base_url: crate::config::DEFAULT_DEEPSEEK_BASE_URL.to_owned(),
            deepseek_model: crate::config::DEFAULT_DEEPSEEK_MODEL.to_owned(),
            stream: false,
            send_mode: "final".to_owned(),
            request_timeout_seconds: 1,
            ttft_warn_seconds: crate::config::DEFAULT_TTFT_WARN_SECONDS,
            max_output_tokens: crate::config::DEFAULT_MAX_OUTPUT_TOKENS,
            server_host: crate::config::DEFAULT_SERVER_HOST.to_owned(),
            server_port: crate::config::DEFAULT_SERVER_PORT,
            app_db_file: crate::config::DEFAULT_APP_DB_FILE.to_owned(),
            rss_enabled: true,
            rss_poll_interval_seconds: crate::config::DEFAULT_RSS_POLL_INTERVAL_SECONDS,
            rss_http_timeout_seconds: crate::config::DEFAULT_RSS_HTTP_TIMEOUT_SECONDS,
            rss_max_body_bytes: crate::config::DEFAULT_RSS_MAX_BODY_BYTES,
            rss_max_push_per_feed: crate::config::DEFAULT_RSS_MAX_PUSH_PER_FEED,
            rss_summary_max_chars: crate::config::DEFAULT_RSS_SUMMARY_MAX_CHARS,
            rss_seen_retention: crate::config::DEFAULT_RSS_SEEN_RETENTION,
            rss_push_max_failures: crate::config::DEFAULT_RSS_PUSH_MAX_FAILURES,
            rss_push_url: crate::config::DEFAULT_RSS_PUSH_URL.to_owned(),
            rss_push_token: None,
            rss_push_message_type: crate::config::DEFAULT_RSS_PUSH_MESSAGE_TYPE.to_owned(),
            rss_allow_private_urls: false,
            prompt_dir: crate::config::DEFAULT_PROMPT_DIR.to_owned(),
            prompt_dir_uses_builtin_defaults: true,
            world_file: None,
            member_id_mapping_file: crate::config::DEFAULT_MEMBER_ID_MAPPING_FILE.to_owned(),
            qweather_api_key: "test-qweather-key".to_owned(),
            qweather_api_host: "https://api.qweather.com".to_owned(),
            qweather_geo_host: "https://geoapi.qweather.com".to_owned(),
            web_console_enabled: false,
            web_console_allowed_origins: Vec::new(),
        }
    }

    fn fixed_time_context() -> RequestTimeContext {
        let offset = FixedOffset::east_opt(8 * 60 * 60).unwrap();
        RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 6, 9, 18, 40, 0).unwrap())
    }

    #[tokio::test]
    async fn chat_only_query_executor_returns_config_error_without_http_request() {
        let executor = build_query_executor(&test_config(OpenAiApiMode::ChatOnly)).unwrap();

        let err = executor
            .query(QueryRequest {
                query: "keyword".to_owned(),
                raw_question: None,
                max_results: None,
                context_size: None,
            })
            .await
            .unwrap_err();

        assert_eq!(err.code, "config");
        assert!(err.message.contains("OPENAI_API_MODE=chat_only"));
        assert!(err.message.contains("Responses web_search"));
    }

    #[test]
    fn openai_url_uses_default_or_custom_base() {
        assert_eq!(
            openai_responses_url(None),
            "https://api.openai.com/v1/responses"
        );
        assert_eq!(
            openai_responses_url(Some("https://proxy.example/v1/")),
            "https://proxy.example/v1/responses"
        );
    }

    #[test]
    fn normal_payload_uses_web_search_context_size() {
        let req = QueryRequest {
            query: "Cloudflare D1".to_owned(),
            raw_question: None,
            max_results: Some(3),
            context_size: Some("high".to_owned()),
        };
        let payload = openai_responses_payload(&req, &req.query, 3, DEFAULT_SEARCH_MODEL, false);

        assert_eq!(payload["model"], DEFAULT_SEARCH_MODEL);
        assert_eq!(payload["tools"][0]["type"], "web_search");
        assert_eq!(payload["tools"][0]["search_context_size"], "high");
        assert_eq!(payload["tool_choice"], "required");
        assert!(
            payload["input"]
                .as_str()
                .unwrap()
                .contains("参考来源最多列出 3 条")
        );
        assert!(
            payload["input"]
                .as_str()
                .unwrap()
                .contains("当前本地日期：")
        );
        assert!(payload.get("stream").is_none());
    }

    #[test]
    fn stream_payload_sets_stream_flag() {
        let req = QueryRequest {
            query: "Cloudflare D1".to_owned(),
            raw_question: None,
            max_results: Some(3),
            context_size: None,
        };
        let payload = openai_responses_payload(&req, &req.query, 3, DEFAULT_SEARCH_MODEL, true);

        assert_eq!(payload["stream"], true);
    }

    #[test]
    fn parses_sse_frames_across_chunks() {
        let mut buffer = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"你"
            .as_bytes()
            .to_vec();
        assert!(take_sse_frame(&mut buffer).is_none());
        buffer.extend_from_slice("好\"}\n\n".as_bytes());

        let frame = take_sse_frame(&mut buffer).unwrap();
        let parsed = parse_sse_frame(&frame).unwrap().unwrap();

        assert_eq!(parsed.event.as_deref(), Some("response.output_text.delta"));
        assert!(parsed.data.contains("你好"));
    }

    #[test]
    fn query_prompt_includes_time_context_and_resolved_relative_date() {
        let prompt = build_query_prompt(
            "昨天苹果发布会情况",
            Some("/查 昨天苹果发布会情况"),
            5,
            &fixed_time_context(),
        );

        assert!(prompt.contains("当前本地日期：2026-06-09"));
        assert!(prompt.contains("当前本地时间：2026-06-09 18:40:00"));
        assert!(prompt.contains("当前时区：Asia/Shanghai"));
        assert!(prompt.contains("用户原始问题：\n/查 昨天苹果发布会情况"));
        assert!(prompt.contains("昨天 = 2026-06-08"));
        assert!(prompt.contains("不要自行猜测当前日期"));
        assert!(prompt.contains("搜索结果日期与用户所指日期不一致"));
    }

    #[test]
    fn query_prompt_includes_resolved_relative_range() {
        let prompt = build_query_prompt("上周政策变化", None, 5, &fixed_time_context());

        assert!(prompt.contains("上周 = 2026-06-01 至 2026-06-07"));
        assert!(prompt.contains("请联网查询"));
    }

    /// 合并 3 个 extract_output_text 测试为表驱动测试。
    #[test]
    fn extracts_output_text_from_various_shapes() {
        struct Case {
            name: &'static str,
            body: serde_json::Value,
            expected: Option<&'static str>,
        }

        let cases = [
            Case {
                name: "extracts_output_text_from_top_level_field",
                body: json!({"output_text": " answer "}),
                expected: Some("answer"),
            },
            Case {
                name: "extracts_output_text_from_nested_response_output",
                body: json!({
                    "output": [
                        {"type": "web_search_call", "action": {"sources": []}},
                        {
                            "type": "message",
                            "content": [
                                {"type": "output_text", "text": " nested answer "}
                            ]
                        }
                    ]
                }),
                expected: Some("nested answer"),
            },
            Case {
                name: "joins_multiple_nested_output_text_items",
                body: json!({
                    "output": [{
                        "type": "message",
                        "content": [
                            {"type": "output_text", "text": "first"},
                            {"type": "refusal", "refusal": "skip"},
                            {"type": "output_text", "text": "second"}
                        ]
                    }]
                }),
                expected: Some("first\n\nsecond"),
            },
        ];

        for case in &cases {
            let actual = extract_output_text(&case.body);
            assert_eq!(
                actual.as_deref(),
                case.expected,
                "case '{}' failed",
                case.name
            );
        }
    }

    #[test]
    fn extracts_sources_from_action_and_annotations() {
        let body = json!({
            "output_text": "answer",
            "output": [
                {
                    "action": {
                        "sources": [
                            {"title": "A", "url": "https://a.test", "snippet": "aa"}
                        ]
                    },
                    "content": [
                        {
                            "annotations": [
                                {"title": "A duplicate", "url": "https://a.test"},
                                {"title": "B", "url": "https://b.test"}
                            ]
                        }
                    ]
                }
            ]
        });

        let sources = extract_sources(&body, 5);

        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].title, "A");
        assert_eq!(sources[0].snippet, "aa");
        assert_eq!(sources[1].url, "https://b.test");
    }
}

//! Web Search 查询执行抽象。
//!
//! Core 只负责 `/查` 命令解析、权限、session 记录和回复排版；本模块负责
//! 搜索 provider 路由、请求 payload、HTTP transport、SSE 文本增量、answer 和 sources 提取。

mod gemini;
mod openai;
mod routing;
mod tavily;

use std::{env, sync::Arc};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::{
    config::{LlmConfig, OpenAiApiMode},
    error::{ErrorInfo, LlmError},
};
use qq_maid_common::time_context::RequestTimeContext;

use gemini::{GeminiWebSearchExecutor, MissingGeminiWebSearchExecutor};
use openai::{ChatOnlyWebSearchExecutor, MissingWebSearchExecutor, OpenAiWebSearchExecutor};
use routing::RoutedWebSearchExecutor;
use tavily::{MissingTavilyWebSearchExecutor, TavilyWebSearchExecutor};

/// 默认搜索结果返回数量。
pub const DEFAULT_MAX_RESULTS: u8 = 5;
/// 搜索结果返回数量上限。
pub const MAX_RESULTS_LIMIT: u8 = 10;
/// 默认搜索上下文大小。
pub(crate) const DEFAULT_SEARCH_CONTEXT_SIZE: &str = "low";

/// 联网搜索后端。默认值保持历史 Provider 原生搜索行为。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchBackend {
    #[default]
    ProviderNative,
    Tavily,
    Disabled,
}

impl WebSearchBackend {
    pub fn parse_config(value: &str, name: &str) -> Result<Self, LlmError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "provider_native" => Ok(Self::ProviderNative),
            "tavily" => Ok(Self::Tavily),
            "disabled" => Ok(Self::Disabled),
            _ => Err(LlmError::config(format!(
                "{name} must be provider_native, tavily, or disabled"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderNative => "provider_native",
            Self::Tavily => "tavily",
            Self::Disabled => "disabled",
        }
    }
}

/// Tavily 搜索深度；第一阶段只开放稳定的 basic / advanced 两档。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WebSearchDepth {
    #[default]
    Basic,
    Advanced,
}

impl WebSearchDepth {
    pub fn parse_config(value: &str, name: &str) -> Result<Self, LlmError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "basic" => Ok(Self::Basic),
            "advanced" => Ok(Self::Advanced),
            _ => Err(LlmError::config(format!(
                "{name} must be basic or advanced"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Advanced => "advanced",
        }
    }
}

/// Tavily 搜索主题。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WebSearchTopic {
    #[default]
    General,
    News,
    Finance,
}

impl WebSearchTopic {
    pub fn parse_config(value: &str, name: &str) -> Result<Self, LlmError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "general" => Ok(Self::General),
            "news" => Ok(Self::News),
            "finance" => Ok(Self::Finance),
            _ => Err(LlmError::config(format!(
                "{name} must be general, news, or finance"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::News => "news",
            Self::Finance => "finance",
        }
    }
}

/// Tavily 相对时间范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchTimeRange {
    Day,
    Week,
    Month,
    Year,
}

impl WebSearchTimeRange {
    pub fn parse_config(value: &str, name: &str) -> Result<Self, LlmError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "day" => Ok(Self::Day),
            "week" => Ok(Self::Week),
            "month" => Ok(Self::Month),
            "year" => Ok(Self::Year),
            _ => Err(LlmError::config(format!(
                "{name} must be day, week, month, or year"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}

/// 统一联网搜索配置。Provider 原生后端只使用 max_results，Tavily 额外使用其余字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSearchConfig {
    /// 默认搜索后端；请求未带场景 route 时使用。
    pub default_backend: WebSearchBackend,
    /// 默认搜索模型；provider_native/openai/gemini route 使用。
    pub default_model: String,
    pub max_results: u8,
    pub search_depth: WebSearchDepth,
    pub topic: WebSearchTopic,
    pub time_range: Option<WebSearchTimeRange>,
    pub connect_timeout_seconds: u64,
    pub first_response_timeout_seconds: u64,
    pub total_timeout_seconds: u64,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            default_backend: WebSearchBackend::ProviderNative,
            default_model: "gpt-search".to_owned(),
            max_results: DEFAULT_MAX_RESULTS,
            search_depth: WebSearchDepth::Basic,
            topic: WebSearchTopic::General,
            time_range: None,
            connect_timeout_seconds: 10,
            first_response_timeout_seconds: 30,
            total_timeout_seconds: 60,
        }
    }
}

/// Web Search 请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchRequest {
    /// 搜索查询文本。
    pub query: String,
    /// 用户的原始问题（用于构造给 LLM 的提示，比 query 更完整）。
    #[serde(default)]
    pub raw_question: Option<String>,
    /// 期望返回的结果数量。
    pub max_results: Option<u8>,
    /// 搜索上下文大小（"low"、"medium"、"high"）。
    pub context_size: Option<String>,
    /// 搜索主题；当前由 Tavily 后端消费，Provider 原生后端保持自身协议语义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// 相对时间范围；当前由 Tavily 后端消费。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range: Option<String>,
    /// 请求级搜索后端；由 Core 根据场景 route 注入，模型不能覆盖。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_override: Option<WebSearchBackend>,
    /// 请求级搜索模型覆盖；由上层场景策略解析后传入。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
}

/// Web Search 的单个来源。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchSource {
    /// 来源标题。
    pub title: String,
    /// 来源 URL。
    pub url: String,
    /// 摘要片段。
    #[serde(default)]
    pub snippet: String,
}

/// Web Search 响应传输结构。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSearchResponse {
    /// 是否成功。
    pub ok: bool,
    /// 搜索结果回答文本。
    pub answer: String,
    /// 来源列表。
    pub sources: Vec<WebSearchSource>,
    /// 服务提供商名称。
    pub provider: String,
    /// 耗时（毫秒）。
    pub elapsed_ms: u64,
    /// 错误信息（成功时为 None）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

/// Web Search 内部结果。
#[derive(Debug, Clone)]
pub struct WebSearchOutcome {
    /// 回答文本。
    pub answer: String,
    /// 来源列表。
    pub sources: Vec<WebSearchSource>,
    /// 提供商名称。
    pub provider: String,
    /// 耗时（毫秒）。
    pub elapsed_ms: u64,
}

impl WebSearchResponse {
    pub fn ok(outcome: WebSearchOutcome) -> Self {
        Self {
            ok: true,
            answer: outcome.answer,
            sources: outcome.sources,
            provider: outcome.provider,
            elapsed_ms: outcome.elapsed_ms,
            error: None,
        }
    }

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

#[async_trait]
pub trait WebSearchExecutor: Send + Sync {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError>;

    /// 默认实现保持兼容：完整查询结束后把完整回答作为一个 delta 发出。
    async fn query_stream(
        &self,
        req: WebSearchRequest,
        delta_tx: mpsc::Sender<String>,
    ) -> Result<WebSearchOutcome, LlmError> {
        let outcome = self.query(req).await?;
        let _ = delta_tx.send(outcome.answer.clone()).await;
        Ok(outcome)
    }

    fn provider_name(&self) -> &'static str;
}

pub type DynWebSearchExecutor = Arc<dyn WebSearchExecutor>;

/// 根据 LLM 配置构建 Web Search 执行器。
pub fn build_web_search_executor(config: &LlmConfig) -> Result<DynWebSearchExecutor, LlmError> {
    let openai: DynWebSearchExecutor = if config.openai_api_key.is_none() {
        Arc::new(MissingWebSearchExecutor)
    } else if config.openai_api_mode == OpenAiApiMode::ChatOnly {
        Arc::new(ChatOnlyWebSearchExecutor)
    } else {
        Arc::new(OpenAiWebSearchExecutor::new(config)?)
    };
    let gemini: DynWebSearchExecutor = if config.gemini_api_key.is_none() {
        Arc::new(MissingGeminiWebSearchExecutor)
    } else {
        Arc::new(GeminiWebSearchExecutor::new(config)?)
    };
    let tavily: DynWebSearchExecutor = match config.tavily_api_key.clone() {
        Some(api_key) => Arc::new(TavilyWebSearchExecutor::new(
            api_key,
            config.web_search.clone(),
        )?),
        None => Arc::new(MissingTavilyWebSearchExecutor),
    };
    Ok(Arc::new(RoutedWebSearchExecutor::new(
        config.web_search.default_backend,
        config.web_search.default_model.clone(),
        config.web_search.max_results,
        openai,
        gemini,
        tavily,
        Arc::new(DisabledWebSearchExecutor),
    )))
}

struct DisabledWebSearchExecutor;

#[async_trait]
impl WebSearchExecutor for DisabledWebSearchExecutor {
    async fn query(&self, _req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        Err(LlmError::new(
            "web_search_disabled",
            "web search is disabled by tools.web_search.backend",
            "web_search",
        ))
    }

    fn provider_name(&self) -> &'static str {
        "disabled"
    }
}

pub(crate) fn configured_max_results(max_results: Option<u8>) -> u8 {
    max_results
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, MAX_RESULTS_LIMIT)
}

pub(crate) fn build_query_prompt(
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

pub(crate) fn trace_query_input_enabled() -> bool {
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

pub(crate) fn truncate_error_detail(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let mut truncated = value.chars().take(limit).collect::<String>();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};
    use qq_maid_common::time_context::RequestTimeContext;

    fn fixed_time_context() -> RequestTimeContext {
        let offset = FixedOffset::east_opt(8 * 60 * 60).unwrap();
        RequestTimeContext::from_datetime(offset.with_ymd_and_hms(2026, 6, 9, 18, 40, 0).unwrap())
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
        assert!(prompt.contains("用户原始问题：\n/查 昨天苹果发布会情况"));
        assert!(prompt.contains("昨天 = 2026-06-08"));
        assert!(prompt.contains("搜索结果日期与用户所指日期不一致"));
    }
}

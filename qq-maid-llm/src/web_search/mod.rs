//! Web Search 查询执行抽象。
//!
//! Core 只负责 `/查` 命令解析、权限、session 记录和回复排版；本模块负责
//! 搜索 provider 路由、请求 payload、HTTP transport、SSE 文本增量、answer 和 sources 提取。

mod gemini;
mod openai;
mod routing;

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

/// 默认搜索结果返回数量。
pub const DEFAULT_MAX_RESULTS: u8 = 5;
/// 搜索结果返回数量上限。
pub const MAX_RESULTS_LIMIT: u8 = 10;
/// 默认搜索上下文大小。
pub(crate) const DEFAULT_SEARCH_CONTEXT_SIZE: &str = "low";

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
    Ok(Arc::new(RoutedWebSearchExecutor::new(
        config.openai_search_model.clone(),
        openai,
        gemini,
    )))
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

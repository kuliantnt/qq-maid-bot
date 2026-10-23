//! 独立 Search API：不依赖聊天模型，也不发送会话历史或用户身份。
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::Url;
use serde::Deserialize;
use serde_json::json;

use super::{
    MAX_RESULTS_LIMIT, WebSearchConfig, WebSearchExecutor, WebSearchOutcome, WebSearchRequest,
    WebSearchSource,
};
use crate::{
    config::LlmConfig,
    error::{LlmError, LlmErrorKind},
    metrics::duration_ms,
};
use qq_maid_common::text::truncate_chars_with_ellipsis_trimmed;

/// answer 中单条检索摘要的长度上限；与 `/查` 的 1500 字符展示预算配合，
/// 保证摘要之外仍能看到参考来源链接。
const ANSWER_SOURCE_SUMMARY_MAX_CHARS: usize = 180;
/// answer 摘要正文的总预算，长摘要不能挤掉结构化 `sources` 之外的参考链接。
const ANSWER_SUMMARY_TOTAL_MAX_CHARS: usize = 900;

pub(super) struct DeepSeekSearchExecutor {
    client: reqwest::Client,
    endpoint: String,
    api_key: Option<String>,
    config: WebSearchConfig,
}

impl DeepSeekSearchExecutor {
    pub(super) fn new(config: &LlmConfig) -> Result<Self, LlmError> {
        let options = &config.web_search;
        super::search_support::validate_timeouts(options)?;
        let client = qq_maid_common::http_client::try_builder()
            .map_err(|err| LlmError::config(format!("DeepSeek Search TLS: {err}")))?
            .connect_timeout(Duration::from_secs(options.connect_timeout_seconds))
            .timeout(Duration::from_secs(options.total_timeout_seconds))
            .build()
            .map_err(|err| LlmError::config(format!("DeepSeek Search HTTP client: {err}")))?;
        // 兼容管理员使用根地址或 /v1 地址，保留私有网关部署前缀。
        let base = config.deepseek_base_url.trim_end_matches('/');
        let base = base.strip_suffix("/v1").unwrap_or(base);
        Ok(Self {
            client,
            endpoint: format!("{base}/v1/tools/search"),
            api_key: config
                .deepseek_api_key
                .clone()
                .filter(|key| !key.trim().is_empty()),
            config: options.clone(),
        })
    }
}

#[async_trait]
impl WebSearchExecutor for DeepSeekSearchExecutor {
    async fn query(&self, req: WebSearchRequest) -> Result<WebSearchOutcome, LlmError> {
        let api_key = self.api_key.as_deref().ok_or_else(|| {
            // stage 统一带 `deepseek_search` 前缀：Core 需要据此为用户选择 DeepSeek
            // 专属错误文案，不能把缺少 DeepSeek Key 误报成 Tavily 配置问题。
            LlmError::new(
                "web_search_not_configured",
                "DEEPSEEK_API_KEY is required for DeepSeek Search",
                "deepseek_search_config",
            )
        })?;
        let query = req.query.trim();
        // 不截断搜索表达式，避免破坏 site:/引号等运算符并改变检索意图。
        if query.is_empty() || query.chars().count() > 128 {
            return Err(LlmError::new(
                "bad_request",
                "DeepSeek Search query must contain 1 to 128 Unicode characters",
                "request",
            ));
        }
        let limit = req
            .max_results
            .unwrap_or(self.config.max_results)
            .min(self.config.max_results)
            .clamp(1, MAX_RESULTS_LIMIT);
        let started = Instant::now();
        let response = tokio::time::timeout(
            Duration::from_secs(self.config.first_response_timeout_seconds),
            self.client
                .post(&self.endpoint)
                .bearer_auth(api_key)
                .json(&json!({"query": query, "count": limit, "need_content": false}))
                .send(),
        )
        .await
        .map_err(|_| {
            LlmError::new(
                "timeout",
                "DeepSeek Search response headers timed out",
                "deepseek_search_first_response",
            )
        })?
        .map_err(transport_error)?;
        let status = response.status();
        if !status.is_success() {
            // 保留上游错误文本，统一截断和脱敏；不得记录请求正文或密钥。
            let body = response.text().await.map_err(transport_error)?;
            let body = qq_maid_common::redaction::redact_sensitive_text(
                body.replace(api_key, "<redacted>"),
            );
            let detail = super::truncate_error_detail(&body, 300);
            let code = match status.as_u16() {
                400 | 413 => "upstream_bad_request",
                401 => "authentication_failed",
                402 => "quota_exhausted",
                403 => "permission_denied",
                408 | 504 => "timeout",
                429 => "rate_limited",
                500..=599 => "upstream_unavailable",
                _ => "provider_error",
            };
            return Err(LlmError::new(
                code,
                format!("DeepSeek Search HTTP {status}: {detail}"),
                "deepseek_search_http",
            )
            .with_upstream_status(status.as_u16()));
        }
        // results 是必填字段；畸形成功响应不能误报为正常空结果。
        let body: SearchResponse = response.json().await.map_err(|err| {
            LlmError::from_error_source(
                &err,
                LlmErrorKind::Other,
                "deepseek_search_json",
                "failed to read DeepSeek Search JSON",
            )
        })?;
        let sources = sources(body, usize::from(limit));
        let answer = if sources.is_empty() {
            String::new()
        } else {
            render_answer(&sources)
        };
        Ok(WebSearchOutcome {
            answer,
            sources,
            provider: "deepseek".to_owned(),
            elapsed_ms: duration_ms(started.elapsed()),
        })
    }

    fn provider_name(&self) -> &'static str {
        "deepseek"
    }
}

/// DeepSeek 只返回逐条检索结果，不生成整段回答；answer 必须汇总真实检索摘要，
/// 否则 `/查`、Agent 搜索和多实体调研只能拿到参考链接而没有可核实内容。
fn render_answer(sources: &[WebSearchSource]) -> String {
    let mut digest = String::from("搜索到以下公开网页结果：");
    let summarized = sources
        .iter()
        .filter(|source| !source.snippet.trim().is_empty())
        .count();
    // 按摘要条数均分总预算，避免前部长摘要挤掉后部结果的摘要。
    let per_source =
        (ANSWER_SUMMARY_TOTAL_MAX_CHARS / summarized.max(1)).min(ANSWER_SOURCE_SUMMARY_MAX_CHARS);
    let mut used = 0usize;
    let mut rendered = 0usize;
    for source in sources {
        let summary = source.snippet.trim();
        if summary.is_empty() {
            continue;
        }
        let budget = per_source.min(ANSWER_SUMMARY_TOTAL_MAX_CHARS.saturating_sub(used));
        if budget == 0 {
            break;
        }
        let entry = format!(
            "\n{}. {}：{}",
            rendered + 1,
            super::search_support::escape_markdown_label(&source.title),
            truncate_chars_with_ellipsis_trimmed(summary, budget),
        );
        used += entry.chars().count();
        digest.push_str(&entry);
        rendered += 1;
    }
    // 摘要为空时仍保留原有「只有参考来源」的兜底输出。
    super::search_support::render_source_links(Some(&digest), sources)
}

fn transport_error(err: reqwest::Error) -> LlmError {
    LlmError::from_error_source(
        &err,
        LlmErrorKind::Network,
        "deepseek_search_request",
        "DeepSeek Search request failed",
    )
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    summary: String,
}

fn sources(body: SearchResponse, limit: usize) -> Vec<WebSearchSource> {
    body.results
        .into_iter()
        .filter_map(|item| {
            let url = Url::parse(item.url.trim()).ok()?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return None;
            }
            let title = if item.title.trim().is_empty() {
                url.host_str().unwrap_or("未命名来源")
            } else {
                item.title.trim()
            };
            let summary = if item.summary.trim().is_empty() {
                item.snippet.trim()
            } else {
                item.summary.trim()
            };
            Some(WebSearchSource {
                title: title.chars().take(200).collect(),
                url: url.to_string(),
                snippet: summary.chars().take(1_000).collect(),
            })
        })
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests;

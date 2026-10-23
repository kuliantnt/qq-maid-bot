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
use qq_maid_common::text::truncate_markdown_prefix;

/// answer 整体字符预算。`/查` 会在「【联网查询】\n\n」前缀之后按 1500 字符展示
/// 正文，这里预留头部空间，保证下游不会再把 answer 整体截断，也就不会切断
/// Markdown 参考来源链接。
const ANSWER_TOTAL_MAX_CHARS: usize = 1_400;
/// answer 中单条检索摘要的展示上限。
const ANSWER_SOURCE_SUMMARY_MAX_CHARS: usize = 160;
/// 单条检索摘要的展示下限；放不下下限时从尾部减少展示条数，而不是只留几个字。
const ANSWER_SOURCE_SUMMARY_MIN_CHARS: usize = 30;
/// answer 中单条标题的展示上限；完整标题仍保留在结构化 `sources`。
const ANSWER_SOURCE_TITLE_MAX_CHARS: usize = 60;
const ANSWER_HEADER: &str = "搜索到以下公开网页结果：";
const ANSWER_SOURCE_HEADER: &str = "\n\n参考来源：";

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
///
/// 预算分配顺序：
/// 1. 先为每条有摘要的结果锁定编号、展示标题和一段最短摘要，前部长结果无法
///    挤占后续结果的展示位；
/// 2. 参考来源链接在剩余预算内按整行优先占据，放不下的来源整条省略，
///    保证链接永远完整且可追溯；
/// 3. 最后把剩下的预算平均补给摘要；预算异常收缩时才从尾部减少展示条数。
///
/// 结构调整只影响 answer 正文，结构化 `sources` 始终保留全部有效来源。
fn render_answer(sources: &[WebSearchSource]) -> String {
    let entries = summary_entries(sources);
    let digest_floor = entries
        .iter()
        .map(|(prefix, _)| prefix.chars().count())
        .sum::<usize>()
        + entries.len() * ANSWER_SOURCE_SUMMARY_MIN_CHARS;
    let link_budget = ANSWER_TOTAL_MAX_CHARS
        .saturating_sub(ANSWER_HEADER.chars().count())
        .saturating_sub(ANSWER_SOURCE_HEADER.chars().count())
        .saturating_sub(digest_floor);
    let link_section = render_source_link_section(sources, link_budget);
    let entry_budget = ANSWER_TOTAL_MAX_CHARS
        .saturating_sub(ANSWER_HEADER.chars().count())
        .saturating_sub(link_section.chars().count());

    let mut digest = String::from(ANSWER_HEADER);
    digest.push_str(&render_summary_entries(&entries, entry_budget));
    digest.push_str(&link_section);
    digest
}

/// 先把每条有摘要的结果渲染成「编号 + 展示标题 + 分隔符」，标题按展示上限
/// 缩写，完整标题仍保留在结构化 `sources`。
fn summary_entries(sources: &[WebSearchSource]) -> Vec<(String, &WebSearchSource)> {
    sources
        .iter()
        .filter(|source| !source.snippet.trim().is_empty())
        .enumerate()
        .map(|(index, source)| {
            let title =
                super::search_support::display_title(&source.title, ANSWER_SOURCE_TITLE_MAX_CHARS);
            (format!("\n{}. {title}：", index + 1), source)
        })
        .collect()
}

/// 参考来源链接按整行纳入预算：单条链接必须完整展示，放不下的来源整条省略，
/// 结构化 `sources` 仍保留全部有效来源，因此不影响可追溯性。
fn render_source_link_section(sources: &[WebSearchSource], budget: usize) -> String {
    let header_len = ANSWER_SOURCE_HEADER.chars().count();
    let mut section = String::new();
    let mut used = header_len;
    for source in sources {
        let Some(line) = super::search_support::source_link_line(
            &source.title,
            &source.url,
            ANSWER_SOURCE_TITLE_MAX_CHARS,
        ) else {
            continue;
        };
        let next = used + line.chars().count();
        if next > budget {
            // 整条省略放不下的链接，继续尝试后面更短的来源。
            continue;
        }
        section.push_str(&line);
        used = next;
    }
    if used > header_len {
        format!("{ANSWER_SOURCE_HEADER}{section}")
    } else {
        String::new()
    }
}

/// 把摘要区预算先按每条下限分配，剩余额度均分补给各条（单条不超过展示上限）。
fn render_summary_entries(entries: &[(String, &WebSearchSource)], budget: usize) -> String {
    // 预算已为每条结果锁定了下限，这里是常量被调小时的兜底：后续条目整体不展示。
    let mut shown = 0usize;
    let mut fixed = 0usize;
    for (prefix, _) in entries {
        let next_fixed = fixed + prefix.chars().count();
        if next_fixed + (shown + 1) * ANSWER_SOURCE_SUMMARY_MIN_CHARS > budget {
            break;
        }
        fixed = next_fixed;
        shown += 1;
    }
    if shown == 0 {
        return String::new();
    }

    let spare = budget - fixed - shown * ANSWER_SOURCE_SUMMARY_MIN_CHARS;
    let summary_budget =
        (ANSWER_SOURCE_SUMMARY_MIN_CHARS + spare / shown).min(ANSWER_SOURCE_SUMMARY_MAX_CHARS);
    let mut digest = String::new();
    for (prefix, source) in entries.iter().take(shown) {
        digest.push_str(prefix);
        digest.push_str(&truncate_summary(&source.snippet, summary_budget));
    }
    digest
}

/// 摘要按展示预算截断，但不能切断摘要文本自带的 Markdown 行内链接。
///
/// 摘要整体就是一条放不下的链接时安全前缀为空，此时宁可只输出省略号，也不
/// 输出残缺的 `[标签](URL)`；完整摘要在结构化 `sources` 中仍可追溯。
fn truncate_summary(summary: &str, budget: usize) -> String {
    let summary = summary.trim();
    if summary.chars().count() <= budget {
        return summary.to_owned();
    }
    let (prefix, _) = truncate_markdown_prefix(summary, budget);
    if prefix.is_empty() {
        return "…".to_owned();
    }
    format!("{}…", prefix)
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

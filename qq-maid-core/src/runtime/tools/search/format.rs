//! 联网搜索 Tool 结果的确定性正文与来源标记。
//!
//! formatter 同时生成正文和来源字段的展示状态，调用方不需要再扫描最终字符串
//! 推断来源是否已经出现，避免 URL 或摘要子串造成误判。

use qq_maid_common::text::truncate_chars_with_ellipsis_trimmed as truncate_chars;
use serde_json::Value;

use crate::error::LlmError;

const WEB_SEARCH_EMPTY_RESULT_REPLY: &str =
    "【联网查询】\n\n没查到明确结果。可以换一个关键词再试。";
const WEB_SEARCH_ARGUMENT_ERROR_REPLY: &str =
    "【联网查询】\n\n本次联网查询的参数无效，查询未执行。请换一种说法再试。";
const WEB_SEARCH_CONFIG_ERROR_REPLY: &str = "【联网查询】\n\n联网查询还没有配置好，请检查 tools.web_search 后端、搜索 route 和对应 Provider 配置。";
const WEB_SEARCH_DISABLED_REPLY: &str =
    "【联网查询】\n\n联网查询已在 tools.web_search 配置中关闭。";
const WEB_SEARCH_TAVILY_KEY_MISSING_REPLY: &str =
    "【联网查询】\n\n已选择 Tavily，但还没有配置 TAVILY_API_KEY。请在配置中心完成设置后重启。";
const WEB_SEARCH_TAVILY_AUTH_REPLY: &str =
    "【联网查询】\n\nTavily API Key 无效或已失效，请在配置中心检查后重启。";
const WEB_SEARCH_RATE_LIMIT_REPLY: &str =
    "【联网查询】\n\n联网查询请求过于频繁，已被上游限流，请稍后再试。";
const WEB_SEARCH_QUOTA_REPLY: &str =
    "【联网查询】\n\nTavily 查询额度已用尽或账户不可用，请检查账户额度。";
const WEB_SEARCH_TIMEOUT_REPLY: &str = "【联网查询】\n\n联网查询超时了，请稍后再试。";
const WEB_SEARCH_UPSTREAM_ERROR_REPLY: &str =
    "【联网查询】\n\n联网查询服务暂时不可用，可能是上游接口、代理或网络配置异常。请稍后再试。";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormattedWebSearchToolReply {
    pub body: String,
    pub rendered_sources: Vec<WebSearchRenderedSource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebSearchSourceLocation {
    /// None 表示单次搜索顶层 sources，Some(index) 表示多目标调研的
    /// results[index].sources。
    pub result_index: Option<usize>,
    pub source_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebSearchRenderedSource {
    pub location: WebSearchSourceLocation,
    pub identity_rendered: bool,
    pub snippet_rendered: bool,
}

#[derive(Debug, Clone)]
struct WebSearchSourceParts {
    identity: Option<String>,
    snippet: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct SourceTextRange {
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct SourceRenderMarker {
    location: WebSearchSourceLocation,
    identity: Option<SourceTextRange>,
    snippet: Option<SourceTextRange>,
}

#[derive(Debug, Default)]
struct SearchReplyBuilder {
    body: String,
    markers: Vec<SourceRenderMarker>,
}

impl SearchReplyBuilder {
    fn push(&mut self, text: &str) {
        self.body.push_str(text);
    }

    fn push_marked(&mut self, text: &str) -> SourceTextRange {
        let start = self.body.chars().count();
        self.body.push_str(text);
        SourceTextRange {
            start,
            end: start + text.chars().count(),
        }
    }

    fn push_source(&mut self, location: WebSearchSourceLocation, parts: &WebSearchSourceParts) {
        let marker_index = self.markers.len();
        self.markers.push(SourceRenderMarker {
            location,
            identity: None,
            snippet: None,
        });
        if let Some(identity) = parts.identity.as_deref() {
            let range = self.push_marked(identity);
            self.markers[marker_index].identity = Some(range);
            if let Some(snippet) = parts.snippet.as_deref() {
                self.push("：");
                self.markers[marker_index].snippet = Some(self.push_marked(snippet));
            }
        } else if let Some(snippet) = parts.snippet.as_deref() {
            self.markers[marker_index].snippet = Some(self.push_marked(snippet));
        }
    }

    fn mark_existing_snippet(&mut self, location: WebSearchSourceLocation, range: SourceTextRange) {
        self.markers.push(SourceRenderMarker {
            location,
            identity: None,
            snippet: Some(range),
        });
    }
}

#[cfg(test)]
pub(crate) fn format_web_search_tool_reply(output: &Value) -> String {
    format_web_search_tool_reply_with_sources(output).body
}

/// `/查` 和 Agent fallback 共用的搜索正文入口；正文长度和空结果文案必须一致。
pub(crate) fn format_web_search_command_reply(answer: &str) -> String {
    let mut text = answer.trim().to_owned();
    if text.is_empty() {
        text = WEB_SEARCH_EMPTY_RESULT_REPLY
            .strip_prefix("【联网查询】\n\n")
            .unwrap_or(WEB_SEARCH_EMPTY_RESULT_REPLY)
            .to_owned();
    }
    if !text.starts_with("【联网查询】") {
        text = format!("【联网查询】\n\n{text}");
    }
    truncate_chars(&text, 1500)
}

/// 把搜索工具错误映射为用户可执行的稳定文案；上游原始错误只进入日志和诊断。
pub(crate) fn format_web_search_error_reply(err: &LlmError) -> String {
    match err.code.as_str() {
        "config" => WEB_SEARCH_CONFIG_ERROR_REPLY.to_owned(),
        "web_search_disabled" => WEB_SEARCH_DISABLED_REPLY.to_owned(),
        "web_search_not_configured" => WEB_SEARCH_TAVILY_KEY_MISSING_REPLY.to_owned(),
        "tavily_auth_error" => WEB_SEARCH_TAVILY_AUTH_REPLY.to_owned(),
        "rate_limited" => WEB_SEARCH_RATE_LIMIT_REPLY.to_owned(),
        "quota_exhausted" => WEB_SEARCH_QUOTA_REPLY.to_owned(),
        "empty_result" => WEB_SEARCH_EMPTY_RESULT_REPLY.to_owned(),
        "invalid_arguments" | "bad_tool_arguments" => WEB_SEARCH_ARGUMENT_ERROR_REPLY.to_owned(),
        "timeout" => WEB_SEARCH_TIMEOUT_REPLY.to_owned(),
        _ => WEB_SEARCH_UPSTREAM_ERROR_REPLY.to_owned(),
    }
}

pub(crate) fn format_web_search_tool_reply_with_sources(
    output: &Value,
) -> FormattedWebSearchToolReply {
    if json_string_field(output, "mode").as_deref() == Some("multi_entity_research") {
        return format_web_search_research_reply_with_sources(output);
    }

    if let Some(answer) = json_string_field(output, "answer") {
        let mut builder = SearchReplyBuilder::default();
        let answer_range = builder.push_marked(&answer);
        if let Some(sources) = output.get("sources").and_then(Value::as_array) {
            for (source_index, source) in sources.iter().enumerate() {
                let Some(snippet) = json_string_field(source, "snippet") else {
                    continue;
                };
                let Some(byte_offset) = answer.find(&snippet) else {
                    continue;
                };
                let start = answer_range.start + answer[..byte_offset].chars().count();
                builder.mark_existing_snippet(
                    WebSearchSourceLocation {
                        result_index: None,
                        source_index,
                    },
                    SourceTextRange {
                        start,
                        end: start + snippet.chars().count(),
                    },
                );
            }
        }
        return finalize_web_search_tool_reply(builder);
    }

    let source = output
        .get("sources")
        .and_then(Value::as_array)
        .and_then(|sources| {
            sources
                .iter()
                .enumerate()
                .find_map(|(source_index, source)| {
                    format_web_search_source_parts(source).map(|parts| (source_index, parts))
                })
        });
    let mut builder = SearchReplyBuilder::default();
    match source {
        Some((source_index, source)) => {
            builder.push("来源：");
            builder.push_source(
                WebSearchSourceLocation {
                    result_index: None,
                    source_index,
                },
                &source,
            );
        }
        None => builder.push("没查到明确结果。可以换一个关键词再试。"),
    }
    finalize_web_search_tool_reply(builder)
}

fn format_web_search_research_reply_with_sources(output: &Value) -> FormattedWebSearchToolReply {
    let (successful, failed) = multi_entity_research_counts(output);
    let items = output
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let title = multi_entity_research_title(successful, failed);
    let mut builder = SearchReplyBuilder::default();
    builder.push(&title);
    builder.push("\n\n多目标调研结果：\n\n");
    let mut rendered_count = 0;

    for (index, item) in items.iter().enumerate() {
        if json_string_field(item, "status").as_deref() != Some("success") {
            continue;
        }
        let facts = json_string_field(item, "facts")
            .or_else(|| json_string_field(item, "summary"))
            .or_else(|| json_string_field(item, "answer"));
        let source = item
            .get("sources")
            .and_then(Value::as_array)
            .and_then(|sources| {
                sources
                    .iter()
                    .enumerate()
                    .find_map(|(source_index, source)| {
                        format_web_search_source_parts(source).map(|parts| (source_index, parts))
                    })
            });
        if facts.is_none() && source.is_none() {
            continue;
        }

        if rendered_count > 0 {
            builder.push("\n");
        }
        let entity =
            json_string_field(item, "entity").unwrap_or_else(|| format!("目标 {}", index + 1));
        builder.push(&format!("- **{entity}**"));
        if let Some(facts) = facts {
            builder.push("：");
            builder.push(&facts);
        }
        if let Some((source_index, source)) = source {
            builder.push("\n  - 来源：");
            builder.push_source(
                WebSearchSourceLocation {
                    result_index: Some(index),
                    source_index,
                },
                &source,
            );
        }
        rendered_count += 1;
    }

    if rendered_count == 0 {
        let mut empty = SearchReplyBuilder::default();
        empty.push(&format!(
            "{title}\n\n没查到明确结果。可以换一个关键词再试。"
        ));
        return finalize_web_search_tool_reply(empty);
    }

    finalize_web_search_tool_reply(builder)
}

pub(crate) fn format_web_search_research_error_reply(output: &Value, error: &str) -> String {
    let (successful, failed) = multi_entity_research_counts(output);
    let title = multi_entity_research_title(successful, failed);
    let body = error.strip_prefix("【联网查询】\n\n").unwrap_or(error);
    format!("{title}\n\n{body}")
}

fn multi_entity_research_title(successful: usize, failed: usize) -> String {
    if failed == 0 {
        "【联网查询】".to_owned()
    } else {
        format!("【联网查询（成功 {successful}，失败 {failed}）】")
    }
}

fn multi_entity_research_counts(output: &Value) -> (usize, usize) {
    let top_level_counts = output
        .get("successful")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .zip(
            output
                .get("failed")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
        );
    if let Some(counts) = top_level_counts {
        return counts;
    }

    let mut successful = 0;
    let mut failed = 0;
    for item in output
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match json_string_field(item, "status").as_deref() {
            Some("success") => successful += 1,
            Some("failed" | "timeout") => failed += 1,
            _ => {}
        }
    }
    (successful, failed)
}

fn format_web_search_source_parts(source: &Value) -> Option<WebSearchSourceParts> {
    if let Some(source) = source
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(WebSearchSourceParts {
            identity: Some(source.to_owned()),
            snippet: None,
        });
    }

    let title = json_string_field(source, "title");
    let url = json_string_field(source, "url");
    let snippet = json_string_field(source, "snippet");
    let reference = match (title, url) {
        (Some(title), Some(url)) => Some(format!("[{title}]({url})")),
        (Some(title), None) => Some(title),
        (None, Some(url)) => Some(url),
        (None, None) => None,
    };

    (reference.is_some() || snippet.is_some()).then_some(WebSearchSourceParts {
        identity: reference,
        snippet,
    })
}

fn finalize_web_search_tool_reply(mut builder: SearchReplyBuilder) -> FormattedWebSearchToolReply {
    let prefix = if !builder.body.starts_with("【联网查询") {
        "【联网查询】\n\n"
    } else {
        ""
    };
    if !prefix.is_empty() {
        let offset = prefix.chars().count();
        builder.body = format!("{prefix}{}", builder.body);
        for marker in &mut builder.markers {
            shift_source_range(&mut marker.identity, offset);
            shift_source_range(&mut marker.snippet, offset);
        }
    }

    let original_length = builder.body.chars().count();
    let body = truncate_chars(&builder.body, 1500);
    let keep_limit = 1500usize.saturating_sub(1);
    let rendered_sources = builder
        .markers
        .into_iter()
        .map(|marker| WebSearchRenderedSource {
            location: marker.location,
            identity_rendered: source_range_survives_truncation(
                marker.identity,
                original_length,
                keep_limit,
            ),
            snippet_rendered: source_range_survives_truncation(
                marker.snippet,
                original_length,
                keep_limit,
            ),
        })
        .collect();
    FormattedWebSearchToolReply {
        body,
        rendered_sources,
    }
}

fn shift_source_range(range: &mut Option<SourceTextRange>, offset: usize) {
    if let Some(range) = range.as_mut() {
        range.start += offset;
        range.end += offset;
    }
}

fn source_range_survives_truncation(
    range: Option<SourceTextRange>,
    original_length: usize,
    keep_limit: usize,
) -> bool {
    range.is_some_and(|range| original_length <= keep_limit + 1 || range.end <= keep_limit)
}

/// 解析搜索 JSON 中的非空字符串字段，供 formatter 与整轮投影共用。
pub(crate) fn json_string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

//! Web Search Tool 的整轮结果投影。
//!
//! 搜索领域在这里决定单次工具轨迹应展示、隐藏还是交给其他领域处理；通用
//! `tools/agent_turn.rs` 只负责调度，不理解 `deduplicated` 等搜索结果字段。

use qq_maid_common::text::{
    sanitize_single_line_visible_text, truncate_chars_with_ellipsis_trimmed,
};
use qq_maid_llm::provider::{ToolExecutionAttempt, ToolExecutionResult};
use serde_json::Value;
use std::collections::HashSet;

use crate::{
    error::LlmError,
    runtime::respond::{
        agent_outcome::{
            OutcomePresentation, ProvenanceSource, ResponseBlock, ToolEffect, ToolExecutionOutcome,
            ToolOutcomeStatus,
        },
        common::{CommandBody, structured_command_body},
    },
};

use super::{
    WEB_SEARCH_TOOL_NAME, WEB_SEARCH_TOOL_SOURCE_LIMIT, WEB_SEARCH_TOOL_SOURCE_SNIPPET_MAX_CHARS,
    WEB_SEARCH_TOOL_SOURCE_TITLE_MAX_CHARS, WebSearchRenderedSource, WebSearchSourceLocation,
    format_web_search_error_reply, format_web_search_research_error_reply,
    format_web_search_tool_reply_with_sources, json_string_field,
};

const WEB_SEARCH_PROVENANCE_URL_MAX_CHARS: usize = 500;

pub(crate) enum SearchResultProjection {
    Hidden,
    Visible(ToolExecutionOutcome),
}

pub(crate) struct SearchTurnProjection {
    pub(crate) consumed_result_indexes: HashSet<usize>,
    pub(crate) outcomes: Vec<(usize, ToolExecutionOutcome)>,
    pub(crate) provenance: Vec<ProvenanceSource>,
}

/// 搜索结果必须按整轮投影，才能区分“全部失败”和“部分成功”。
/// 原始工具轨迹仍完整留在 diagnostics；这里只控制可信用户正文及模型 final_text
/// 是否允许保留，避免空结果提示重复或被模型补成无证据的时效信息。
pub(crate) fn project_results(
    results: &[ToolExecutionResult],
    attempts: &[ToolExecutionAttempt],
    final_candidate_result_start: usize,
) -> SearchTurnProjection {
    let mut consumed_result_indexes = HashSet::new();
    let mut projected = Vec::new();
    let mut provenance = Vec::new();
    for (index, result) in results.iter().enumerate() {
        let Some(projection) = project_result(result) else {
            continue;
        };
        consumed_result_indexes.insert(index);
        if attempts
            .iter()
            .any(|attempt| attempt.retry_of == Some(index))
        {
            continue;
        }
        if let SearchResultProjection::Visible(outcome) = projection {
            // 失败候选的只读结果仍留在请求级 diagnostics 中，但没有回填给最终
            // 候选。它们可以参与故障诊断，不能作为最终模型正文的引用来源。
            if index >= final_candidate_result_start
                && outcome.status == ToolOutcomeStatus::Succeeded
            {
                extend_unique_provenance(&mut provenance, provenance_from_output(&result.output));
            }
            projected.push((index, outcome));
        }
    }

    let has_success = projected
        .iter()
        .any(|(_, outcome)| outcome.status == ToolOutcomeStatus::Succeeded);
    let mut visible_failure_keys = HashSet::new();
    for (_, outcome) in &mut projected {
        if outcome.status != ToolOutcomeStatus::Failed {
            continue;
        }
        let failure_key = outcome
            .error_code
            .clone()
            .unwrap_or_else(|| "provider_error".to_owned());
        let hide_empty_beside_success = has_success && failure_key == "empty_result";
        let duplicate_failure = !visible_failure_keys.insert(failure_key);
        if hide_empty_beside_success || duplicate_failure {
            outcome.blocks.clear();
        }
    }

    SearchTurnProjection {
        consumed_result_indexes,
        outcomes: projected,
        provenance,
    }
}

pub(crate) fn project_result(result: &ToolExecutionResult) -> Option<SearchResultProjection> {
    if result.name != WEB_SEARCH_TOOL_NAME {
        return None;
    }
    if result.output.get("deduplicated").and_then(Value::as_bool) == Some(true) {
        // 缓存命中仍保留在 Agent 原始轨迹中，但不是新的搜索结果，不能参与
        // 用户展示、来源生成或整轮成功/失败/超时统计。
        return Some(SearchResultProjection::Hidden);
    }

    Some(SearchResultProjection::Visible(visible_outcome(result)))
}

fn visible_outcome(result: &ToolExecutionResult) -> ToolExecutionOutcome {
    let mut status = ToolOutcomeStatus::from_tool_result(result);
    let mut error_code = structured_error_code(&result.output);
    // 兼容旧工具输出和跨版本累计轨迹：即使上游误标 succeeded，只要没有 answer
    // 或可用 source，就不能作为搜索成功证据，也不能保留模型最终补全文。
    if status == ToolOutcomeStatus::Succeeded && !output_has_evidence(&result.output) {
        status = ToolOutcomeStatus::Failed;
        error_code = Some("empty_result".to_owned());
    }
    let block = match status {
        ToolOutcomeStatus::Succeeded => {
            let formatted = format_web_search_tool_reply_with_sources(&result.output);
            ResponseBlock::FactCard(structured_command_body(formatted.body))
        }
        ToolOutcomeStatus::Skipped => ResponseBlock::Warning(skip_body(&result.output)),
        ToolOutcomeStatus::RequiresClarification => {
            ResponseBlock::Clarification(CommandBody::plain("请说明要联网查询什么内容。"))
        }
        ToolOutcomeStatus::PendingConfirmation | ToolOutcomeStatus::Failed => {
            ResponseBlock::Error(error_body(&result.output, error_code.as_deref()))
        }
    };

    ToolExecutionOutcome {
        tool_name: result.name.clone(),
        domain: "search".to_owned(),
        status,
        effect: ToolEffect::ReadOnly,
        presentation: OutcomePresentation::Trusted,
        blocks: vec![block],
        error_code,
        command: Some("web_search".to_owned()),
    }
}

fn error_body(output: &Value, projected_error_code: Option<&str>) -> CommandBody {
    let code = projected_error_code
        .map(str::to_owned)
        .or_else(|| structured_error_code(output))
        .unwrap_or_else(|| "provider_error".to_owned());
    let stage = output
        .get("error")
        .and_then(|error| error.get("stage"))
        .and_then(Value::as_str)
        .unwrap_or("web_search");
    let err = LlmError::new(code, "web search tool failed", stage);
    let reply = format_web_search_error_reply(&err);
    if json_string_field(output, "mode").as_deref() == Some("multi_entity_research") {
        return structured_command_body(format_web_search_research_error_reply(output, &reply));
    }
    structured_command_body(reply)
}

fn output_has_evidence(output: &Value) -> bool {
    if json_string_field(output, "mode").as_deref() == Some("multi_entity_research") {
        return output
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|item| {
                json_string_field(item, "status").as_deref() == Some("success")
                    && (json_string_field(item, "facts").is_some()
                        || json_string_field(item, "summary").is_some()
                        || json_string_field(item, "answer").is_some()
                        || sources_have_evidence(item.get("sources")))
            });
    }
    json_string_field(output, "answer").is_some() || sources_have_evidence(output.get("sources"))
}

fn sources_have_evidence(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|source| {
            source.as_str().is_some_and(|text| !text.trim().is_empty())
                || json_string_field(source, "title").is_some()
                || json_string_field(source, "url").is_some()
                || json_string_field(source, "snippet").is_some()
        })
}

fn skip_body(output: &Value) -> CommandBody {
    let text = match json_string_field(output, "reason").as_deref() {
        Some("dependency_previous_call_failed") => {
            "联网查询因前序工具失败已跳过；根因以上方失败信息为准。".to_owned()
        }
        Some(reason) => format!("联网查询已跳过：{reason}。"),
        None => "联网查询已跳过。".to_owned(),
    };
    CommandBody::plain(text)
}

fn structured_error_code(output: &Value) -> Option<String> {
    output
        .get("error_code")
        .and_then(Value::as_str)
        .or_else(|| {
            output
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

fn provenance_from_output(output: &Value) -> Vec<ProvenanceSource> {
    // 来源是否已展示由 formatter 在组装字段时返回；后部条目的来源行即使被
    // 1500 字符上限裁掉，也不会因最终正文的偶然子串而误判。
    let formatted = format_web_search_tool_reply_with_sources(output);
    if json_string_field(output, "mode").as_deref() == Some("multi_entity_research") {
        let mut provenance = Vec::new();
        for (result_index, item) in output
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, item)| json_string_field(item, "status").as_deref() == Some("success"))
        {
            let sources = provenance_sources_from_value(
                item.get("sources"),
                Some(result_index),
                &formatted.rendered_sources,
            );
            extend_unique_provenance(&mut provenance, sources);
            if provenance.len() == WEB_SEARCH_TOOL_SOURCE_LIMIT {
                break;
            }
        }
        return provenance;
    }
    let sources =
        provenance_sources_from_value(output.get("sources"), None, &formatted.rendered_sources);
    let mut provenance = Vec::new();
    extend_unique_provenance(&mut provenance, sources);
    provenance
}

/// 来源预算按唯一来源消耗；重复项只合并正文展示标记，并继续扫描后续候选来回填上限。
fn extend_unique_provenance(
    provenance: &mut Vec<ProvenanceSource>,
    sources: impl IntoIterator<Item = ProvenanceSource>,
) {
    for source in sources {
        if provenance.len() >= WEB_SEARCH_TOOL_SOURCE_LIMIT {
            break;
        }
        if let Some(existing) = provenance
            .iter_mut()
            .find(|existing| existing.has_same_identity(&source))
        {
            existing.merge_rendered_markers(&source);
        } else {
            provenance.push(source);
            if provenance.len() == WEB_SEARCH_TOOL_SOURCE_LIMIT {
                break;
            }
        }
    }
}

fn provenance_sources_from_value<'a>(
    value: Option<&'a Value>,
    result_index: Option<usize>,
    markers: &'a [WebSearchRenderedSource],
) -> impl Iterator<Item = ProvenanceSource> + 'a {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(source_index, source)| {
            if let Some(text) = source
                .as_str()
                .map(|text| sanitize_source_field(text, WEB_SEARCH_TOOL_SOURCE_TITLE_MAX_CHARS))
                .filter(|text| !text.is_empty())
            {
                return Some((
                    source_index,
                    ProvenanceSource {
                        title: text.to_owned(),
                        url: String::new(),
                        snippet: String::new(),
                        identity_in_deterministic_body: false,
                        snippet_in_deterministic_body: false,
                    },
                ));
            }
            // Tool 输出可能来自旧版本轨迹或测试注入，不能假定已经过当前搜索入口的
            // 数量和字段裁剪。进入整轮 provenance 前再次建立统一出站边界。
            let title = sanitize_source_field(
                &json_string_field(source, "title").unwrap_or_default(),
                WEB_SEARCH_TOOL_SOURCE_TITLE_MAX_CHARS,
            );
            let url = sanitize_source_url(&json_string_field(source, "url").unwrap_or_default());
            let snippet = sanitize_source_field(
                &json_string_field(source, "snippet").unwrap_or_default(),
                WEB_SEARCH_TOOL_SOURCE_SNIPPET_MAX_CHARS,
            );
            (!title.is_empty() || !url.is_empty() || !snippet.is_empty()).then_some((
                source_index,
                ProvenanceSource {
                    title,
                    url,
                    snippet,
                    identity_in_deterministic_body: false,
                    snippet_in_deterministic_body: false,
                },
            ))
        })
        .map(move |(source_index, mut source)| {
            let location = WebSearchSourceLocation {
                result_index,
                source_index,
            };
            if let Some(marker) = markers.iter().find(|marker| marker.location == location) {
                source.identity_in_deterministic_body = marker.identity_rendered;
                source.snippet_in_deterministic_body = marker.snippet_rendered;
            }
            source
        })
}

fn sanitize_source_field(value: &str, max_chars: usize) -> String {
    truncate_chars_with_ellipsis_trimmed(&sanitize_single_line_visible_text(value), max_chars)
}

fn sanitize_source_url(value: &str) -> String {
    let url = sanitize_single_line_visible_text(value);
    if url.chars().count() <= WEB_SEARCH_PROVENANCE_URL_MAX_CHARS {
        url
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn web_search_result(output: Value) -> ToolExecutionResult {
        ToolExecutionResult {
            name: WEB_SEARCH_TOOL_NAME.to_owned(),
            output,
            succeeded: true,
        }
    }

    fn web_search_fact_text(output: Value) -> String {
        let SearchResultProjection::Visible(outcome) = project_result(&web_search_result(output))
            .expect("web search result should be handled")
        else {
            panic!("expected visible web search result");
        };
        let ResponseBlock::FactCard(body) = &outcome.blocks[0] else {
            panic!("expected web search fact card");
        };
        body.text.clone()
    }

    #[test]
    fn deduplicated_cache_hit_is_hidden() {
        let projection = project_result(&web_search_result(json!({
            "ok": true,
            "deduplicated": true,
            "message": "已使用本次请求中相同检索的已有证据。"
        })))
        .expect("web search result should be handled");

        assert!(matches!(projection, SearchResultProjection::Hidden));
    }

    #[test]
    fn first_empty_search_remains_visible() {
        let projection = project_result(&web_search_result(json!({
            "ok": true,
            "answer": "",
            "sources": []
        })))
        .expect("web search result should be handled");

        assert!(matches!(projection, SearchResultProjection::Visible(_)));
    }

    #[test]
    fn single_search_keeps_top_level_answer_card() {
        let text = web_search_fact_text(json!({
            "answer": "单次搜索的明确答案",
            "sources": []
        }));

        assert!(text.starts_with("【联网查询】"));
        assert!(text.contains("单次搜索的明确答案"));
        assert!(!text.contains("没查到明确结果"));
    }

    #[test]
    fn single_search_with_only_sources_does_not_look_empty() {
        let text = web_search_fact_text(json!({
            "answer": "",
            "sources": [{
                "title": "来源标题",
                "url": "https://example.test/source",
                "snippet": "来源摘要"
            }]
        }));

        assert!(text.contains("来源标题"));
        assert!(text.contains("来源摘要"));
        assert!(!text.contains("没查到明确结果"));
    }

    #[test]
    fn provenance_marks_source_fields_embedded_by_single_search_formatter() {
        let sources = provenance_from_output(&json!({
            "answer": "",
            "sources": [{
                "title": "来源标题",
                "url": "https://example.test/source",
                "snippet": "来源摘要"
            }, {
                "title": "第二来源",
                "url": "https://example.test/second",
                "snippet": "第二摘要"
            }]
        }));

        assert!(sources[0].identity_in_deterministic_body);
        assert!(sources[0].snippet_in_deterministic_body);
        assert!(!sources[1].identity_in_deterministic_body);
        assert!(!sources[1].snippet_in_deterministic_body);
    }

    #[test]
    fn provenance_marks_matching_answer_snippet_without_hiding_source_identity() {
        let sources = provenance_from_output(&json!({
            "answer": "与摘要相同的答案",
            "sources": [{
                "title": "来源标题",
                "url": "https://example.test/source",
                "snippet": "与摘要相同的答案"
            }]
        }));

        assert!(!sources[0].identity_in_deterministic_body);
        assert!(sources[0].snippet_in_deterministic_body);
    }

    #[test]
    fn provenance_sanitizes_and_truncates_external_source_fields() {
        let sources = provenance_from_output(&json!({
            "answer": "搜索答案",
            "sources": [{
                "title": format!("标题\n- 伪造状态\u{0000}{}", "题".repeat(120)),
                "url": "https://example.test/source\n- 伪造来源",
                "snippet": format!("摘要\t下一行\u{200b}{}", "摘".repeat(180))
            }]
        }));

        assert_eq!(sources.len(), 1);
        assert!(!sources[0].title.contains(['\n', '\r', '\t', '\u{0000}']));
        assert!(!sources[0].url.contains(['\n', '\r', '\t']));
        assert!(!sources[0].snippet.contains(['\n', '\r', '\t', '\u{200b}']));
        assert!(sources[0].title.contains("标题 - 伪造状态"));
        assert!(sources[0].url.contains("source - 伪造来源"));
        assert!(sources[0].title.chars().count() <= WEB_SEARCH_TOOL_SOURCE_TITLE_MAX_CHARS);
        assert!(sources[0].snippet.chars().count() <= WEB_SEARCH_TOOL_SOURCE_SNIPPET_MAX_CHARS);
    }

    #[test]
    fn provenance_drops_oversized_url_instead_of_linking_to_a_truncated_address() {
        let sources = provenance_from_output(&json!({
            "answer": "搜索答案",
            "sources": [{
                "title": "仍可展示的来源标题",
                "url": format!("https://example.test/{}", "u".repeat(600)),
                "snippet": "来源摘要"
            }]
        }));

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].title, "仍可展示的来源标题");
        assert!(sources[0].url.is_empty());
        assert_eq!(sources[0].snippet, "来源摘要");
    }

    #[test]
    fn provenance_limits_sources_across_multiple_searches() {
        let results = (0..3)
            .map(|search_index| {
                web_search_result(json!({
                    "answer": format!("搜索答案 {search_index}"),
                    "sources": (0..3).map(|source_index| json!({
                        "title": format!("来源 {search_index}-{source_index}"),
                        "url": format!("https://example.test/{search_index}/{source_index}"),
                        "snippet": "摘要"
                    })).collect::<Vec<_>>()
                }))
            })
            .collect::<Vec<_>>();

        let projection = project_results(&results, &[], 0);

        assert_eq!(projection.provenance.len(), WEB_SEARCH_TOOL_SOURCE_LIMIT);
        assert_eq!(projection.provenance[0].title, "来源 0-0");
        assert_eq!(projection.provenance[3].title, "来源 1-0");
    }

    #[test]
    fn duplicate_urls_do_not_consume_provenance_budget() {
        let result = web_search_result(json!({
            "answer": "搜索答案",
            "sources": [{
                "title": "重复来源 1",
                "url": "https://example.test/duplicate"
            }, {
                "title": "重复来源 2",
                "url": "https://example.test/duplicate"
            }, {
                "title": "重复来源 3",
                "url": "https://example.test/duplicate"
            }, {
                "title": "重复来源 4",
                "url": "https://example.test/duplicate"
            }, {
                "title": "唯一来源 1",
                "url": "https://example.test/unique-1"
            }, {
                "title": "唯一来源 2",
                "url": "https://example.test/unique-2"
            }, {
                "title": "唯一来源 3",
                "url": "https://example.test/unique-3"
            }]
        }));

        let projection = project_results(&[result], &[], 0);

        assert_eq!(projection.provenance.len(), WEB_SEARCH_TOOL_SOURCE_LIMIT);
        assert_eq!(
            projection
                .provenance
                .iter()
                .map(|source| source.url.as_str())
                .collect::<Vec<_>>(),
            vec![
                "https://example.test/duplicate",
                "https://example.test/unique-1",
                "https://example.test/unique-2",
                "https://example.test/unique-3",
            ]
        );
    }

    #[test]
    fn provenance_only_uses_results_seen_by_final_candidate() {
        let results = vec![
            web_search_result(json!({
                "answer": "候选 A 的搜索答案",
                "sources": [{
                    "title": "候选 A 来源",
                    "url": "https://example.test/candidate-a"
                }]
            })),
            web_search_result(json!({
                "answer": "候选 B 的搜索答案",
                "sources": [{
                    "title": "候选 B 来源",
                    "url": "https://example.test/candidate-b"
                }]
            })),
        ];

        let projection = project_results(&results, &[], 1);

        assert_eq!(projection.provenance.len(), 1);
        assert_eq!(projection.provenance[0].title, "候选 B 来源");
        assert_eq!(projection.outcomes.len(), 2);
    }

    #[test]
    fn truncated_research_source_is_not_marked_as_rendered() {
        let output = json!({
            "mode": "multi_entity_research",
            "successful": 2,
            "failed": 0,
            "results": [{
                "entity": "项目甲",
                "status": "success",
                "facts": "前序事实".repeat(350),
                "sources": []
            }, {
                "entity": "项目乙",
                "status": "success",
                "facts": "尾部事实仍然可见".repeat(4),
                "sources": [{
                    "title": "项目乙官方来源",
                    "url": "https://example.test/project-b",
                    "snippet": "项目乙来源摘要"
                }]
            }]
        });

        let text = web_search_fact_text(output.clone());
        let sources = provenance_from_output(&output);

        assert!(text.contains("尾部事实仍然可见"));
        assert!(!text.contains("https://example.test/project-b"));
        assert!(!sources[0].identity_in_deterministic_body);
        assert!(!sources[0].snippet_in_deterministic_body);
    }

    #[test]
    fn multi_entity_search_renders_facts_without_top_level_answer() {
        let text = web_search_fact_text(json!({
            "mode": "multi_entity_research",
            "successful": 1,
            "failed": 0,
            "results": [{
                "entity": "项目甲",
                "status": "success",
                "facts": "项目甲支持能力 A",
                "sources": [{
                    "title": "项目甲文档",
                    "url": "https://example.test/project-a",
                    "snippet": "官方功能摘要"
                }]
            }]
        }));

        assert!(text.starts_with("【联网查询】"));
        assert!(text.contains("项目甲支持能力 A"));
        assert!(text.contains("项目甲文档"));
        assert!(!text.contains("没查到明确结果"));
    }

    #[test]
    fn multi_entity_search_shows_partial_success_counts() {
        let text = web_search_fact_text(json!({
            "mode": "multi_entity_research",
            "successful": "类型异常",
            "failed": null,
            "results": [{
                "entity": "成功项",
                "status": "success",
                "facts": "成功事实"
            }, {
                "entity": "失败项",
                "status": "failed",
                "facts": "不应展示的失败详情",
                "error": {"message": "内部错误"}
            }]
        }));

        assert!(text.starts_with("【联网查询（成功 1，失败 1）】"));
        assert!(text.contains("成功事实"));
        assert!(!text.contains("不应展示的失败详情"));
        assert!(!text.contains("内部错误"));
    }

    #[test]
    fn multi_entity_search_counts_timeout_as_failure() {
        let text = web_search_fact_text(json!({
            "mode": "multi_entity_research",
            "results": [{
                "entity": "成功项",
                "status": "success",
                "facts": "成功事实"
            }, {
                "entity": "超时项",
                "status": "timeout"
            }, {
                "entity": "失败项",
                "status": "failed"
            }]
        }));

        assert!(text.starts_with("【联网查询（成功 1，失败 2）】"));
        assert!(text.contains("成功事实"));
    }

    #[test]
    fn all_failed_multi_entity_search_keeps_friendly_failure_hint() {
        let SearchResultProjection::Visible(outcome) = project_result(&web_search_result(json!({
            "ok": false,
            "mode": "multi_entity_research",
            "successful": 0,
            "failed": 2,
            "results": [{
                "entity": "失败项",
                "status": "failed",
                "error": {"message": "内部错误"}
            }, {
                "entity": "超时项",
                "status": "timeout"
            }]
        })))
        .expect("web search result should be handled") else {
            panic!("expected visible web search result");
        };

        assert_eq!(outcome.status, ToolOutcomeStatus::Failed);
        let ResponseBlock::Error(body) = &outcome.blocks[0] else {
            panic!("expected web search error block");
        };
        assert!(body.text.starts_with("【联网查询（成功 0，失败 2）】"));
        assert!(body.text.contains("联网查询服务暂时不可用"));
        assert!(!body.text.contains("内部错误"));
    }
}

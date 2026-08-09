//! Web Search Tool 的整轮结果投影。
//!
//! 搜索领域在这里决定单次工具轨迹应展示、隐藏还是交给其他领域处理；通用
//! `tools/agent_turn.rs` 只负责调度，不理解 `deduplicated` 等搜索结果字段。

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
        search_flow::{
            format_web_search_error_reply, format_web_search_research_error_reply,
            format_web_search_tool_reply,
        },
    },
};

use super::WEB_SEARCH_TOOL_NAME;

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
            if outcome.status == ToolOutcomeStatus::Succeeded {
                provenance.extend(provenance_from_output(&result.output));
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
        ToolOutcomeStatus::Succeeded => ResponseBlock::FactCard(structured_command_body(
            format_web_search_tool_reply(&result.output),
        )),
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
    if string_field(output, "mode").as_deref() == Some("multi_entity_research") {
        return structured_command_body(format_web_search_research_error_reply(output, &reply));
    }
    structured_command_body(reply)
}

fn output_has_evidence(output: &Value) -> bool {
    if string_field(output, "mode").as_deref() == Some("multi_entity_research") {
        return output
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|item| {
                string_field(item, "status").as_deref() == Some("success")
                    && (string_field(item, "facts").is_some()
                        || string_field(item, "summary").is_some()
                        || string_field(item, "answer").is_some()
                        || sources_have_evidence(item.get("sources")))
            });
    }
    string_field(output, "answer").is_some() || sources_have_evidence(output.get("sources"))
}

fn sources_have_evidence(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|source| {
            source.as_str().is_some_and(|text| !text.trim().is_empty())
                || string_field(source, "title").is_some()
                || string_field(source, "url").is_some()
                || string_field(source, "snippet").is_some()
        })
}

fn skip_body(output: &Value) -> CommandBody {
    let text = match string_field(output, "reason").as_deref() {
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

fn string_field(output: &Value, key: &str) -> Option<String> {
    output
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn provenance_from_output(output: &Value) -> Vec<ProvenanceSource> {
    // 来源去重标记必须以 formatter 截断后的实际正文为准；否则后部条目的来源行
    // 已被 1500 字符上限裁掉时，fallback 仍会误以为来源已经展示。
    let deterministic_body = format_web_search_tool_reply(output);
    if string_field(output, "mode").as_deref() == Some("multi_entity_research") {
        return output
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| string_field(item, "status").as_deref() == Some("success"))
            .flat_map(|item| {
                let mut sources = sources_from_value(item.get("sources"));
                mark_first_source_rendered_fields(&mut sources, &deterministic_body);
                mark_rendered_snippets(&mut sources, &deterministic_body);
                sources
            })
            .collect();
    }
    let answer = string_field(output, "answer");
    let mut sources = sources_from_value(output.get("sources"));
    if answer.is_some() {
        mark_rendered_snippets(&mut sources, &deterministic_body);
    } else {
        mark_first_source_rendered_fields(&mut sources, &deterministic_body);
    }
    sources
}

/// formatter 对无顶层 answer 的单次搜索、以及每个多目标条目，只尝试嵌入第一个
/// 可展示来源。只有完整身份和摘要确实保留在截断后正文里，才标记为已展示。
fn mark_first_source_rendered_fields(sources: &mut [ProvenanceSource], body: &str) {
    if let Some(source) = sources.first_mut() {
        source.identity_in_deterministic_body = source_identity(source)
            .as_deref()
            .is_some_and(|identity| body.contains(identity));
        source.snippet_in_deterministic_body =
            !source.snippet.is_empty() && body.contains(source.snippet.as_str());
    }
}

fn mark_rendered_snippets(sources: &mut [ProvenanceSource], body: &str) {
    for source in sources {
        source.snippet_in_deterministic_body =
            !source.snippet.is_empty() && body.contains(source.snippet.as_str());
    }
}

fn source_identity(source: &ProvenanceSource) -> Option<String> {
    match (source.title.trim(), source.url.trim()) {
        (title, url) if !title.is_empty() && !url.is_empty() => Some(format!("[{title}]({url})")),
        (title, _) if !title.is_empty() => Some(title.to_owned()),
        (_, url) if !url.is_empty() => Some(url.to_owned()),
        _ => None,
    }
}

fn sources_from_value(value: Option<&Value>) -> Vec<ProvenanceSource> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|source| {
            if let Some(text) = source
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                return Some(ProvenanceSource {
                    title: text.to_owned(),
                    url: String::new(),
                    snippet: String::new(),
                    identity_in_deterministic_body: false,
                    snippet_in_deterministic_body: false,
                });
            }
            let title = string_field(source, "title").unwrap_or_default();
            let url = string_field(source, "url").unwrap_or_default();
            let snippet = string_field(source, "snippet").unwrap_or_default();
            (!title.is_empty() || !url.is_empty() || !snippet.is_empty()).then_some(
                ProvenanceSource {
                    title,
                    url,
                    snippet,
                    identity_in_deterministic_body: false,
                    snippet_in_deterministic_body: false,
                },
            )
        })
        .collect()
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

//! Web Search Tool 的结构化结果投影与尺寸压缩。

use qq_maid_common::text::truncate_chars_with_ellipsis_trimmed;
use qq_maid_llm::web_search::{WebSearchOutcome, WebSearchSource};
use serde_json::{Value, json};

use crate::error::LlmError;

use super::{
    WEB_SEARCH_EMPTY_RESULT_MODEL_MESSAGE, WEB_SEARCH_TOOL_SOURCE_LIMIT,
    WEB_SEARCH_TOOL_SOURCE_SNIPPET_MAX_CHARS, WEB_SEARCH_TOOL_SOURCE_TITLE_MAX_CHARS,
};

pub(super) fn web_search_tool_output(
    outcome: &WebSearchOutcome,
    backend: &str,
    output_max_chars: usize,
) -> Value {
    let result_count = outcome
        .sources
        .iter()
        .filter(|source| web_search_source_has_evidence(source))
        .count();
    if !web_search_outcome_has_evidence(outcome) {
        return json!({
            "ok": false,
            "execution_succeeded": true,
            "backend": backend,
            "provider": outcome.provider,
            "answer": "",
            "sources": [],
            "result_count": 0,
            "elapsed_ms": outcome.elapsed_ms,
            "error": {
                "code": "empty_result",
                "stage": "web_search",
                "message": WEB_SEARCH_EMPTY_RESULT_MODEL_MESSAGE,
            },
        });
    }

    let output = json!({
        "ok": true,
        "execution_succeeded": true,
        "backend": backend,
        "provider": outcome.provider,
        "answer": outcome.answer,
        "sources": outcome.sources.iter().map(web_search_source_json).collect::<Vec<_>>(),
        "result_count": result_count,
        "elapsed_ms": outcome.elapsed_ms,
    });
    if serialized_value_chars(&output) <= output_max_chars {
        return output;
    }

    compact_web_search_tool_output(outcome, backend, result_count, output_max_chars)
}

/// Tool Registry 对超限输出只能保留通用 preview，搜索投影将因此失去结构化证据。
/// 搜索领域先压缩重复的来源摘要，并在剩余预算内尽量保留 answer，确保事实卡仍可验真。
fn compact_web_search_tool_output(
    outcome: &WebSearchOutcome,
    backend: &str,
    result_count: usize,
    output_max_chars: usize,
) -> Value {
    let source_candidates = outcome
        .sources
        .iter()
        .filter(|source| web_search_source_has_evidence(source))
        .take(WEB_SEARCH_TOOL_SOURCE_LIMIT)
        .collect::<Vec<_>>();
    let sources = compact_web_search_sources(
        outcome,
        backend,
        result_count,
        output_max_chars,
        &source_candidates,
    );

    let answer_chars = outcome.answer.trim().chars().collect::<Vec<_>>();
    let mut low = 0usize;
    let mut high = answer_chars.len();
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        let answer = answer_chars[..mid].iter().collect::<String>();
        let candidate =
            successful_web_search_output(outcome, backend, result_count, &answer, &sources);
        if serialized_value_chars(&candidate) <= output_max_chars {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let answer = answer_chars[..low].iter().collect::<String>();
    successful_web_search_output(outcome, backend, result_count, &answer, &sources)
}

fn successful_web_search_output(
    outcome: &WebSearchOutcome,
    backend: &str,
    result_count: usize,
    answer: &str,
    sources: &[Value],
) -> Value {
    json!({
        "ok": true,
        "execution_succeeded": true,
        "backend": backend,
        "provider": outcome.provider,
        "answer": answer,
        "sources": sources,
        "result_count": result_count,
        "elapsed_ms": outcome.elapsed_ms,
    })
}

fn compact_web_search_sources(
    outcome: &WebSearchOutcome,
    backend: &str,
    result_count: usize,
    output_max_chars: usize,
    candidates: &[&WebSearchSource],
) -> Vec<Value> {
    let fits = |sources: &[Value]| {
        serialized_value_chars(&successful_web_search_output(
            outcome,
            backend,
            result_count,
            "",
            sources,
        )) <= output_max_chars
    };
    let with_snippets =
        compact_web_search_source_jsons(candidates, WEB_SEARCH_TOOL_SOURCE_SNIPPET_MAX_CHARS);
    if fits(&with_snippets) {
        return with_snippets;
    }

    // URL 必须保持完整；预算不足时先压缩摘要，仍放不下才减少来源。
    let without_snippets = compact_web_search_source_jsons(candidates, 0);
    if fits(&without_snippets) {
        return without_snippets;
    }

    let mut retained = Vec::new();
    for source in candidates {
        let mut candidate = retained.clone();
        candidate.push(*source);
        if fits(&compact_web_search_source_jsons(&candidate, 0)) {
            retained = candidate;
        }
    }
    compact_web_search_source_jsons(&retained, 0)
}

fn compact_web_search_source_jsons(
    sources: &[&WebSearchSource],
    snippet_max_chars: usize,
) -> Vec<Value> {
    sources
        .iter()
        .map(|source| compact_web_search_source_json(source, snippet_max_chars))
        .collect()
}

fn compact_web_search_source_json(source: &WebSearchSource, snippet_max_chars: usize) -> Value {
    let snippet = if snippet_max_chars == 0 {
        String::new()
    } else {
        truncate_chars_with_ellipsis_trimmed(&source.snippet, snippet_max_chars)
    };
    json!({
        "title": truncate_chars_with_ellipsis_trimmed(
            &source.title,
            WEB_SEARCH_TOOL_SOURCE_TITLE_MAX_CHARS,
        ),
        "url": source.url,
        "snippet": snippet,
    })
}

pub(super) fn serialized_value_chars(value: &Value) -> usize {
    serde_json::to_string(value)
        .map(|serialized| serialized.chars().count())
        .unwrap_or(usize::MAX)
}

pub(super) fn web_search_failure_output(backend: &str, attempts: usize, error: &LlmError) -> Value {
    json!({
        "ok": false,
        "execution_succeeded": false,
        "backend": backend,
        "provider": error.upstream_provider().unwrap_or("unknown"),
        "model": error.upstream_model().unwrap_or("configured_default"),
        "answer": "",
        "sources": [],
        "result_count": 0,
        "attempts": attempts,
        "error": {
            "code": error.code,
            "message": error.message,
            "stage": error.stage,
            "kind": error.error_kind(),
            "retriable": error.retriable(),
            "upstream_status": error.upstream_status,
        },
    })
}

pub(super) fn web_search_outcome_has_evidence(outcome: &WebSearchOutcome) -> bool {
    !outcome.answer.trim().is_empty() || outcome.sources.iter().any(web_search_source_has_evidence)
}

fn web_search_source_has_evidence(source: &WebSearchSource) -> bool {
    !source.title.trim().is_empty()
        || !source.url.trim().is_empty()
        || !source.snippet.trim().is_empty()
}

fn web_search_source_json(source: &WebSearchSource) -> Value {
    json!({
        "title": source.title,
        "url": source.url,
        "snippet": source.snippet,
    })
}

//! 多实体搜索的领域编排。
//!
//! 主 Agent 负责给出实体、消歧假设和统一维度；本模块只执行限并发只读搜索、
//! 保留逐项成功/失败/超时，并生成供主 Agent 汇总的紧凑结构化结果。

use std::time::Duration;

use futures::{StreamExt, stream};
use qq_maid_llm::{
    tool::{ToolContext, ToolOutput},
    web_search::{WebSearchOutcome, WebSearchSource},
};
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::error::LlmError;

use super::{
    WEB_SEARCH_QUERY_MAX_LENGTH, WEB_SEARCH_TOOL_NAME, WebSearchTool, WebSearchToolRequest,
    optional_string_field, parse_context_size, parse_max_results,
};

pub(super) const WEB_SEARCH_RESEARCH_MAX_TARGETS: usize = 5;
pub(super) const WEB_SEARCH_RESEARCH_CONCURRENCY: usize = 3;
const WEB_SEARCH_RESEARCH_FACT_MAX_CHARS: usize = 240;
const WEB_SEARCH_RESEARCH_SOURCE_LIMIT: usize = 1;
const WEB_SEARCH_RESEARCH_SOURCE_URL_MAX_CHARS: usize = 200;
const WEB_SEARCH_RESEARCH_SNIPPET_MAX_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResearchTarget {
    pub(super) entity: String,
    pub(super) query: String,
    pub(super) assumption: Option<String>,
}

pub(super) async fn execute_research(
    tool: &WebSearchTool,
    context: &ToolContext,
    arguments: &Value,
    targets: Vec<ResearchTarget>,
) -> Result<ToolOutput, LlmError> {
    let dimensions = parse_comparison_dimensions(arguments.get("comparison_dimensions"))?;
    let raw_question = optional_string_field(arguments, "raw_question");
    let max_results = parse_max_results(arguments.get("max_results"))?;
    let context_size = parse_context_size(arguments.get("context_size"))?;
    let total = targets.len();
    let model = tool
        .model_override
        .as_deref()
        .unwrap_or("configured_default")
        .to_owned();
    let execution_deadline = context.execution_deadline;
    let mut results = stream::iter(targets.into_iter().enumerate().map(|(index, target)| {
        let dimensions = dimensions.clone();
        let raw_question = raw_question.clone();
        let model = model.clone();
        let context_size = context_size.clone();
        async move {
            let started = Instant::now();
            let request = WebSearchToolRequest {
                query: target.query.clone(),
                raw_question: Some(research_question(
                    raw_question.as_deref(),
                    &target,
                    &dimensions,
                )),
                max_results,
                context_size,
                model_override: tool.model_override.clone(),
            };
            let outcome = tool
                .query_stream_for_agent(request, execution_deadline)
                .await;
            let elapsed_ms = duration_ms(started.elapsed());
            log_research_result(index, total, &model, elapsed_ms, &outcome);
            (
                index,
                research_result_json(target, &model, elapsed_ms, outcome),
            )
        }
    }))
    .buffer_unordered(WEB_SEARCH_RESEARCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    results.sort_by_key(|(index, _)| *index);
    let results = results
        .into_iter()
        .map(|(_, result)| result)
        .collect::<Vec<_>>();
    let succeeded = results
        .iter()
        .filter(|result| result["status"] == "success")
        .count();
    Ok(ToolOutput::json(json!({
        "ok": succeeded > 0,
        "mode": "multi_entity_research",
        "comparison_dimensions": dimensions,
        "successful": succeeded,
        "failed": total - succeeded,
        "results": results,
    })))
}

pub(super) fn parse_research_targets(
    value: Option<&Value>,
) -> Result<Option<Vec<ResearchTarget>>, LlmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let items = value.as_array().ok_or_else(|| {
        LlmError::new(
            "bad_tool_arguments",
            "research_targets must be an array or null",
            "tool",
        )
    })?;
    if !(2..=WEB_SEARCH_RESEARCH_MAX_TARGETS).contains(&items.len()) {
        return Err(LlmError::new(
            "bad_tool_arguments",
            format!(
                "research_targets must contain between 2 and {WEB_SEARCH_RESEARCH_MAX_TARGETS} items"
            ),
            "tool",
        ));
    }
    items
        .iter()
        .map(|item| {
            let entity = required_bounded_string(item, "entity", 80)?;
            let query = required_bounded_string(item, "query", WEB_SEARCH_QUERY_MAX_LENGTH)?;
            let assumption = optional_bounded_string(item, "assumption", 160)?;
            Ok(ResearchTarget {
                entity,
                query,
                assumption,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

pub(super) fn parse_comparison_dimensions(value: Option<&Value>) -> Result<Vec<String>, LlmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let items = value.as_array().ok_or_else(|| {
        LlmError::new(
            "bad_tool_arguments",
            "comparison_dimensions must be an array or null",
            "tool",
        )
    })?;
    if items.len() > 8 {
        return Err(LlmError::new(
            "bad_tool_arguments",
            "comparison_dimensions must not contain more than 8 items",
            "tool",
        ));
    }
    items
        .iter()
        .map(|item| {
            let text = item.as_str().map(str::trim).filter(|text| !text.is_empty());
            match text {
                Some(text) if text.chars().count() <= 80 => Ok(text.to_owned()),
                _ => Err(LlmError::new(
                    "bad_tool_arguments",
                    "comparison dimension must be a non-empty string up to 80 characters",
                    "tool",
                )),
            }
        })
        .collect()
}

fn required_bounded_string(value: &Value, key: &str, max_chars: usize) -> Result<String, LlmError> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            LlmError::new(
                "bad_tool_arguments",
                format!("research target requires non-empty {key}"),
                "tool",
            )
        })?;
    if text.chars().count() > max_chars {
        return Err(LlmError::new(
            "bad_tool_arguments",
            format!("research target {key} is too long"),
            "tool",
        ));
    }
    Ok(text.to_owned())
}

fn optional_bounded_string(
    value: &Value,
    key: &str,
    max_chars: usize,
) -> Result<Option<String>, LlmError> {
    let Some(value) = value.get(key) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value.as_str().map(str::trim).ok_or_else(|| {
        LlmError::new(
            "bad_tool_arguments",
            format!("research target {key} must be a string or null"),
            "tool",
        )
    })?;
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > max_chars {
        return Err(LlmError::new(
            "bad_tool_arguments",
            format!("research target {key} is too long"),
            "tool",
        ));
    }
    Ok(Some(text.to_owned()))
}

fn research_question(
    raw_question: Option<&str>,
    target: &ResearchTarget,
    dimensions: &[String],
) -> String {
    let raw_question = raw_question.unwrap_or("多实体公开资料调研");
    let dimensions = if dimensions.is_empty() {
        "未指定，按用户原问题需要调查".to_owned()
    } else {
        dimensions.join("、")
    };
    let assumption = target.assumption.as_deref().unwrap_or("无");
    format!(
        "用户原始问题：{raw_question}\n当前只调查实体：{}\n统一比较维度：{dimensions}\n消歧假设：{assumption}\n只返回该实体可核实的简明事实、来源和不确定项，不要在本次搜索中生成跨实体对比或推荐。",
        target.entity
    )
}

fn research_result_json(
    target: ResearchTarget,
    model: &str,
    elapsed_ms: u64,
    outcome: Result<WebSearchOutcome, LlmError>,
) -> Value {
    match outcome {
        Ok(outcome) => json!({
            "entity": target.entity,
            "assumption": target.assumption,
            "status": "success",
            "model": truncate_chars(model, 100),
            "provider": outcome.provider,
            "elapsed_ms": outcome.elapsed_ms.max(elapsed_ms),
            "facts": truncate_chars(&outcome.answer, WEB_SEARCH_RESEARCH_FACT_MAX_CHARS),
            "sources": outcome.sources.iter().take(WEB_SEARCH_RESEARCH_SOURCE_LIMIT)
                .filter_map(compact_research_source_json).collect::<Vec<_>>(),
        }),
        Err(err) => json!({
            "entity": target.entity,
            "assumption": target.assumption,
            "status": if err.code == "timeout" { "timeout" } else { "failed" },
            "model": truncate_chars(model, 100),
            "elapsed_ms": elapsed_ms,
            "error": {
                "code": err.code,
                "stage": err.stage,
                "message": truncate_chars(&err.message, 200),
            }
        }),
    }
}

fn compact_research_source_json(source: &WebSearchSource) -> Option<Value> {
    // 不能截断 URL 后返回无效引用；异常超长 URL 直接忽略，保留该实体的事实文本。
    (source.url.chars().count() <= WEB_SEARCH_RESEARCH_SOURCE_URL_MAX_CHARS).then(|| {
        json!({
            "title": truncate_chars(&source.title, 60),
            "url": source.url,
            "snippet": truncate_chars(&source.snippet, WEB_SEARCH_RESEARCH_SNIPPET_MAX_CHARS),
        })
    })
}

fn log_research_result(
    index: usize,
    total: usize,
    model: &str,
    elapsed_ms: u64,
    outcome: &Result<WebSearchOutcome, LlmError>,
) {
    match outcome {
        Ok(outcome) => tracing::info!(
            tool = WEB_SEARCH_TOOL_NAME,
            research_item = index + 1,
            research_total = total,
            model,
            provider = outcome.provider,
            elapsed_ms,
            status = "success",
            "web search research item completed"
        ),
        Err(err) => tracing::warn!(
            tool = WEB_SEARCH_TOOL_NAME,
            research_item = index + 1,
            research_total = total,
            model,
            elapsed_ms,
            status = if err.code == "timeout" {
                "timeout"
            } else {
                "failed"
            },
            failure_stage = err.stage,
            error_code = err.code,
            "web search research item failed"
        ),
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let mut truncated = value.chars().take(limit).collect::<String>();
    truncated.push('…');
    truncated
}

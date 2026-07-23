//! Web Search Tool 的整轮结果投影。
//!
//! 搜索领域在这里决定单次工具轨迹应展示、隐藏还是交给其他领域处理；通用
//! `tools/agent_turn.rs` 只负责调度，不理解 `deduplicated` 等搜索结果字段。

use qq_maid_llm::provider::ToolExecutionResult;
use serde_json::Value;

use crate::{
    error::LlmError,
    runtime::respond::{
        agent_outcome::{
            OutcomePresentation, ResponseBlock, ToolEffect, ToolExecutionOutcome, ToolOutcomeStatus,
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
    let status = ToolOutcomeStatus::from_tool_result(result);
    let error_code = structured_error_code(&result.output);
    let block = match status {
        ToolOutcomeStatus::Succeeded => ResponseBlock::FactCard(structured_command_body(
            format_web_search_tool_reply(&result.output),
        )),
        ToolOutcomeStatus::Skipped => ResponseBlock::Warning(skip_body(&result.output)),
        ToolOutcomeStatus::RequiresClarification => {
            ResponseBlock::Clarification(CommandBody::plain("请说明要联网查询什么内容。"))
        }
        ToolOutcomeStatus::PendingConfirmation | ToolOutcomeStatus::Failed => {
            ResponseBlock::Error(error_body(&result.output))
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

fn error_body(output: &Value) -> CommandBody {
    let code = structured_error_code(output).unwrap_or_else(|| "provider_error".to_owned());
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

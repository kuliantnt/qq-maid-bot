//! `knowledge_search` Tool 入口。

use async_trait::async_trait;
use serde_json::{Value, json};

use qq_maid_llm::tool::{Tool, ToolContext, ToolEffect, ToolMetadata, ToolOutput};

use crate::error::LlmError;

use super::{
    KnowledgeEvidence, KnowledgeEvidenceStatus, KnowledgeIndex, KnowledgeTruncationReason,
};

pub const KNOWLEDGE_SEARCH_TOOL_NAME: &str = "knowledge_search";
const MAX_QUERY_CHARS: usize = 2_000;
const MAX_QUERIES: usize = 4;
const MAX_RESULTS: usize = 8;
const MAX_CALLS_PER_REQUEST: usize = 2;
const BODY_TRUNCATION_MARKER: &str = "\n[正文因字符预算已裁剪]";

/// 只读知识证据查询，不负责生成最终答案或写入知识文件。
#[derive(Clone)]
pub struct KnowledgeSearchTool {
    index: KnowledgeIndex,
    output_max_chars: usize,
}

impl KnowledgeSearchTool {
    pub fn new(index: KnowledgeIndex, output_max_chars: usize) -> Self {
        Self {
            index,
            output_max_chars,
        }
    }
}

#[async_trait]
impl Tool for KnowledgeSearchTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: KNOWLEDGE_SEARCH_TOOL_NAME.to_owned(),
            description: "只读检索本地 Markdown 知识库并返回结构化证据。遇到项目知识、配置项、错误码、部署说明或需要核对本地资料的问题时调用；不要把工具结果当成最终答案，必须基于真实证据回答。无命中、低相关、截断或失败时要明确说明证据状态。不要用它处理普通闲聊。".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "独立、具体的知识检索问题；包含错误码、配置项或项目术语时保留原样"
                    },
                    "max_results": {
                        "type": ["integer", "null"],
                        "description": "最多返回的证据项数量，1 到 8；不确定时传 null",
                        "minimum": 1,
                        "maximum": MAX_RESULTS
                    },
                    "additional_queries": {
                        "type": ["array", "null"],
                        "description": "复杂问题可补充 1 到 3 个独立检索表达；结果会统一融合、去重和控制预算",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "maxItems": 3
                    }
                },
                "required": ["query", "max_results", "additional_queries"],
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn max_calls_per_request(&self) -> Option<usize> {
        Some(MAX_CALLS_PER_REQUEST)
    }

    fn deduplication_key(&self, arguments: &Value) -> Option<String> {
        // 查询语义对大小写、连续空白和 additional_queries 顺序不敏感；使用规范化
        // 参数复用 ToolLoopExecutor 的统一只读缓存，避免重复执行和重复注入证据。
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .map(normalize_query)?;
        let mut additional = arguments
            .get("additional_queries")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(normalize_query)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        additional.sort();
        additional.dedup();
        let max_results = arguments
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(MAX_RESULTS as u64);
        serde_json::to_string(&json!({
            "query": query,
            "additional_queries": additional,
            "max_results": max_results,
        }))
        .ok()
    }

    async fn execute(
        &self,
        _context: ToolContext,
        arguments: Value,
    ) -> Result<ToolOutput, LlmError> {
        let query = arguments
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| {
                LlmError::new(
                    "bad_tool_arguments",
                    "knowledge_search requires a non-empty query",
                    "tool",
                )
            })?;
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(LlmError::new(
                "bad_tool_arguments",
                "knowledge_search query is too long",
                "tool",
            ));
        }
        let max_results = parse_max_results(arguments.get("max_results"))?;
        let queries = parse_queries(query, arguments.get("additional_queries"))?;
        let mut evidence = self.index.search_evidence_many(&queries);
        if max_results < evidence.items.len() {
            evidence.items = retain_prioritized_items(evidence.items, max_results);
            evidence.diagnostics.returned_chunk_count = evidence.items.len();
            evidence.diagnostics.source_count = evidence
                .items
                .iter()
                .map(|item| item.relative_path.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len();
            if !evidence
                .diagnostics
                .truncation_reasons
                .contains(&KnowledgeTruncationReason::ResultLimit)
            {
                evidence
                    .diagnostics
                    .truncation_reasons
                    .push(KnowledgeTruncationReason::ResultLimit);
            }
            evidence.status = KnowledgeEvidenceStatus::Truncated;
        }
        Ok(ToolOutput::json(compact_output(
            evidence,
            self.output_max_chars,
        )))
    }
}

fn normalize_query(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn parse_max_results(value: Option<&Value>) -> Result<usize, LlmError> {
    match value {
        None | Some(Value::Null) => Ok(MAX_RESULTS),
        Some(Value::Number(number)) if !number.is_f64() => number
            .as_u64()
            .map(|value| value as usize)
            .filter(|value| (1..=MAX_RESULTS).contains(value))
            .ok_or_else(invalid_max_results),
        _ => Err(invalid_max_results()),
    }
}

fn invalid_max_results() -> LlmError {
    LlmError::new(
        "bad_tool_arguments",
        "max_results must be an integer between 1 and 8 or null",
        "tool",
    )
}

fn parse_queries(primary: &str, value: Option<&Value>) -> Result<Vec<String>, LlmError> {
    let mut queries = vec![primary.to_owned()];
    match value {
        None | Some(Value::Null) => {}
        Some(Value::Array(values)) if (1..MAX_QUERIES).contains(&values.len()) => {
            for value in values {
                let query = value
                    .as_str()
                    .map(str::trim)
                    .filter(|query| !query.is_empty())
                    .ok_or_else(invalid_additional_queries)?;
                if query.chars().count() > MAX_QUERY_CHARS {
                    return Err(invalid_additional_queries());
                }
                if !queries.iter().any(|existing| existing == query) {
                    queries.push(query.to_owned());
                }
            }
        }
        _ => return Err(invalid_additional_queries()),
    }
    Ok(queries)
}

fn invalid_additional_queries() -> LlmError {
    LlmError::new(
        "bad_tool_arguments",
        "additional_queries must be null or an array of 1 to 3 non-empty queries",
        "tool",
    )
}

fn compact_output(mut evidence: KnowledgeEvidence, max_chars: usize) -> Value {
    let original_status = evidence.status;
    let mut budget_truncated = false;
    while serialized_len(&evidence) > max_chars {
        // 章节扩展只用于补充上下文，字符预算不足时优先整项删除，保留主命中。
        if let Some(index) = evidence
            .items
            .iter()
            .rposition(|item| item.recall_type == super::KnowledgeRecallType::Section)
        {
            evidence.items.remove(index);
            budget_truncated = true;
            update_item_diagnostics(&mut evidence);
            mark_character_budget(&mut evidence, original_status);
            continue;
        }

        let Some(index) = evidence
            .items
            .iter()
            .enumerate()
            .max_by_key(|(_, item)| item.body_excerpt.chars().count())
            .map(|(index, _)| index)
        else {
            break;
        };
        let current = evidence.items[index].body_excerpt.clone();
        let next = truncated_excerpt(&current);
        if next == current {
            // 极小字符预算下连裁剪标记也放不下时，最后才移除主命中，避免死循环。
            evidence.items.remove(index);
            budget_truncated = true;
            update_item_diagnostics(&mut evidence);
            mark_character_budget(&mut evidence, original_status);
        } else {
            evidence.items[index].body_excerpt = next;
            budget_truncated = true;
            mark_character_budget(&mut evidence, original_status);
        }
    }

    if budget_truncated {
        // 标记已在每次实际裁剪时写入；保留该分支作为状态变化的显式护栏。
        mark_character_budget(&mut evidence, original_status);
    }
    evidence_value(&evidence)
}

fn mark_character_budget(
    evidence: &mut KnowledgeEvidence,
    original_status: KnowledgeEvidenceStatus,
) {
    if !evidence
        .diagnostics
        .truncation_reasons
        .contains(&KnowledgeTruncationReason::CharacterBudget)
    {
        evidence
            .diagnostics
            .truncation_reasons
            .push(KnowledgeTruncationReason::CharacterBudget);
    }
    if original_status == KnowledgeEvidenceStatus::Ok {
        evidence.status = KnowledgeEvidenceStatus::Truncated;
    }
}

fn retain_prioritized_items(
    items: Vec<super::KnowledgeEvidenceItem>,
    max_results: usize,
) -> Vec<super::KnowledgeEvidenceItem> {
    let mut prioritized = Vec::with_capacity(max_results);
    prioritized.extend(
        items
            .iter()
            .filter(|item| item.recall_type != super::KnowledgeRecallType::Section)
            .take(max_results)
            .cloned(),
    );
    if prioritized.len() < max_results {
        prioritized.extend(
            items
                .into_iter()
                .filter(|item| item.recall_type == super::KnowledgeRecallType::Section)
                .take(max_results - prioritized.len()),
        );
    }
    prioritized
}

fn serialized_len(evidence: &KnowledgeEvidence) -> usize {
    evidence_value(evidence).to_string().chars().count()
}

fn truncated_excerpt(current: &str) -> String {
    let marker_len = BODY_TRUNCATION_MARKER.chars().count();
    let content = current
        .strip_suffix(BODY_TRUNCATION_MARKER)
        .unwrap_or(current);
    let prefix_limit = content.chars().count().saturating_sub(marker_len);
    let prefix_len = (content.chars().count() / 2).min(prefix_limit);
    let mut next = content.chars().take(prefix_len).collect::<String>();
    next.push_str(BODY_TRUNCATION_MARKER);
    next
}

fn update_item_diagnostics(evidence: &mut KnowledgeEvidence) {
    evidence.diagnostics.returned_chunk_count = evidence.items.len();
    evidence.diagnostics.source_count = evidence
        .items
        .iter()
        .map(|item| item.relative_path.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();
}

fn evidence_value(evidence: &KnowledgeEvidence) -> Value {
    let failed = evidence.status == KnowledgeEvidenceStatus::Failed;
    let error_code = evidence
        .failure
        .as_ref()
        .map(|failure| failure.error_code.as_str());
    json!({
        "ok": !failed,
        "status": evidence.status,
        "items": evidence.items,
        "diagnostics": evidence.diagnostics,
        "injection": evidence.injection,
        "failure": evidence.failure,
        "error_code": error_code,
        "message": status_message(evidence.status),
    })
}

fn status_message(status: KnowledgeEvidenceStatus) -> &'static str {
    match status {
        KnowledgeEvidenceStatus::Ok => "已返回本地知识证据。",
        KnowledgeEvidenceStatus::NoHit => "本地知识库没有找到相关证据，不要据此编造结论。",
        KnowledgeEvidenceStatus::LowRelevance => "找到的片段相关性不足，不要把它们当作可靠证据。",
        KnowledgeEvidenceStatus::Truncated => "证据结果已截断，只能基于已返回片段回答并说明限制。",
        KnowledgeEvidenceStatus::Failed => "知识检索失败，不能据此生成知识库结论。",
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use qq_maid_common::identity_context::{
        ConversationKind, ExecutionActorContext, ExecutionConversationContext,
    };
    use qq_maid_llm::tool::Tool;

    use super::*;
    use crate::{
        runtime::tools::knowledge::{KNOWLEDGE_MIGRATIONS, KnowledgeStore, render_context},
        storage::database::SqliteDatabase,
    };

    fn context() -> ToolContext {
        ToolContext {
            task_id: "task-knowledge".to_owned(),
            actor: ExecutionActorContext {
                user_id: Some("user-1".to_owned()),
                group_member_role: None,
            },
            conversation: ExecutionConversationContext {
                platform: "test".to_owned(),
                account_id: None,
                kind: ConversationKind::Private,
                target_id: Some("user-1".to_owned()),
                scope_id: "private:user-1".to_owned(),
                interaction_scope_id: "private:user-1".to_owned(),
            },
            tool_call_id: Some("call-1".to_owned()),
            execution_deadline: None,
        }
    }

    fn tool() -> KnowledgeSearchTool {
        tool_with_content("# 配置\n\n## RAG-504\n\nRAG-504 表示上游请求超时。", 4_000)
    }

    fn tool_with_content(content: &str, output_max_chars: usize) -> KnowledgeSearchTool {
        let base =
            std::env::temp_dir().join(format!("qq-maid-knowledge-tool-{}", uuid::Uuid::new_v4()));
        let knowledge_dir = base.join("knowledge");
        fs::create_dir_all(&knowledge_dir).unwrap();
        fs::write(knowledge_dir.join("guide.md"), content).unwrap();
        let database =
            SqliteDatabase::open_temp("qq-maid-knowledge-tool", KNOWLEDGE_MIGRATIONS).unwrap();
        let index = KnowledgeIndex::new(KnowledgeStore::new(database), Path::new(&knowledge_dir));
        index.sync().unwrap();
        KnowledgeSearchTool::new(index, output_max_chars)
    }

    #[test]
    fn metadata_is_read_only_and_does_not_offer_file_access() {
        let tool = tool();
        assert_eq!(tool.effect(), ToolEffect::ReadOnly);
        assert!(tool.metadata().description.contains("只读"));
        assert!(!tool.metadata().parameters.to_string().contains("path"));
    }

    #[test]
    fn equivalent_queries_share_normalized_deduplication_key() {
        let tool = tool();
        let left = tool.deduplication_key(&json!({
            "query": " RAG-504  是什么 ",
            "max_results": null,
            "additional_queries": ["超时", "错误码"]
        }));
        let right = tool.deduplication_key(&json!({
            "query": "rag-504 是什么",
            "max_results": 8,
            "additional_queries": ["错误码", "超时"]
        }));
        assert_eq!(left, right);
    }

    #[tokio::test]
    async fn returns_structured_evidence_without_answer_generation() {
        let tool = tool();
        let output = tool
            .execute(context(), json!({"query": "RAG-504", "max_results": null}))
            .await
            .unwrap();

        assert!(output.value["ok"].as_bool().unwrap());
        assert_eq!(output.value["status"], "ok");
        assert_eq!(output.value["items"][0]["relative_path"], "guide.md");
        assert!(
            output.value["items"][0]["body_excerpt"]
                .as_str()
                .unwrap()
                .contains("RAG-504")
        );
        assert!(!output.value.to_string().contains("答案："));
        assert!(render_context(&tool.index.search_evidence("RAG-504")).contains("RAG-504"));
    }

    #[tokio::test]
    async fn no_hit_and_bad_arguments_are_explicit() {
        let tool = tool();
        let output = tool
            .execute(
                context(),
                json!({"query": "今晚吃什么", "max_results": null}),
            )
            .await
            .unwrap();
        assert!(output.value["ok"].as_bool().unwrap());
        assert_eq!(output.value["status"], "no_hit");
        assert!(output.value["items"].as_array().unwrap().is_empty());

        let error = tool
            .execute(context(), json!({"query": "", "max_results": null}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "bad_tool_arguments");
    }

    #[tokio::test]
    async fn max_results_prioritizes_lexical_hits_over_adjacent_chunks() {
        let mut content =
            String::from("# 相邻补全\n\n## 参数\n\n前置定义：这是 lexical 命中前的上下文。\n\n");
        for index in 0..30 {
            content.push_str(&format!(
                "普通说明 {index}：这些文字用于把目标推到后续 chunk，不包含目标词。\n"
            ));
        }
        content.push_str("\n真正命中的目标词 KNOWLEDGE-LEXICAL-TARGET 在这里。\n");
        let tool = tool_with_content(&content, 4_000);

        for max_results in [1, 2] {
            let output = tool
                .execute(
                    context(),
                    json!({"query": "KNOWLEDGE-LEXICAL-TARGET", "max_results": max_results}),
                )
                .await
                .unwrap();
            let items = output.value["items"].as_array().unwrap();
            assert!(items.len() <= max_results);
            assert!(items.iter().any(|item| {
                item["recall_type"] == "lexical"
                    && item["body_excerpt"]
                        .as_str()
                        .is_some_and(|body| body.contains("KNOWLEDGE-LEXICAL-TARGET"))
            }));
        }
    }

    #[tokio::test]
    async fn separate_section_hits_remain_primary_and_survive_max_results() {
        let content = "# 相邻主命中回归\n\n\
## 普通前置片段\n\n这是 lexical 主命中前的普通相邻内容，不包含检索目标。\n\n\
## 第一主命中\n\nLEXICAL-PAIR-TARGET 出现在第一个主命中片段。\n\n\
## 第二主命中\n\nLEXICAL-PAIR-TARGET 出现在第二个主命中片段。";
        let tool = tool_with_content(content, 4_000);

        let evidence = tool.index.search_evidence("LEXICAL-PAIR-TARGET");
        let lexical = evidence
            .items
            .iter()
            .filter(|item| item.recall_type == super::super::KnowledgeRecallType::Lexical)
            .collect::<Vec<_>>();
        assert_eq!(lexical.len(), 2);
        assert!(lexical.iter().all(|item| item.score.is_some()));
        assert!(!evidence.items.iter().any(|item| {
            item.recall_type == super::super::KnowledgeRecallType::Section
                && item.body_excerpt.contains("普通相邻内容")
        }));

        let output = tool
            .execute(
                context(),
                json!({"query": "LEXICAL-PAIR-TARGET", "max_results": 2}),
            )
            .await
            .unwrap();
        let items = output.value["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|item| item["recall_type"] == "lexical"));
        assert!(items.iter().all(|item| item["score"].is_number()));
    }

    fn evidence_for_compaction(status: KnowledgeEvidenceStatus) -> KnowledgeEvidence {
        let long_body = "正文证据。".repeat(700);
        KnowledgeEvidence {
            status,
            items: vec![
                super::super::KnowledgeEvidenceItem {
                    chunk_id: "lexical".to_owned(),
                    relative_path: "guide.md".to_owned(),
                    document_title: None,
                    heading_path: None,
                    start_line: Some(1),
                    end_line: Some(2),
                    score: Some(1.0),
                    recall_type: super::super::KnowledgeRecallType::Lexical,
                    body_excerpt: long_body.clone(),
                },
                super::super::KnowledgeEvidenceItem {
                    chunk_id: "adjacent".to_owned(),
                    relative_path: "guide.md".to_owned(),
                    document_title: None,
                    heading_path: None,
                    start_line: Some(3),
                    end_line: Some(4),
                    score: None,
                    recall_type: super::super::KnowledgeRecallType::Section,
                    body_excerpt: long_body,
                },
            ],
            diagnostics: Default::default(),
            injection: super::super::KnowledgeInjectionDecision {
                allow_injection: status != KnowledgeEvidenceStatus::Failed,
                reason: if status == KnowledgeEvidenceStatus::Failed {
                    super::super::KnowledgeInjectionReason::SearchFailed
                } else {
                    super::super::KnowledgeInjectionReason::LexicalHighConfidence
                },
                threshold_version: "test".to_owned(),
            },
            failure: (status == KnowledgeEvidenceStatus::Failed).then(|| {
                super::super::KnowledgeEvidenceFailure {
                    error_code: "knowledge_db_error".to_owned(),
                }
            }),
        }
    }

    #[test]
    fn compact_output_reports_character_budget_and_marks_body() {
        let value = compact_output(evidence_for_compaction(KnowledgeEvidenceStatus::Ok), 1_200);
        assert!(value.to_string().chars().count() <= 1_200);
        assert_eq!(value["status"], "truncated");
        assert_eq!(
            value["diagnostics"]["truncation_reasons"][0],
            "character_budget"
        );
        assert!(value["items"].to_string().contains("正文因字符预算已裁剪"));
        assert_eq!(value["items"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn compact_output_preserves_non_ok_statuses() {
        for status in [
            KnowledgeEvidenceStatus::LowRelevance,
            KnowledgeEvidenceStatus::Failed,
        ] {
            let value = compact_output(evidence_for_compaction(status), 1_200);
            assert!(value.to_string().chars().count() <= 1_200);
            assert_eq!(value["status"], serde_json::to_value(status).unwrap());
            assert_ne!(value["status"], "truncated");
        }
    }
}

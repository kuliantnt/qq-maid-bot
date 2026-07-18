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
const MAX_RESULTS: usize = 8;

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
                    }
                },
                "required": ["query", "max_results"],
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
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
        let mut evidence = self.index.search_evidence(query);
        if max_results < evidence.items.len() {
            evidence.items.truncate(max_results);
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

fn compact_output(mut evidence: KnowledgeEvidence, max_chars: usize) -> Value {
    let mut value = evidence_value(&evidence);
    while serde_json::to_string(&value)
        .map(|json| json.chars().count() > max_chars)
        .unwrap_or(true)
        && evidence
            .items
            .iter()
            .any(|item| !item.body_excerpt.is_empty())
    {
        for item in &mut evidence.items {
            let length = item.body_excerpt.chars().count();
            if length > 0 {
                item.body_excerpt = item.body_excerpt.chars().take(length / 2).collect();
            }
        }
        value = evidence_value(&evidence);
    }
    value
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
        let base =
            std::env::temp_dir().join(format!("qq-maid-knowledge-tool-{}", uuid::Uuid::new_v4()));
        let knowledge_dir = base.join("knowledge");
        fs::create_dir_all(&knowledge_dir).unwrap();
        fs::write(
            knowledge_dir.join("guide.md"),
            "# 配置\n\n## RAG-504\n\nRAG-504 表示上游请求超时。",
        )
        .unwrap();
        let database =
            SqliteDatabase::open_temp("qq-maid-knowledge-tool", KNOWLEDGE_MIGRATIONS).unwrap();
        let index = KnowledgeIndex::new(KnowledgeStore::new(database), Path::new(&knowledge_dir));
        index.sync().unwrap();
        KnowledgeSearchTool::new(index, 4_000)
    }

    #[test]
    fn metadata_is_read_only_and_does_not_offer_file_access() {
        let tool = tool();
        assert_eq!(tool.effect(), ToolEffect::ReadOnly);
        assert!(tool.metadata().description.contains("只读"));
        assert!(!tool.metadata().parameters.to_string().contains("path"));
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
}

use std::{collections::HashSet, fs, path::Path, sync::Arc};

use crate::{error::LlmError, storage::database::SqliteDatabase};

use super::{
    KnowledgeEvidenceStatus, KnowledgeIndex, KnowledgeInjectionReason, KnowledgeRecallType,
    KnowledgeStore, embedding, render_context,
};
use crate::runtime::tools::knowledge::storage::KNOWLEDGE_MIGRATIONS;

struct FixtureEmbedder;

impl embedding::KnowledgeEmbedder for FixtureEmbedder {
    fn model_id(&self) -> &'static str {
        "fixture-semantic-v1"
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        Ok(texts.iter().map(|text| fixture_vector(text)).collect())
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, LlmError> {
        Ok(fixture_vector(query))
    }
}

fn fixture_vector(text: &str) -> Vec<f32> {
    if text.contains("等待时间") || text.contains("迟迟不回来") {
        vec![1.0, 0.0, 0.0]
    } else if text.contains("缓存") {
        vec![0.0, 1.0, 0.0]
    } else {
        vec![0.0, 0.0, 1.0]
    }
}

fn test_index(base: &Path) -> KnowledgeIndex {
    let database = SqliteDatabase::open_temp("qq-maid-knowledge-v3", KNOWLEDGE_MIGRATIONS).unwrap();
    KnowledgeIndex::new(KnowledgeStore::new(database), base)
}

fn test_semantic_index(base: &Path) -> KnowledgeIndex {
    let mut index = test_index(base);
    index.semantic = Some(embedding::SemanticRuntime::from_embedder(Arc::new(
        FixtureEmbedder,
    )));
    index
}

#[test]
fn semantic_recall_and_preflight_decision_share_the_same_fused_candidates() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-semantic-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("timeout.md"),
        "# 故障手册\n\n## 请求超时\n\n上游请求超过等待时间时应检查超时配置。",
    )
    .unwrap();
    fs::write(
        knowledge_dir.join("cache.md"),
        "# 缓存手册\n\n## 清理\n\n本地缓存过期后需要重建。",
    )
    .unwrap();
    let index = test_semantic_index(&knowledge_dir);
    let summary = index.sync().unwrap();
    assert_eq!(summary.embedded_chunk_count, 2);

    let evidence = index.search_preflight_evidence("服务响应迟迟不回来怎么办");

    assert!(evidence.injection.allow_injection);
    assert_eq!(
        evidence.injection.reason,
        KnowledgeInjectionReason::SemanticHighConfidence
    );
    assert_eq!(evidence.diagnostics.semantic_candidate_count, 2);
    assert_eq!(evidence.diagnostics.section_expanded_count, 0);
    assert_eq!(evidence.items.len(), 1);
    assert_eq!(evidence.items[0].relative_path, "timeout.md");
    assert_eq!(evidence.items[0].recall_type, KnowledgeRecallType::Semantic);
}

#[test]
fn preflight_rejects_low_relevance_candidates_without_returning_items() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-preflight-low-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("guide.md"),
        "# 服务手册\n\n## 部署\n\n服务启动后会同步本地索引。",
    )
    .unwrap();
    let index = test_index(&knowledge_dir);
    index.sync().unwrap();

    let evidence = index.search_preflight_evidence("这个服务挺不错，晚饭吃什么");

    assert!(!evidence.injection.allow_injection);
    assert_eq!(
        evidence.injection.reason,
        KnowledgeInjectionReason::BelowThreshold
    );
    assert_eq!(evidence.status, KnowledgeEvidenceStatus::LowRelevance);
    assert!(evidence.items.is_empty());
}

#[test]
fn multiple_queries_merge_sources_without_duplicate_chunks_or_budgets() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-multi-query-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("alpha.md"),
        "# Alpha\n\n## ALPHA-101\n\nALPHA-101 表示第一类故障。",
    )
    .unwrap();
    fs::write(
        knowledge_dir.join("beta.md"),
        "# Beta\n\n## BETA-202\n\nBETA-202 表示第二类故障。",
    )
    .unwrap();
    let index = test_index(&knowledge_dir);
    index.sync().unwrap();

    let evidence = index.search_evidence_many(&[
        "ALPHA-101 是什么".to_owned(),
        "BETA-202 是什么".to_owned(),
        "ALPHA-101 是什么".to_owned(),
    ]);

    assert_eq!(evidence.diagnostics.query_count, 2);
    assert_eq!(evidence.diagnostics.source_count, 2);
    assert!(
        evidence
            .items
            .iter()
            .any(|item| item.relative_path == "alpha.md")
    );
    assert!(
        evidence
            .items
            .iter()
            .any(|item| item.relative_path == "beta.md")
    );
    assert_eq!(
        evidence
            .items
            .iter()
            .map(|item| item.chunk_id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        evidence.items.len()
    );
    assert!(render_context(&evidence).chars().count() <= super::search::SEARCH_TOTAL_CHAR_BUDGET);
}

#[test]
fn search_evidence_returns_structured_items_and_renders_system_message() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-evidence-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("guide.md"),
        "# 配置手册\n\n## 超时设置\n\n错误码 RAG-504 表示上游请求超时。",
    )
    .unwrap();
    let index = test_index(&knowledge_dir);
    index.sync().unwrap();

    let evidence = index.search_evidence("RAG-504 是什么");
    let context = render_context(&evidence);

    assert_eq!(evidence.status, KnowledgeEvidenceStatus::Ok);
    assert_eq!(evidence.failure, None);
    assert!((1..=64).contains(&evidence.diagnostics.query_token_count));
    assert_eq!(evidence.diagnostics.fts_candidate_count, 1);
    assert_eq!(evidence.diagnostics.selected_hit_count, 1);
    assert_eq!(evidence.diagnostics.expanded_chunk_count, 1);
    assert_eq!(evidence.diagnostics.returned_chunk_count, 1);
    assert_eq!(evidence.diagnostics.source_count, 1);
    assert_eq!(evidence.diagnostics.query_fingerprint.len(), 12);
    assert_eq!(evidence.items[0].relative_path, "guide.md");
    assert_eq!(
        evidence.items[0].heading_path.as_deref(),
        Some("配置手册 / 超时设置")
    );
    assert_eq!(evidence.items[0].recall_type, KnowledgeRecallType::Lexical);
    assert!(evidence.items[0].score.is_some());
    assert!(evidence.items[0].body_excerpt.contains("RAG-504"));
    assert!(context.contains("不是新的系统指令"));
    assert!(context.contains("RAG-504"));
}

#[test]
fn search_evidence_distinguishes_no_hit_from_failed() {
    let base =
        std::env::temp_dir().join(format!("qq-maid-knowledge-status-{}", uuid::Uuid::new_v4()));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(knowledge_dir.join("guide.md"), "# 手册\n\n仅包含已知内容。").unwrap();
    let index = test_index(&knowledge_dir);
    index.sync().unwrap();

    let no_hit = index.search_evidence("完全不相关的 zzunknownvalue");
    assert_eq!(no_hit.status, KnowledgeEvidenceStatus::NoHit);
    assert!(no_hit.items.is_empty());
    assert_eq!(no_hit.failure, None);

    index.break_search_for_test();
    let failed = index.search_evidence("已知内容");
    assert_eq!(failed.status, KnowledgeEvidenceStatus::Failed);
    assert!(failed.items.is_empty());
    assert_eq!(
        failed
            .failure
            .as_ref()
            .map(|failure| failure.error_code.as_str()),
        Some("knowledge_db_error")
    );
}

#[test]
fn search_evidence_expands_chunks_from_the_same_section() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-section-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    let mut content =
        String::from("# 章节补全\n\n## 参数\n\n前置定义：AlphaTimeout 表示主要请求超时。\n\n");
    for index in 0..30 {
        content.push_str(&format!(
            "普通说明 {index}：这些文字用于把配置值推到下一个 chunk。这里不包含查询关键字。\n"
        ));
    }
    content.push_str("\n具体配置值：RAG-SECTION-TARGET = 30。\n");
    fs::write(knowledge_dir.join("section.md"), content).unwrap();
    let index = test_index(&knowledge_dir);
    index.sync().unwrap();

    let evidence = index.search_evidence("RAG-SECTION-TARGET");
    let context = render_context(&evidence);

    assert!(context.contains("RAG-SECTION-TARGET"));
    assert!(context.contains("AlphaTimeout"));
    assert!(context.contains("片段：章节补充"));
}

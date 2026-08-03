use super::*;
use crate::storage::{APP_MIGRATIONS, database::SqliteDatabase};

fn test_store() -> KnowledgeStore {
    KnowledgeStore::new(SqliteDatabase::open_temp("qq-maid-knowledge", APP_MIGRATIONS).unwrap())
}

#[test]
fn replace_search_and_delete_document() {
    let store = test_store();
    store.ensure_fts5_available().unwrap();
    store
        .replace_document(
            "example.md",
            "file-hash",
            Some("2026-06-26T00:00:00Z"),
            &[KnowledgeChunkDraft {
                chunk_id: "example-md-0001-abcd".to_owned(),
                relative_path: "example.md".to_owned(),
                document_title: Some("知识示例".to_owned()),
                heading_path: Some("知识示例 / 中文检索".to_owned()),
                chunk_index: 0,
                chunk_type: "text".to_owned(),
                body: "编号 RAG-407 用于验证中文知识检索。".to_owned(),
                content_hash: "chunk-hash".to_owned(),
                file_hash: "file-hash".to_owned(),
                modified_at: Some("2026-06-26T00:00:00Z".to_owned()),
                start_line: Some(3),
                end_line: Some(3),
                code_language: None,
                chunking_version: 2,
                search_text: "编号 rag 407 中文 检索 知识".to_owned(),
            }],
        )
        .unwrap();

    assert_eq!(store.chunk_count().unwrap(), 1);
    assert_eq!(
        store
            .document_state("example.md")
            .unwrap()
            .unwrap()
            .file_hash,
        "file-hash"
    );
    let results = store.search("rag 407", 5).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].relative_path, "example.md");
    assert_eq!(results[0].chunk_index, 0);
    assert_eq!(results[0].start_line, Some(3));

    let missing = store
        .missing_embedding_sources("fixture-model", 1, 1)
        .unwrap();
    assert_eq!(missing.len(), 1);
    store
        .upsert_embeddings(
            "fixture-model",
            1,
            &[KnowledgeEmbeddingRecord {
                chunk_id: missing[0].chunk_id.clone(),
                content_hash: missing[0].content_hash.clone(),
                vector: vec![1.0, 0.0],
            }],
        )
        .unwrap();
    assert!(
        store
            .missing_embedding_sources("fixture-model", 1, 1)
            .unwrap()
            .is_empty()
    );
    let semantic = store
        .semantic_search("fixture-model", 1, &[1.0, 0.0], 5)
        .unwrap();
    assert_eq!(semantic.len(), 1);
    assert_eq!(semantic[0].origin, KnowledgeSearchOrigin::Semantic);
    assert!((semantic[0].score - 1.0).abs() < f64::EPSILON);

    store.delete_document("example.md").unwrap();
    assert_eq!(store.chunk_count().unwrap(), 0);
    assert!(store.search("rag 407", 5).unwrap().is_empty());
    assert!(
        store
            .semantic_search("fixture-model", 1, &[1.0, 0.0], 5)
            .unwrap()
            .is_empty()
    );
}

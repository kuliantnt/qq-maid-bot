use std::sync::Arc;

use super::chunking::{ChunkingError, chunk_markdown, chunk_markdown_with_limit};
use super::text::build_index_text;
use super::*;
use crate::storage::{APP_MIGRATIONS, database::SqliteDatabase};

fn test_index(base: &Path) -> KnowledgeIndex {
    let db = SqliteDatabase::open_temp("qq-maid-knowledge-runtime", APP_MIGRATIONS).unwrap();
    KnowledgeIndex::new(KnowledgeStore::new(db), base)
}

struct FixedEmbedder;

impl embedding::KnowledgeEmbedder for FixedEmbedder {
    fn model_id(&self) -> &'static str {
        "test-managed-model"
    }

    fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn embed_query(&self, _query: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![1.0, 0.0])
    }
}

struct FailingEmbedder;

impl embedding::KnowledgeEmbedder for FailingEmbedder {
    fn model_id(&self) -> &'static str {
        "test-failing-model"
    }

    fn embed_documents(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
        Err(LlmError::new(
            "knowledge_embedding_error",
            "test embedding failure",
            "knowledge",
        ))
    }

    fn embed_query(&self, _query: &str) -> Result<Vec<f32>, LlmError> {
        Err(LlmError::new(
            "knowledge_embedding_error",
            "test embedding failure",
            "knowledge",
        ))
    }
}

#[test]
fn managed_chunking_rejects_excessive_chunk_count_before_index_write() {
    let index = test_index(Path::new("managed-too-many-chunks"));
    let content = (0..=MAX_MANAGED_CHUNKS)
        .map(|index| format!("# Section {index}\n\nchunk-marker-{index}\n"))
        .collect::<String>();
    let error = index
        .process_managed_file(
            "00000000-0000-4000-8000-000000000003",
            "too-many-chunks.md",
            content.as_bytes(),
            content.len() + 1,
            None,
        )
        .unwrap_err();
    assert_eq!(error.code, "too_many_chunks");
    assert_eq!(index.store.chunk_count().unwrap(), 0);
    assert_eq!(
        index
            .database()
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM knowledge_documents", [], |row| row
                .get::<_, i64>(0),)
            .unwrap(),
        0
    );
}

#[test]
fn fenced_code_single_line_is_split_by_character_limit() {
    let content = format!("```text\n{}\n```", "x".repeat(5_000));

    let chunks = chunk_markdown("long-code.md", &content);

    assert!(chunks.len() > 1);
    assert!(
        chunks
            .iter()
            .all(|chunk| chunk.body.chars().count() <= 1_400)
    );
}

#[test]
fn fenced_code_single_line_honors_chunk_count_limit_before_splitting() {
    let content = format!("```text\n{}", "x".repeat(5_000));

    let result = chunk_markdown_with_limit("long-code.md", &content, 2);

    assert!(matches!(result, Err(ChunkingError::TooManyChunks)));
}

#[test]
fn managed_chunks_search_by_original_filename_without_internal_key_terms() {
    let index = test_index(Path::new("managed-filename"));
    let file_id = "00000000-0000-4000-8000-000000000642";
    let document_key = managed_document_key(file_id);

    index
        .process_managed_file(
            file_id,
            "deployment-handbook-642.md",
            "# 运维手册\n\n正文只包含中文说明。".as_bytes(),
            1024,
            None,
        )
        .unwrap();

    let search_text: String = index
        .database()
        .connection()
        .unwrap()
        .query_row(
            "SELECT c.search_text
             FROM knowledge_chunks c
             JOIN knowledge_documents d ON d.id = c.document_id
             WHERE d.relative_path = ?1",
            [&document_key],
            |row| row.get(0),
        )
        .unwrap();
    assert!(search_text.contains("deployment"));
    assert!(search_text.contains("handbook"));
    assert!(!search_text.contains("managed"));
    assert!(!search_text.contains(&document_key["managed/".len()..]));
}

#[test]
fn managed_embedding_success_is_atomic_and_failure_leaves_no_derived_rows() {
    let mut successful = test_index(Path::new("managed-success"));
    successful.semantic = Some(embedding::SemanticRuntime::from_embedder(Arc::new(
        FixedEmbedder,
    )));
    let result = successful
        .process_managed_file(
            "00000000-0000-4000-8000-000000000001",
            "managed.md",
            b"# Managed\n\nmanaged-embedding-marker",
            1024,
            None,
        )
        .unwrap();
    assert_eq!(result.embedding_count, result.chunk_count);
    let embedding_count: i64 = successful
        .database()
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM knowledge_chunk_embeddings",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(embedding_count as usize, result.embedding_count);

    let mut failing = test_index(Path::new("managed-failure"));
    failing.semantic = Some(embedding::SemanticRuntime::from_embedder(Arc::new(
        FailingEmbedder,
    )));
    let error = failing
        .process_managed_file(
            "00000000-0000-4000-8000-000000000002",
            "managed.md",
            b"# Managed\n\nmanaged-embedding-failure-marker",
            1024,
            None,
        )
        .unwrap_err();
    assert_eq!(error.code, "knowledge_embedding_error");
    assert_eq!(failing.store.chunk_count().unwrap(), 0);
    let embedding_count: i64 = failing
        .database()
        .connection()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM knowledge_chunk_embeddings",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(embedding_count, 0);
}

#[test]
fn markdown_chunks_follow_headings_and_are_stable() {
    let chunks = chunk_markdown(
        "guide/example.md",
        "# 示例知识\n\n## 中文检索\n\n女仆编号 RAG-407 负责整理本地 Markdown。\n\n## Mixed API\n\nOpenAI Web Search 与 SQLite FTS5 可以同时存在。",
    );

    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].document_title.as_deref(), Some("示例知识"));
    assert_eq!(
        chunks[0].heading_path.as_deref(),
        Some("示例知识 / 中文检索")
    );
    assert!(chunks[0].chunk_id.starts_with("guide-example-md-"));
    assert!(chunks[0].chunk_id.contains(":0000:"));
    assert!(chunks[0].search_text.contains("rag"));
    assert!(chunks[0].search_text.contains("女仆"));
}

#[test]
fn scan_skips_committed_example_markdown_templates() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-example-skip-{}",
        uuid::Uuid::new_v4()
    ));
    let nested = base.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(base.join("real.md"), "真实知识").unwrap();
    fs::write(base.join("template.example.md"), "公开模板").unwrap();
    fs::write(nested.join("template.example.markdown"), "公开模板").unwrap();

    let files = scan_markdown_files(&base).unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].relative_path, "real.md");
}

#[cfg(unix)]
#[test]
fn scan_skips_symbolic_links() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-symlink-skip-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    let external_dir = base.join("private");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::create_dir_all(&external_dir).unwrap();
    fs::write(knowledge_dir.join("real.md"), "目录内真实知识").unwrap();
    fs::write(external_dir.join("secret.md"), "目录外私有知识").unwrap();
    std::os::unix::fs::symlink(&external_dir, knowledge_dir.join("linked-dir")).unwrap();
    std::os::unix::fs::symlink(
        external_dir.join("secret.md"),
        knowledge_dir.join("linked-file.md"),
    )
    .unwrap();

    let files = scan_markdown_files(&knowledge_dir).unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].relative_path, "real.md");
}

#[test]
fn chunk_id_keeps_slug_readable_but_uses_path_hash_for_uniqueness() {
    let left = chunk_markdown("a-b.md", "相同正文用于验证 chunk id 不碰撞。");
    let right = chunk_markdown("a/b.md", "相同正文用于验证 chunk id 不碰撞。");
    let chinese_left = chunk_markdown("甲.md", "相同正文用于验证中文路径不碰撞。");
    let chinese_right = chunk_markdown("乙.md", "相同正文用于验证中文路径不碰撞。");

    assert!(left[0].chunk_id.starts_with("a-b-md-"));
    assert!(right[0].chunk_id.starts_with("a-b-md-"));
    assert_ne!(left[0].chunk_id, right[0].chunk_id);
    assert!(chinese_left[0].chunk_id.starts_with("md-"));
    assert!(chinese_right[0].chunk_id.starts_with("md-"));
    assert_ne!(chinese_left[0].chunk_id, chinese_right[0].chunk_id);
}

#[test]
fn markdown_chunks_aggregate_short_paragraphs_within_same_heading() {
    let chunks = chunk_markdown(
        "guide.md",
        "# 指南\n\n## 配置\n\n第一段说明超时配置。\n\n第二段说明重试配置。\n\n## 部署\n\n部署段落包含足够文字用于单独成块。",
    );

    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].body.contains("第一段说明超时配置。"));
    assert!(chunks[0].body.contains("第二段说明重试配置。"));
    assert!(!chunks[0].body.contains("部署段落"));
    assert_eq!(chunks[0].chunk_index, 0);
    assert_eq!(chunks[1].chunk_index, 1);
}

#[test]
fn markdown_chunks_split_oversized_code_fence_and_repair_fences() {
    let mut content = String::from("# 代码\n\n```rust\n");
    for index in 0..70 {
        content.push_str(&format!("let value_{index} = {index};\n"));
    }
    content.push_str("```\n");

    let chunks = chunk_markdown("code.md", &content);

    assert!(chunks.len() >= 2);
    for chunk in &chunks {
        assert_eq!(chunk.chunk_type, "code");
        assert_eq!(chunk.code_language.as_deref(), Some("rust"));
        assert!(chunk.body.starts_with("```rust\n"));
        assert!(chunk.body.ends_with("```"));
    }
    assert!(chunks[0].body.contains("value_0"));
    assert!(chunks.last().unwrap().body.contains("value_69"));
}

#[test]
fn markdown_chunks_split_unclosed_code_fence_without_repeating_last_line() {
    let mut content = String::from("# 代码\n\n```rust\n");
    for index in 0..70 {
        content.push_str(&format!("let value_{index} = {index};\n"));
    }
    content.push_str("let last_unique_rag_token = 70;");

    let chunks = chunk_markdown("unclosed-code.md", &content);

    assert!(chunks.len() >= 2);
    for chunk in &chunks {
        assert_eq!(chunk.chunk_type, "code");
        assert_eq!(chunk.code_language.as_deref(), Some("rust"));
        assert!(chunk.body.starts_with("```rust\n"));
        assert!(chunk.body.ends_with("```"));
    }
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| chunk.body.contains("last_unique_rag_token"))
            .count(),
        1
    );
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| chunk.search_text.contains("last_unique_rag_token"))
            .count(),
        1
    );
}

#[test]
fn markdown_chunks_preserve_short_valuable_config_items() {
    let chunks = chunk_markdown(
        "config.md",
        "# 配置\n\n---\n\nREQUEST_TIMEOUT\n\n/foo\n\nE1001\n\nconfig.toml\n\ntimeout = 30",
    );

    let body = chunks
        .iter()
        .map(|chunk| chunk.body.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(body.contains("REQUEST_TIMEOUT"));
    assert!(body.contains("/foo"));
    assert!(body.contains("E1001"));
    assert!(body.contains("config.toml"));
    assert!(body.contains("timeout = 30"));
    assert!(!body.contains("---"));
}

#[test]
fn markdown_frontmatter_is_not_returned_as_body_but_metadata_can_recall_content() {
    let chunks = chunk_markdown(
        "DID.md",
        "\u{feff}---\n\
title: DID\n\
synonyms:\n\
  - 解离性身份障碍\n\
  - DID\n\
  - did\n\
  - did是什么病\n\
  - did多重人格\n\
aliases: [多重人格, DID]\n\
ignored:\n\
  nested: value\n\
---\n\n\
> 触发警告：以下内容涉及精神健康。\n\n\
## DID 是什么病？\n\n\
DID 是解离性身份障碍的英文缩写。",
    );

    assert_eq!(chunks.len(), 2);
    assert!(chunks[0].body.contains("触发警告"));
    assert!(chunks[1].body.contains("DID 是解离性身份障碍"));
    let combined_body = chunks
        .iter()
        .map(|chunk| chunk.body.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!combined_body.contains("synonyms:"));
    assert!(!combined_body.contains("did多重人格"));
    assert!(!combined_body.contains("nested: value"));
    assert!(!chunks[0].search_text.contains("多重人格"));
    assert!(chunks[1].search_text.contains("重人"));
    assert_eq!(
        chunks[1]
            .search_text
            .split_whitespace()
            .filter(|token| *token == "did")
            .count(),
        1
    );
    assert!(!chunks[1].search_text.contains("synonyms"));
    assert!(!chunks[1].search_text.contains("nested"));
}

#[test]
fn markdown_unclosed_frontmatter_marker_is_treated_as_normal_content() {
    let chunks = chunk_markdown(
        "broken.md",
        "---\ntitle: 未闭合元数据\n\n## 正文\n\n正文不能因为缺少闭合标记被丢弃。",
    );

    let combined_body = chunks
        .iter()
        .map(|chunk| chunk.body.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(combined_body.contains("title: 未闭合元数据"));
    assert!(combined_body.contains("正文不能因为缺少闭合标记被丢弃"));
}

#[test]
fn markdown_code_fence_dashes_are_not_treated_as_frontmatter() {
    let chunks = chunk_markdown(
        "code-yaml.md",
        "```yaml\n---\ntitle: fenced\n---\n```\n\n正文保留在代码块之后。",
    );

    assert!(chunks[0].body.contains("```yaml"));
    assert!(chunks[0].body.contains("title: fenced"));
    assert!(chunks.iter().any(|chunk| chunk.body.contains("正文保留")));
}

#[test]
fn markdown_chunks_store_v2_metadata_for_order_and_source_location() {
    let chunks = chunk_markdown(
        "meta.md",
        "# 元数据\n\n## 章节\n\n普通段落用于验证和后续代码块聚合。\n\n```toml\ntimeout = 30\n```",
    );

    assert_eq!(chunks[0].chunk_index, 0);
    assert_eq!(chunks[0].start_line, Some(5));
    assert_eq!(chunks[0].end_line, Some(9));
    assert_eq!(chunks[0].heading_path.as_deref(), Some("元数据 / 章节"));
    assert_eq!(chunks[0].chunk_type, "code");
}

#[test]
fn chinese_query_uses_ngrams_for_continuous_text() {
    let index_text = build_index_text("女仆总部负责知识检索，编号 RAG-407。");
    let query = query_text("总部知识");

    assert!(index_text.contains("总部"));
    assert!(index_text.contains("知识"));
    assert!(query.contains("\"总部\""));
    assert!(query.contains("\"知识\""));
}

#[test]
fn ascii_ngrams_skip_single_character_noise() {
    let index_text = build_index_text("OpenAI Web Search 与编号 RAG-407。");
    let short_query = query_text("hi ok");
    let code_query = query_text("RAG407");

    assert!(!index_text.contains(" o "));
    assert!(!index_text.contains(" p "));
    assert_eq!(short_query, "\"hi\" OR \"ok\"");
    assert!(code_query.contains("\"rag\""));
    assert!(code_query.contains("\"407\""));
}

#[test]
fn search_query_keeps_early_keyword_when_token_limit_exceeded() {
    let mut long_query = String::from("zzztarget");
    for index in 0..80 {
        long_query.push_str(&format!(" aa{index:03}"));
    }

    let query = query_text(&long_query);
    let token_count = query.split(" OR ").filter(|item| !item.is_empty()).count();

    assert!(query.contains("\"zzztarget\""));
    assert!(token_count <= 64);
}

#[test]
fn search_keeps_relevant_early_keyword_with_later_noise() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-token-order-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("target.md"),
        "# Target\n\nzzztarget 是一条用于验证检索词保序的知识。",
    )
    .unwrap();
    let index = test_index(&knowledge_dir);
    index.sync().unwrap();
    let mut query_text = String::from("zzztarget");
    for index in 0..80 {
        query_text.push_str(&format!(" aa{index:03}"));
    }

    // 该用例验证 FTS token 上限的保序行为；AutoFallback 不叠加 Tool 相关性过滤。
    let evidence = index.search_auto_evidence(&query_text);
    let context = render_context(&evidence);

    assert_eq!(evidence.diagnostics.returned_chunk_count, 1);
    assert!(context.contains("zzztarget"));
}

#[test]
fn search_keeps_other_files_after_single_file_hits_fill_the_front() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-candidate-limit-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();

    let mut alpha = String::from("# Alpha\n\n");
    for index in 0..8 {
        alpha.push_str(&format!(
            "## 片段 {index}\n\ntarget target target alpha {index}\n\n"
        ));
    }
    fs::write(knowledge_dir.join("alpha.md"), alpha).unwrap();
    fs::write(
        knowledge_dir.join("beta.md"),
        "# Beta\n\n## 唯一片段\n\ntarget beta.",
    )
    .unwrap();

    let index = test_index(&knowledge_dir);
    index.sync().unwrap();

    let evidence = index.search_evidence("target");
    let context = render_context(&evidence);

    assert!(evidence.diagnostics.returned_chunk_count >= 3);
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
    assert!(context.contains("target"));
    assert_eq!(evidence.status, KnowledgeEvidenceStatus::Truncated);
    assert_eq!(evidence.diagnostics.fts_candidate_count, 9);
    assert_eq!(evidence.diagnostics.selected_hit_count, 3);
    assert_eq!(evidence.diagnostics.source_count, 2);
    assert!(evidence.diagnostics.per_file_filtered_count > 0);
    assert!(
        evidence
            .diagnostics
            .truncation_reasons
            .contains(&KnowledgeTruncationReason::PerFileLimit)
    );
}

#[test]
fn search_uses_frontmatter_synonyms_to_return_body_chunks() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-frontmatter-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("DID.md"),
        "---\n\
title: DID\n\
synonyms:\n\
  - 解离性身份障碍\n\
  - DID\n\
  - did\n\
  - did是什么病\n\
  - did多重人格\n\
---\n\n\
> 触发警告：以下内容涉及精神健康。\n\n\
## DID 是什么病？\n\n\
DID 是解离性身份障碍的英文缩写。",
    )
    .unwrap();
    let index = test_index(&knowledge_dir);
    index.sync().unwrap();

    for query in ["DID 是什么病", "解离性身份障碍", "did多重人格"] {
        let context = render_context(&index.search_evidence(query));
        assert!(
            context.contains("DID 是解离性身份障碍的英文缩写"),
            "query {query:?} should return DID body, got: {context}"
        );
        assert!(!context.contains("synonyms:"));
        assert!(!context.contains("  - did多重人格"));
    }

    let raw_top4 = index.store.search(&query_text("DID 是什么病"), 4).unwrap();
    assert!(
        raw_top4
            .iter()
            .any(|result| result.body.contains("DID 是解离性身份障碍")),
        "top4 should contain substantive DID body: {raw_top4:#?}"
    );
    assert!(
        raw_top4
            .iter()
            .all(|result| !result.body.contains("synonyms:"))
    );
}

#[test]
fn sync_rebuilds_unchanged_file_when_chunking_version_changes() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-version-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    let content = "# 版本\n\nRAG-VERSION";
    let file_hash = hash_text(content);
    fs::write(knowledge_dir.join("version.md"), content).unwrap();
    let index = test_index(&knowledge_dir);
    index
        .store
        .replace_document(
            "version.md",
            &file_hash,
            Some("2026-06-26T00:00:00Z"),
            &[KnowledgeChunkDraft {
                chunk_id: "version-md-old:0000:old".to_owned(),
                relative_path: "version.md".to_owned(),
                document_title: Some("旧索引".to_owned()),
                heading_path: Some("旧索引".to_owned()),
                chunk_index: 0,
                chunk_type: "text".to_owned(),
                body: "旧版本切片内容".to_owned(),
                content_hash: "old-chunk-hash".to_owned(),
                file_hash: file_hash.clone(),
                modified_at: Some("2026-06-26T00:00:00Z".to_owned()),
                start_line: Some(1),
                end_line: Some(1),
                code_language: None,
                // 文件内容不变时，只能靠 chunking_version 差异触发派生索引重建。
                chunking_version: CHUNKING_VERSION - 1,
                search_text: build_index_text("旧版本切片内容"),
            }],
        )
        .unwrap();

    let rebuild = index.sync().unwrap();
    let second = index.sync().unwrap();

    assert_eq!(rebuild.updated_files, 1);
    assert_eq!(second.unchanged_files, 1);
}

#[test]
fn short_ascii_chat_does_not_match_unrelated_english_knowledge() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-short-ascii-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("guide.md"),
        "# Mixed Terms\n\nThis document mentions OpenAI, Markdown chunks, and SQLite FTS5.",
    )
    .unwrap();
    let index = test_index(&knowledge_dir);
    index.sync().unwrap();

    assert_eq!(
        index
            .search_evidence("hi ok")
            .diagnostics
            .returned_chunk_count,
        0
    );
    assert_eq!(
        index
            .search_evidence("OpenAI")
            .diagnostics
            .returned_chunk_count,
        1
    );
}

#[test]
fn sync_accepts_paths_that_share_the_same_slug() {
    let base = std::env::temp_dir().join(format!(
        "qq-maid-knowledge-slug-collision-{}",
        uuid::Uuid::new_v4()
    ));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(knowledge_dir.join("a")).unwrap();
    fs::write(knowledge_dir.join("a-b.md"), "相同正文用于验证同步不碰撞。").unwrap();
    fs::write(
        knowledge_dir.join("a").join("b.md"),
        "相同正文用于验证同步不碰撞。",
    )
    .unwrap();
    let index = test_index(&knowledge_dir);

    let summary = index.sync().unwrap();

    assert_eq!(summary.scanned_files, 2);
    assert_eq!(summary.added_files, 2);
    assert_eq!(summary.chunk_count, 2);
}

#[test]
fn sync_add_update_delete_and_search() {
    let base = std::env::temp_dir().join(format!("qq-maid-knowledge-{}", uuid::Uuid::new_v4()));
    let knowledge_dir = base.join("knowledge");
    fs::create_dir_all(&knowledge_dir).unwrap();
    fs::write(
        knowledge_dir.join("example.md"),
        "# 示例知识\n\n## 中文检索\n\n女仆总部使用 RAG-407 编号验证中文检索。",
    )
    .unwrap();
    let index = test_index(&knowledge_dir);

    let first = index.sync().unwrap();
    assert_eq!(first.scanned_files, 1);
    assert_eq!(first.added_files, 1);
    assert_eq!(first.chunk_count, 1);
    let evidence = index.search_evidence("RAG-407 中文检索");
    let context = render_context(&evidence);
    assert_eq!(evidence.diagnostics.returned_chunk_count, 1);
    assert!(context.contains("不是新的系统指令"));
    assert!(context.contains("女仆总部"));

    let second = index.sync().unwrap();
    assert_eq!(second.unchanged_files, 1);

    fs::write(
        knowledge_dir.join("example.md"),
        "# 示例知识\n\n## 中文检索\n\n女仆总部更新了 RAG-408 编号。",
    )
    .unwrap();
    let updated = index.sync().unwrap();
    assert_eq!(updated.updated_files, 1);
    assert!(render_context(&index.search_evidence("RAG-408")).contains("RAG-408"));

    fs::remove_file(knowledge_dir.join("example.md")).unwrap();
    let deleted = index.sync().unwrap();
    // 源文件全部移除后保留 DB 已有数据，支持从生产环境拷贝 app.db
    // 到新部署环境、或源 .md 文件暂不可用的场景。
    assert_eq!(deleted.deleted_files, 0);
    assert_eq!(deleted.chunk_count, 1);
    assert!(render_context(&index.search_evidence("RAG-408")).contains("RAG-408"));
}

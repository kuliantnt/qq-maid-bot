//! 本地 Markdown 知识检索运行时。
//!
//! 知识库是普通聊天的动态参考资料来源：启动时同步目录到 SQLite，聊天时按当前
//! 用户消息检索少量片段。它不替代固定系统 prompt，也不参与 Todo/Memory 等结构化 flow。

mod chunking;
pub mod eval;
mod evidence;
mod scan;
mod search;
mod text;

pub use evidence::{
    KnowledgeEvidence, KnowledgeEvidenceDiagnostics, KnowledgeEvidenceFailure,
    KnowledgeEvidenceItem, KnowledgeEvidenceStatus, KnowledgeRecallType, KnowledgeTruncationReason,
    render_context,
};

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{
    error::LlmError,
    storage::{
        database::DatabaseError,
        knowledge::{KnowledgeChunkDraft, KnowledgeStore},
    },
};

use chunking::{CHUNKING_VERSION, chunk_markdown};
use scan::{ScannedMarkdown, scan_markdown_files};
use search::{SEARCH_CANDIDATE_LIMIT, build_evidence, query_diagnostics, query_text};
use text::hash_text;

/// 知识库同步结果，用于启动日志和测试断言。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnowledgeSyncSummary {
    pub scanned_files: usize,
    pub added_files: usize,
    pub updated_files: usize,
    pub deleted_files: usize,
    pub unchanged_files: usize,
    pub chunk_count: usize,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct KnowledgeIndex {
    store: KnowledgeStore,
    knowledge_dir: PathBuf,
}

impl KnowledgeIndex {
    pub fn new(store: KnowledgeStore, knowledge_dir: impl Into<PathBuf>) -> Self {
        Self {
            store,
            knowledge_dir: knowledge_dir.into(),
        }
    }

    pub fn knowledge_dir(&self) -> &Path {
        &self.knowledge_dir
    }

    /// 启动期同步 Markdown 知识目录。
    ///
    /// 目录不存在或为空是正常降级；数据库/FTS 错误会返回硬错误，避免索引损坏时
    /// 伪装成“无知识命中”。
    pub fn sync(&self) -> Result<KnowledgeSyncSummary, LlmError> {
        let start = Instant::now();
        self.store
            .ensure_fts5_available()
            .map_err(knowledge_db_error)?;
        let files = scan_markdown_files(&self.knowledge_dir).map_err(knowledge_io_error)?;
        let mut summary = KnowledgeSyncSummary {
            scanned_files: files.len(),
            enabled: !files.is_empty(),
            ..KnowledgeSyncSummary::default()
        };
        let scanned_paths = files
            .iter()
            .map(|file| file.relative_path.clone())
            .collect::<HashSet<_>>();

        for file in files {
            match self.sync_file(&file) {
                Ok(FileSyncOutcome::Added) => summary.added_files += 1,
                Ok(FileSyncOutcome::Updated) => summary.updated_files += 1,
                Ok(FileSyncOutcome::Unchanged) => summary.unchanged_files += 1,
                Err(err) => {
                    tracing::warn!(
                        path = %file.relative_path,
                        error = %err,
                        "knowledge markdown file sync failed"
                    );
                    return Err(err);
                }
            }
        }

        // 知识目录没有可扫描的 .md 文件时，保留 DB 中已有索引不删除。
        // 这支持从生产环境拷贝 app.db 到新部署环境、或源 .md 文件暂不可用的场景。
        if summary.scanned_files > 0 {
            for indexed_path in self
                .store
                .list_document_paths()
                .map_err(knowledge_db_error)?
            {
                if !scanned_paths.contains(&indexed_path) {
                    self.store
                        .delete_document(&indexed_path)
                        .map_err(knowledge_db_error)?;
                    summary.deleted_files += 1;
                }
            }
        } else {
            tracing::info!(
                dir = %self.knowledge_dir.display(),
                "knowledge dir has no scannable markdown files, keeping existing db index"
            );
        }
        summary.chunk_count = self.store.chunk_count().map_err(knowledge_db_error)?;
        summary.enabled = summary.chunk_count > 0;
        tracing::info!(
            scanned_files = summary.scanned_files,
            added_files = summary.added_files,
            updated_files = summary.updated_files,
            deleted_files = summary.deleted_files,
            unchanged_files = summary.unchanged_files,
            chunk_count = summary.chunk_count,
            elapsed_ms = start.elapsed().as_millis(),
            enabled = summary.enabled,
            dir = %self.knowledge_dir.display(),
            "knowledge index sync completed"
        );
        Ok(summary)
    }

    /// 返回结构化知识证据。数据库故障会显式标记为 `failed`，不会伪装成无命中。
    pub fn search_evidence(&self, user_text: &str) -> KnowledgeEvidence {
        let started = Instant::now();
        match self.search_evidence_result(user_text, started) {
            Ok(evidence) => evidence,
            Err(err) => {
                let query = query_text(user_text);
                let (query_fingerprint, query_token_count) = query_diagnostics(&query);
                KnowledgeEvidence {
                    status: KnowledgeEvidenceStatus::Failed,
                    items: Vec::new(),
                    diagnostics: KnowledgeEvidenceDiagnostics {
                        query_fingerprint,
                        query_token_count,
                        latency_ms: elapsed_ms(started),
                        ..KnowledgeEvidenceDiagnostics::default()
                    },
                    failure: Some(KnowledgeEvidenceFailure {
                        error_code: err.code,
                    }),
                }
            }
        }
    }

    fn search_evidence_result(
        &self,
        user_text: &str,
        started: Instant,
    ) -> Result<KnowledgeEvidence, LlmError> {
        let query = query_text(user_text);
        let (query_fingerprint, query_token_count) = query_diagnostics(&query);
        let diagnostics = KnowledgeEvidenceDiagnostics {
            query_fingerprint,
            query_token_count,
            ..KnowledgeEvidenceDiagnostics::default()
        };
        if query.is_empty() {
            return Ok(KnowledgeEvidence {
                status: KnowledgeEvidenceStatus::NoHit,
                items: Vec::new(),
                diagnostics: KnowledgeEvidenceDiagnostics {
                    latency_ms: elapsed_ms(started),
                    ..diagnostics
                },
                failure: None,
            });
        }
        let results = self
            .store
            .search(&query, SEARCH_CANDIDATE_LIMIT)
            .map_err(knowledge_db_error)?;
        let mut evidence =
            build_evidence(&self.store, results, diagnostics).map_err(knowledge_db_error)?;
        evidence.diagnostics.latency_ms = elapsed_ms(started);
        tracing::debug!(
            status = ?evidence.status,
            query_fingerprint = %evidence.diagnostics.query_fingerprint,
            query_token_count = evidence.diagnostics.query_token_count,
            fts_candidate_count = evidence.diagnostics.fts_candidate_count,
            selected_hit_count = evidence.diagnostics.selected_hit_count,
            expanded_chunk_count = evidence.diagnostics.expanded_chunk_count,
            returned_chunk_count = evidence.diagnostics.returned_chunk_count,
            source_count = evidence.diagnostics.source_count,
            truncation_reasons = ?evidence.diagnostics.truncation_reasons,
            latency_ms = evidence.diagnostics.latency_ms,
            "knowledge search completed"
        );
        Ok(evidence)
    }

    fn sync_file(&self, file: &ScannedMarkdown) -> Result<FileSyncOutcome, LlmError> {
        let content = match fs::read_to_string(&file.absolute_path) {
            Ok(content) => content,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                return Ok(FileSyncOutcome::Unchanged);
            }
            Err(err) => return Err(knowledge_io_error(err)),
        };
        let file_hash = hash_text(&content);
        let existing = self
            .store
            .document_state(&file.relative_path)
            .map_err(knowledge_db_error)?;
        if existing.as_ref().is_some_and(|state| {
            state.file_hash == file_hash && state.chunking_version == CHUNKING_VERSION
        }) {
            return Ok(FileSyncOutcome::Unchanged);
        }

        let chunks = chunk_markdown(&file.relative_path, &content)
            .into_iter()
            .map(|chunk| KnowledgeChunkDraft {
                chunk_id: chunk.chunk_id,
                relative_path: chunk.relative_path,
                document_title: chunk.document_title,
                heading_path: chunk.heading_path,
                chunk_index: chunk.chunk_index,
                chunk_type: chunk.chunk_type,
                body: chunk.body,
                content_hash: chunk.content_hash,
                file_hash: file_hash.clone(),
                modified_at: file.modified_at.clone(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                code_language: chunk.code_language,
                chunking_version: CHUNKING_VERSION,
                search_text: chunk.search_text,
            })
            .collect::<Vec<_>>();
        self.store
            .replace_document(
                &file.relative_path,
                &file_hash,
                file.modified_at.as_deref(),
                &chunks,
            )
            .map_err(knowledge_db_error)?;
        Ok(if existing.is_some() {
            FileSyncOutcome::Updated
        } else {
            FileSyncOutcome::Added
        })
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileSyncOutcome {
    Added,
    Updated,
    Unchanged,
}

fn knowledge_db_error(err: DatabaseError) -> LlmError {
    LlmError::new(
        "knowledge_db_error",
        format!("knowledge index database error: {}", err.message()),
        "knowledge",
    )
}

fn knowledge_io_error(err: io::Error) -> LlmError {
    LlmError::new(
        "knowledge_io_error",
        format!("knowledge markdown file error: {err}"),
        "knowledge",
    )
}

#[cfg(test)]
mod tests {
    use super::text::build_index_text;
    use super::*;
    use crate::storage::{database::SqliteDatabase, knowledge::KNOWLEDGE_MIGRATIONS};

    fn test_index(base: &Path) -> KnowledgeIndex {
        let db =
            SqliteDatabase::open_temp("qq-maid-knowledge-runtime", KNOWLEDGE_MIGRATIONS).unwrap();
        KnowledgeIndex::new(KnowledgeStore::new(db), base)
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

        let evidence = index.search_evidence(&query_text);
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
        assert!(!no_hit.diagnostics.query_fingerprint.is_empty());

        index
            .store
            .database_for_test()
            .connection()
            .unwrap()
            .execute("DROP TABLE knowledge_chunks_fts", [])
            .unwrap();

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
    fn search_evidence_includes_adjacent_chunks() {
        let base = std::env::temp_dir().join(format!(
            "qq-maid-knowledge-adjacent-{}",
            uuid::Uuid::new_v4()
        ));
        let knowledge_dir = base.join("knowledge");
        fs::create_dir_all(&knowledge_dir).unwrap();
        let mut content =
            String::from("# 相邻补全\n\n## 参数\n\n前置定义：AlphaTimeout 表示主要请求超时。\n\n");
        for index in 0..30 {
            content.push_str(&format!(
                "普通说明 {index}：这些文字用于把配置值推到下一个 chunk。这里不包含查询关键字。\n"
            ));
        }
        content.push_str("\n具体配置值：RAG-ADJACENT-TARGET = 30。\n");
        fs::write(knowledge_dir.join("adjacent.md"), content).unwrap();
        let index = test_index(&knowledge_dir);
        index.sync().unwrap();

        let evidence = index.search_evidence("RAG-ADJACENT-TARGET");
        let context = render_context(&evidence);

        assert!(context.contains("RAG-ADJACENT-TARGET"));
        assert!(context.contains("AlphaTimeout"));
        assert!(context.contains("片段：相邻补充"));
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
}

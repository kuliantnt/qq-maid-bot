//! 本地 Markdown 知识检索运行时。
//!
//! 知识库是普通聊天的动态参考资料来源：启动时同步目录到 SQLite，聊天时按当前
//! 用户消息检索少量片段。它不替代固定系统 prompt，也不参与 Todo/Memory 等结构化 flow。

mod chunking;
mod diagnostics;
mod embedding;
pub mod eval;
mod evidence;
mod scan;
mod search;
mod text;

#[cfg(test)]
mod semantic_tests;

pub use embedding::KnowledgeSemanticConfig;
pub use evidence::{
    KnowledgeEvidence, KnowledgeEvidenceDiagnostics, KnowledgeEvidenceFailure,
    KnowledgeEvidenceItem, KnowledgeEvidenceStatus, KnowledgeInjectionDecision,
    KnowledgeInjectionReason, KnowledgeRecallType, KnowledgeTruncationReason, render_context,
};

use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::Instant,
};

use crate::{error::LlmError, storage::database::DatabaseError};

use super::storage::{KnowledgeChunkDraft, KnowledgeStore};

use chunking::{CHUNKING_VERSION, ChunkingError, chunk_markdown_with_limit};
use diagnostics::summarize_chunks;
use scan::{ScannedMarkdown, scan_markdown_files};
use search::{KnowledgeSearchProfile, build_evidence, query_diagnostics, query_text};
use text::hash_text;

/// 即使输入文件在字节上限内，极端换行/代码块也可能产生过多切片；限制切片数量，
/// 避免 chunk、search_text 和可选 embedding records 随输入结构无限放大。
pub(crate) const MAX_MANAGED_CHUNKS: usize = 16_384;

/// 知识库同步结果，用于启动日志和测试断言。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnowledgeSyncSummary {
    pub scanned_files: usize,
    pub added_files: usize,
    pub updated_files: usize,
    pub deleted_files: usize,
    pub unchanged_files: usize,
    pub chunk_count: usize,
    pub embedded_chunk_count: usize,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct KnowledgeIndex {
    store: KnowledgeStore,
    knowledge_dir: PathBuf,
    semantic: Option<embedding::SemanticRuntime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ManagedIndexResult {
    pub content_hash: String,
    pub chunk_count: usize,
    pub embedding_count: usize,
}

/// 托管文件的索引键不直接暴露原始文件 ID，也不依赖原始文件名或服务器路径。
pub(crate) fn managed_document_key(file_id: &str) -> String {
    let fingerprint = hash_text(file_id);
    format!("managed/{}", &fingerprint[..24])
}

impl KnowledgeIndex {
    pub fn new(store: KnowledgeStore, knowledge_dir: impl Into<PathBuf>) -> Self {
        Self {
            store,
            knowledge_dir: knowledge_dir.into(),
            semantic: None,
        }
    }

    pub fn with_semantic_config(
        mut self,
        config: KnowledgeSemanticConfig,
    ) -> Result<Self, LlmError> {
        self.semantic = embedding::SemanticRuntime::load(config)?;
        Ok(self)
    }

    pub fn knowledge_dir(&self) -> &Path {
        &self.knowledge_dir
    }

    pub(crate) fn database(&self) -> &crate::storage::database::SqliteDatabase {
        self.store.database()
    }

    /// 在数据库写入前完成托管 Markdown 的全部解析、切片和可选 embedding，
    /// 只有所有步骤成功后才通过一个事务替换知识文档、FTS 和向量派生数据。
    pub(crate) fn process_managed_file(
        &self,
        file_id: &str,
        filename: &str,
        bytes: &[u8],
        max_file_bytes: usize,
        modified_at: Option<&str>,
    ) -> Result<ManagedIndexResult, LlmError> {
        if bytes.len() > max_file_bytes {
            return Err(LlmError::new(
                "file_too_large",
                "knowledge file exceeds configured size limit",
                "knowledge",
            ));
        }
        let lower_name = filename.to_ascii_lowercase();
        if !lower_name.ends_with(".md") && !lower_name.ends_with(".markdown") {
            return Err(LlmError::new(
                "unsupported_format",
                "knowledge file format is not supported",
                "knowledge",
            ));
        }
        if lower_name.ends_with(".example.md") || lower_name.ends_with(".example.markdown") {
            return Err(LlmError::new(
                "example_template",
                "knowledge example templates are not indexable",
                "knowledge",
            ));
        }
        let content = std::str::from_utf8(bytes).map_err(|_| {
            LlmError::new(
                "invalid_encoding",
                "knowledge file is not valid UTF-8",
                "knowledge",
            )
        })?;
        if content.trim().is_empty() {
            return Err(LlmError::new(
                "empty_document",
                "knowledge document is empty",
                "knowledge",
            ));
        }

        let document_key = managed_document_key(file_id);
        let file_hash = hash_text(content);
        let chunks = chunk_markdown_with_limit(&document_key, content, MAX_MANAGED_CHUNKS)
            .map_err(map_chunking_error)?;
        if chunks.is_empty() {
            return Err(LlmError::new(
                "empty_document",
                "knowledge document has no indexable content",
                "knowledge",
            ));
        }
        let chunks = chunks
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
                modified_at: modified_at.map(str::to_owned),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                code_language: chunk.code_language,
                chunking_version: CHUNKING_VERSION,
                search_text: chunk.search_text,
            })
            .collect::<Vec<_>>();

        let embedding = self.semantic.as_ref().map(|semantic| {
            semantic
                .embed_chunks(&chunks)
                .map(|records| (semantic.model_id(), semantic.embedding_version(), records))
        });
        let embedding = match embedding {
            Some(result) => Some(result?),
            None => None,
        };
        if let Some((model, version, records)) = embedding.as_ref() {
            self.store
                .replace_document_with_embeddings(
                    &document_key,
                    &file_hash,
                    modified_at,
                    &chunks,
                    (model, *version, records),
                )
                .map_err(knowledge_db_error)?;
        } else {
            self.store
                .replace_managed_document(&document_key, &file_hash, modified_at, &chunks)
                .map_err(knowledge_db_error)?;
        }
        Ok(ManagedIndexResult {
            content_hash: file_hash,
            chunk_count: chunks.len(),
            embedding_count: embedding
                .as_ref()
                .map_or(0, |(_, _, records)| records.len()),
        })
    }

    #[cfg(test)]
    pub(crate) fn break_search_for_test(&self) {
        self.store
            .database_for_test()
            .connection()
            .unwrap()
            .execute("DROP TABLE knowledge_chunks_fts", [])
            .unwrap();
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
                .list_directory_document_paths()
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
        if let Some(semantic) = &self.semantic {
            summary.embedded_chunk_count = semantic.sync_missing(&self.store)?;
        }
        summary.enabled = summary.chunk_count > 0;
        tracing::info!(
            scanned_files = summary.scanned_files,
            added_files = summary.added_files,
            updated_files = summary.updated_files,
            deleted_files = summary.deleted_files,
            unchanged_files = summary.unchanged_files,
            chunk_count = summary.chunk_count,
            embedded_chunk_count = summary.embedded_chunk_count,
            elapsed_ms = start.elapsed().as_millis(),
            enabled = summary.enabled,
            dir = %self.knowledge_dir.display(),
            "knowledge index sync completed"
        );
        Ok(summary)
    }

    /// 返回结构化知识证据。数据库故障会显式标记为 `failed`，不会伪装成无命中。
    pub fn search_evidence(&self, user_text: &str) -> KnowledgeEvidence {
        self.search_evidence_with_profile(&[user_text.to_owned()], KnowledgeSearchProfile::Tool)
    }

    /// 多个补充 query 在检索层统一融合、去重和预算，避免 Tool 输出各自挤占上下文。
    pub fn search_evidence_many(&self, queries: &[String]) -> KnowledgeEvidence {
        self.search_evidence_with_profile(queries, KnowledgeSearchProfile::Tool)
    }

    /// preflight 只返回通过高相关判定的少量主证据，不执行章节扩展。
    pub fn search_preflight_evidence(&self, user_text: &str) -> KnowledgeEvidence {
        self.search_evidence_with_profile(
            &[user_text.to_owned()],
            KnowledgeSearchProfile::Preflight,
        )
    }

    /// `auto` 是紧急回退，保留纯 FTS 与固定邻接的旧自动注入行为。
    pub fn search_auto_evidence(&self, user_text: &str) -> KnowledgeEvidence {
        self.search_evidence_with_profile(
            &[user_text.to_owned()],
            KnowledgeSearchProfile::AutoFallback,
        )
    }

    fn search_evidence_with_profile(
        &self,
        queries: &[String],
        profile: KnowledgeSearchProfile,
    ) -> KnowledgeEvidence {
        let started = Instant::now();
        match self.search_evidence_result(queries, profile, started) {
            Ok(evidence) => evidence,
            Err(err) => {
                let (query_fingerprint, query_token_count) = combined_query_diagnostics(queries);
                KnowledgeEvidence {
                    status: KnowledgeEvidenceStatus::Failed,
                    items: Vec::new(),
                    diagnostics: KnowledgeEvidenceDiagnostics {
                        query_fingerprint,
                        query_token_count,
                        latency_ms: elapsed_ms(started),
                        ..KnowledgeEvidenceDiagnostics::default()
                    },
                    injection: KnowledgeInjectionDecision {
                        allow_injection: false,
                        reason: KnowledgeInjectionReason::SearchFailed,
                        threshold_version: "knowledge-preflight-v1".to_owned(),
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
        queries: &[String],
        profile: KnowledgeSearchProfile,
        started: Instant,
    ) -> Result<KnowledgeEvidence, LlmError> {
        let queries = normalized_queries(queries);
        let (query_fingerprint, query_token_count) = combined_query_diagnostics(&queries);
        let diagnostics = KnowledgeEvidenceDiagnostics {
            query_fingerprint,
            query_token_count,
            ..KnowledgeEvidenceDiagnostics::default()
        };
        if queries.is_empty() {
            return Ok(KnowledgeEvidence {
                status: KnowledgeEvidenceStatus::NoHit,
                items: Vec::new(),
                diagnostics: KnowledgeEvidenceDiagnostics {
                    latency_ms: elapsed_ms(started),
                    ..diagnostics
                },
                injection: KnowledgeInjectionDecision {
                    allow_injection: false,
                    reason: KnowledgeInjectionReason::NoHit,
                    threshold_version: "knowledge-preflight-v1".to_owned(),
                },
                failure: None,
            });
        }
        let mut evidence = build_evidence(
            &self.store,
            self.semantic.as_ref(),
            &queries,
            profile,
            diagnostics,
        )?;
        evidence.diagnostics.latency_ms = elapsed_ms(started);
        tracing::debug!(
            status = ?evidence.status,
            query_fingerprint = %evidence.diagnostics.query_fingerprint,
            query_token_count = evidence.diagnostics.query_token_count,
            fts_candidate_count = evidence.diagnostics.fts_candidate_count,
            semantic_candidate_count = evidence.diagnostics.semantic_candidate_count,
            fused_candidate_count = evidence.diagnostics.fused_candidate_count,
            selected_hit_count = evidence.diagnostics.selected_hit_count,
            expanded_chunk_count = evidence.diagnostics.expanded_chunk_count,
            returned_chunk_count = evidence.diagnostics.returned_chunk_count,
            source_count = evidence.diagnostics.source_count,
            allow_injection = evidence.injection.allow_injection,
            injection_reason = ?evidence.injection.reason,
            threshold_version = %evidence.injection.threshold_version,
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

        let chunks = chunk_markdown_with_limit(&file.relative_path, &content, MAX_MANAGED_CHUNKS)
            .map_err(map_chunking_error)?;
        let diagnostics = summarize_chunks(&content, &chunks);
        tracing::info!(
            path = %file.relative_path,
            file_bytes = diagnostics.file_bytes,
            file_chars = diagnostics.file_chars,
            chunk_count = diagnostics.chunk_count,
            chunk_chars_min = diagnostics.chunk_chars_min,
            chunk_chars_avg = diagnostics.chunk_chars_avg,
            chunk_chars_p50 = diagnostics.chunk_chars_p50,
            chunk_chars_p95 = diagnostics.chunk_chars_p95,
            chunk_chars_max = diagnostics.chunk_chars_max,
            chunks_with_heading = diagnostics.chunks_with_heading,
            chunks_without_heading = diagnostics.chunks_without_heading,
            heading_section_count = diagnostics.heading_section_count,
            heading_chunks_min = diagnostics.heading_chunks_min,
            heading_chunks_avg = diagnostics.heading_chunks_avg,
            heading_chunks_p50 = diagnostics.heading_chunks_p50,
            heading_chunks_p95 = diagnostics.heading_chunks_p95,
            heading_chunks_max = diagnostics.heading_chunks_max,
            "knowledge markdown chunking completed"
        );
        let chunks = chunks
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

fn map_chunking_error(error: ChunkingError) -> LlmError {
    match error {
        ChunkingError::TooManyChunks => LlmError::new(
            "too_many_chunks",
            "knowledge document produces too many chunks",
            "knowledge",
        ),
    }
}

fn normalized_queries(queries: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for query in queries.iter().take(4) {
        let query = query.trim();
        if !query.is_empty() && !normalized.iter().any(|existing| existing == query) {
            normalized.push(query.to_owned());
        }
    }
    normalized
}

fn combined_query_diagnostics(queries: &[String]) -> (String, usize) {
    let fts_queries = queries
        .iter()
        .map(|query| query_text(query))
        .filter(|query| !query.is_empty())
        .collect::<Vec<_>>();
    let combined = fts_queries.join("\n");
    let token_count = fts_queries
        .iter()
        .map(|query| query_diagnostics(query).1)
        .sum();
    (hash_text(&combined).chars().take(12).collect(), token_count)
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
mod tests;

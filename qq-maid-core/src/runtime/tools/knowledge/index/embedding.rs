//! 可选的本地语义向量运行时。
//!
//! 模型只在显式启用时初始化；文档正文与查询均不离开本机。向量通过 storage
//! 独立表持久化，关闭该能力后检索会直接退回纯 BM25。

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use crate::{
    error::LlmError,
    runtime::tools::knowledge::storage::{KnowledgeEmbeddingRecord, KnowledgeStore},
};

pub const SEMANTIC_MODEL_ID: &str = "BAAI/bge-small-zh-v1.5";
pub const SEMANTIC_EMBEDDING_VERSION: i64 = 1;

// 常见部署只有 2 GiB 内存；同步分页与 ONNX 推理都使用保守小批次，避免 FastEmbed
// 为全部缺失片段保留 tokenizer、推理输出和最终向量。
pub(super) const SEMANTIC_SYNC_BATCH_SIZE: usize = 8;
const FASTEMBED_INFERENCE_BATCH_SIZE: usize = 8;
const PROGRESS_LOG_INTERVAL_BATCHES: usize = 32;

/// 本地语义召回配置。默认关闭，避免升级后隐式下载模型或改变启动时延。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeSemanticConfig {
    pub enabled: bool,
    pub cache_dir: PathBuf,
}

impl KnowledgeSemanticConfig {
    pub fn disabled(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            enabled: false,
            cache_dir: cache_dir.into(),
        }
    }

    pub fn local(cache_dir: impl Into<PathBuf>) -> Self {
        Self {
            enabled: true,
            cache_dir: cache_dir.into(),
        }
    }
}

pub(super) trait KnowledgeEmbedder: Send + Sync {
    fn model_id(&self) -> &'static str;
    fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, LlmError>;
    fn embed_query(&self, query: &str) -> Result<Vec<f32>, LlmError>;
}

pub(super) struct LocalKnowledgeEmbedder {
    model: Mutex<TextEmbedding>,
}

impl LocalKnowledgeEmbedder {
    pub(super) fn load(cache_dir: PathBuf) -> Result<Arc<dyn KnowledgeEmbedder>, LlmError> {
        let options = TextInitOptions::new(EmbeddingModel::BGESmallZHV15)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false)
            .with_intra_threads(2);
        let model = TextEmbedding::try_new(options).map_err(|error| {
            LlmError::new(
                "knowledge_embedding_model_error",
                format!("failed to initialize local knowledge embedding model: {error}"),
                "knowledge",
            )
        })?;
        Ok(Arc::new(Self {
            model: Mutex::new(model),
        }))
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
        self.model
            .lock()
            .map_err(|_| {
                LlmError::new(
                    "knowledge_embedding_lock_error",
                    "local knowledge embedding model lock is poisoned",
                    "knowledge",
                )
            })?
            .embed(texts, Some(FASTEMBED_INFERENCE_BATCH_SIZE))
            .map_err(|error| {
                LlmError::new(
                    "knowledge_embedding_error",
                    format!("local knowledge embedding failed: {error}"),
                    "knowledge",
                )
            })
    }
}

impl KnowledgeEmbedder for LocalKnowledgeEmbedder {
    fn model_id(&self) -> &'static str {
        SEMANTIC_MODEL_ID
    }

    fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
        self.embed(texts)
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, LlmError> {
        // BGE 的公开检索指令是模型能力的一部分，不承载业务词或注入 Gate。
        let text = format!("为这个句子生成表示以用于检索相关文章：{query}");
        self.embed(&[text.as_str()])?
            .into_iter()
            .next()
            .ok_or_else(|| {
                LlmError::new(
                    "knowledge_embedding_empty",
                    "local knowledge embedding returned no vector",
                    "knowledge",
                )
            })
    }
}

#[derive(Clone)]
pub(super) struct SemanticRuntime {
    embedder: Arc<dyn KnowledgeEmbedder>,
}

impl std::fmt::Debug for SemanticRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SemanticRuntime")
            .field("model", &self.embedder.model_id())
            .finish()
    }
}

impl SemanticRuntime {
    pub(super) fn load(config: KnowledgeSemanticConfig) -> Result<Option<Self>, LlmError> {
        if !config.enabled {
            return Ok(None);
        }
        Ok(Some(Self {
            embedder: LocalKnowledgeEmbedder::load(config.cache_dir)?,
        }))
    }

    #[cfg(test)]
    pub(super) fn from_embedder(embedder: Arc<dyn KnowledgeEmbedder>) -> Self {
        Self { embedder }
    }

    pub(super) fn sync_missing(&self, store: &KnowledgeStore) -> Result<usize, LlmError> {
        let started = Instant::now();
        let model = self.embedder.model_id();
        let total_missing = store
            .missing_embedding_count(model, SEMANTIC_EMBEDDING_VERSION)
            .map_err(embedding_db_error)?;
        let mut completed = 0;
        let mut batch_number = 0;
        let mut previous_batch = None;

        loop {
            let batch_started = Instant::now();
            let sources = store
                .missing_embedding_sources(
                    model,
                    SEMANTIC_EMBEDDING_VERSION,
                    SEMANTIC_SYNC_BATCH_SIZE,
                )
                .map_err(embedding_db_error)?;
            if sources.is_empty() {
                break;
            }
            let batch_identity = sources
                .iter()
                .map(|source| (source.chunk_id.clone(), source.content_hash.clone()))
                .collect::<Vec<_>>();
            if previous_batch.as_ref() == Some(&batch_identity) {
                return Err(LlmError::new(
                    "knowledge_embedding_no_progress",
                    "knowledge embedding sync returned the same missing chunks after persistence",
                    "knowledge",
                ));
            }
            let batch_size = sources.len();
            let texts = sources
                .iter()
                .map(|source| source.text.as_str())
                .collect::<Vec<_>>();
            let vectors = self.embedder.embed_documents(&texts)?;
            if vectors.len() != batch_size {
                return Err(LlmError::new(
                    "knowledge_embedding_count_mismatch",
                    "local knowledge embedding result count does not match chunks",
                    "knowledge",
                ));
            }
            drop(texts);

            let records = sources
                .into_iter()
                .zip(vectors)
                .map(|(source, vector)| KnowledgeEmbeddingRecord {
                    chunk_id: source.chunk_id,
                    content_hash: source.content_hash,
                    vector,
                })
                .collect::<Vec<_>>();
            store
                .upsert_embeddings(model, SEMANTIC_EMBEDDING_VERSION, &records)
                .map_err(embedding_db_error)?;
            completed += records.len();
            batch_number += 1;
            previous_batch = Some(batch_identity);
            let batch_elapsed_ms = batch_started.elapsed().as_millis();
            tracing::debug!(
                model,
                batch_number,
                input_count = batch_size,
                completed_chunks = completed,
                remaining_chunks = total_missing.saturating_sub(completed),
                elapsed_ms = batch_elapsed_ms,
                "knowledge embedding batch diagnostics"
            );
            // 大知识库可能产生数万批；保留首批、周期进度与末批，避免日志反向放大。
            if batch_number == 1
                || batch_number % PROGRESS_LOG_INTERVAL_BATCHES == 0
                || batch_size < SEMANTIC_SYNC_BATCH_SIZE
                || completed >= total_missing
            {
                tracing::info!(
                    model,
                    batch_number,
                    batch_size,
                    completed_chunks = completed,
                    remaining_chunks = total_missing.saturating_sub(completed),
                    elapsed_ms = batch_elapsed_ms,
                    "knowledge embedding batch completed"
                );
            }
        }

        tracing::info!(
            model,
            completed_chunks = completed,
            total_missing_chunks = total_missing,
            total_batches = batch_number,
            batch_size = SEMANTIC_SYNC_BATCH_SIZE,
            inference_batch_size = FASTEMBED_INFERENCE_BATCH_SIZE,
            elapsed_ms = started.elapsed().as_millis(),
            "knowledge embedding sync completed"
        );
        Ok(completed)
    }

    pub(super) fn model_id(&self) -> &'static str {
        self.embedder.model_id()
    }

    pub(super) fn embedding_version(&self) -> i64 {
        SEMANTIC_EMBEDDING_VERSION
    }

    /// 托管文件先在内存中完成有界批量 embedding，成功后由索引层与 FTS 一起提交。
    pub(super) fn embed_chunks(
        &self,
        chunks: &[crate::runtime::tools::knowledge::storage::KnowledgeChunkDraft],
    ) -> Result<Vec<KnowledgeEmbeddingRecord>, LlmError> {
        let mut records = Vec::with_capacity(chunks.len());
        for batch in chunks.chunks(SEMANTIC_SYNC_BATCH_SIZE) {
            let texts = batch
                .iter()
                .map(|chunk| {
                    format!(
                        "{}\n{}\n{}",
                        chunk.document_title.as_deref().unwrap_or_default(),
                        chunk.heading_path.as_deref().unwrap_or_default(),
                        chunk.body
                    )
                })
                .collect::<Vec<_>>();
            let references = texts.iter().map(String::as_str).collect::<Vec<_>>();
            let vectors = self.embedder.embed_documents(&references)?;
            if vectors.len() != batch.len() {
                return Err(LlmError::new(
                    "knowledge_embedding_count_mismatch",
                    "local knowledge embedding result count does not match chunks",
                    "knowledge",
                ));
            }
            records.extend(batch.iter().zip(vectors).map(|(chunk, vector)| {
                KnowledgeEmbeddingRecord {
                    chunk_id: chunk.chunk_id.clone(),
                    content_hash: chunk.content_hash.clone(),
                    vector,
                }
            }));
        }
        Ok(records)
    }

    pub(super) fn search(
        &self,
        store: &KnowledgeStore,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::runtime::tools::knowledge::storage::KnowledgeSearchResult>, LlmError>
    {
        let vector = self.embedder.embed_query(query)?;
        store
            .semantic_search(
                self.embedder.model_id(),
                SEMANTIC_EMBEDDING_VERSION,
                &vector,
                limit,
            )
            .map_err(embedding_db_error)
    }
}

fn embedding_db_error(error: crate::storage::database::DatabaseError) -> LlmError {
    LlmError::new(
        "knowledge_db_error",
        format!("knowledge embedding database error: {}", error.message()),
        "knowledge",
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::{
        error::LlmError,
        runtime::tools::knowledge::storage::{
            KNOWLEDGE_MIGRATIONS, KnowledgeChunkDraft, KnowledgeStore,
        },
        storage::database::SqliteDatabase,
    };

    use super::{
        KnowledgeEmbedder, SEMANTIC_EMBEDDING_VERSION, SEMANTIC_SYNC_BATCH_SIZE, SemanticRuntime,
    };

    struct RecordingEmbedder {
        calls: Mutex<Vec<Vec<String>>>,
        fail_on_call: Option<usize>,
        mismatch_on_call: Option<usize>,
    }

    impl RecordingEmbedder {
        fn new(fail_on_call: Option<usize>, mismatch_on_call: Option<usize>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_on_call,
                mismatch_on_call,
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl KnowledgeEmbedder for RecordingEmbedder {
        fn model_id(&self) -> &'static str {
            "recording-embedding-v1"
        }

        fn embed_documents(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, LlmError> {
            let call_number = {
                let mut calls = self.calls.lock().unwrap();
                calls.push(texts.iter().map(|text| (*text).to_owned()).collect());
                calls.len()
            };
            if self.fail_on_call == Some(call_number) {
                return Err(LlmError::new(
                    "fixture_embedding_error",
                    "fixture embedding failed",
                    "knowledge",
                ));
            }
            let vector_count = if self.mismatch_on_call == Some(call_number) {
                texts.len().saturating_sub(1)
            } else {
                texts.len()
            };
            Ok((0..vector_count)
                .map(|index| vec![index as f32, 1.0])
                .collect())
        }

        fn embed_query(&self, _query: &str) -> Result<Vec<f32>, LlmError> {
            Ok(vec![0.0, 1.0])
        }
    }

    fn store_with_chunks(chunk_count: usize) -> KnowledgeStore {
        let database =
            SqliteDatabase::open_temp("qq-maid-knowledge-embedding-batch", KNOWLEDGE_MIGRATIONS)
                .unwrap();
        let store = KnowledgeStore::new(database);
        let chunks = (0..chunk_count)
            .map(|index| KnowledgeChunkDraft {
                chunk_id: format!("batch-chunk-{index:05}"),
                relative_path: "large.md".to_owned(),
                document_title: Some("批处理测试".to_owned()),
                heading_path: Some(format!("批处理测试 / 片段 {index}")),
                chunk_index: index,
                chunk_type: "text".to_owned(),
                body: format!("第 {index} 个知识片段用于验证有界 embedding 同步。"),
                content_hash: format!("content-hash-{index:05}"),
                file_hash: "large-file-hash".to_owned(),
                modified_at: None,
                start_line: Some(index + 1),
                end_line: Some(index + 1),
                code_language: None,
                chunking_version: 4,
                search_text: format!("批处理 测试 片段 {index}"),
            })
            .collect::<Vec<_>>();
        store
            .replace_document("large.md", "large-file-hash", None, &chunks)
            .unwrap();
        store
    }

    fn missing_count(store: &KnowledgeStore) -> usize {
        store
            .missing_embedding_count("recording-embedding-v1", SEMANTIC_EMBEDDING_VERSION)
            .unwrap()
    }

    #[test]
    fn sync_missing_reads_and_persists_bounded_batches() {
        let chunk_count = SEMANTIC_SYNC_BATCH_SIZE * 5 + 5;
        let store = store_with_chunks(chunk_count);
        let embedder = Arc::new(RecordingEmbedder::new(None, None));
        let runtime = SemanticRuntime::from_embedder(embedder.clone());

        assert_eq!(runtime.sync_missing(&store).unwrap(), chunk_count);
        assert_eq!(missing_count(&store), 0);
        let calls = embedder.calls();
        assert_eq!(calls.len(), 6);
        assert!(
            calls
                .iter()
                .all(|batch| !batch.is_empty() && batch.len() <= SEMANTIC_SYNC_BATCH_SIZE)
        );
        assert_eq!(calls.iter().map(Vec::len).sum::<usize>(), chunk_count);
    }

    #[test]
    fn completed_batches_survive_failure_and_resume_without_reembedding() {
        let chunk_count = SEMANTIC_SYNC_BATCH_SIZE * 2 + 3;
        let store = store_with_chunks(chunk_count);
        let failing_embedder = Arc::new(RecordingEmbedder::new(Some(2), None));
        let runtime = SemanticRuntime::from_embedder(failing_embedder.clone());

        let error = runtime.sync_missing(&store).unwrap_err();
        assert_eq!(error.code, "fixture_embedding_error");
        assert_eq!(
            missing_count(&store),
            chunk_count - SEMANTIC_SYNC_BATCH_SIZE
        );

        let first_batch = failing_embedder.calls().remove(0);
        let resumed_embedder = Arc::new(RecordingEmbedder::new(None, None));
        let resumed_runtime = SemanticRuntime::from_embedder(resumed_embedder.clone());
        assert_eq!(
            resumed_runtime.sync_missing(&store).unwrap(),
            chunk_count - SEMANTIC_SYNC_BATCH_SIZE
        );
        assert_eq!(missing_count(&store), 0);
        let resumed_texts = resumed_embedder
            .calls()
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        assert!(
            first_batch
                .iter()
                .all(|completed_text| !resumed_texts.contains(completed_text))
        );
    }

    #[test]
    fn sync_missing_skips_embedder_for_empty_set() {
        let store = store_with_chunks(0);
        let embedder = Arc::new(RecordingEmbedder::new(None, None));
        let runtime = SemanticRuntime::from_embedder(embedder.clone());

        assert_eq!(runtime.sync_missing(&store).unwrap(), 0);
        assert!(embedder.calls().is_empty());
    }

    #[test]
    fn vector_count_mismatch_does_not_persist_partial_batch() {
        let store = store_with_chunks(SEMANTIC_SYNC_BATCH_SIZE + 1);
        let embedder = Arc::new(RecordingEmbedder::new(None, Some(1)));
        let runtime = SemanticRuntime::from_embedder(embedder);

        let error = runtime.sync_missing(&store).unwrap_err();
        assert_eq!(error.code, "knowledge_embedding_count_mismatch");
        assert_eq!(missing_count(&store), SEMANTIC_SYNC_BATCH_SIZE + 1);
    }

    #[test]
    fn thousands_of_chunks_never_reach_embedder_as_one_collection() {
        let chunk_count = 2_003;
        let store = store_with_chunks(chunk_count);
        let embedder = Arc::new(RecordingEmbedder::new(None, None));
        let runtime = SemanticRuntime::from_embedder(embedder.clone());

        assert_eq!(runtime.sync_missing(&store).unwrap(), chunk_count);
        let calls = embedder.calls();
        assert_eq!(calls.iter().map(Vec::len).sum::<usize>(), chunk_count);
        assert!(
            calls
                .iter()
                .all(|batch| batch.len() <= SEMANTIC_SYNC_BATCH_SIZE)
        );
        assert!(calls.len() > 1);
    }
}

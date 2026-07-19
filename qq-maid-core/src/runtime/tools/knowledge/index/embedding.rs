//! 可选的本地语义向量运行时。
//!
//! 模型只在显式启用时初始化；文档正文与查询均不离开本机。向量通过 storage
//! 独立表持久化，关闭该能力后检索会直接退回纯 BM25。

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use crate::{
    error::LlmError,
    runtime::tools::knowledge::storage::{KnowledgeEmbeddingRecord, KnowledgeStore},
};

pub const SEMANTIC_MODEL_ID: &str = "BAAI/bge-small-zh-v1.5";
pub const SEMANTIC_EMBEDDING_VERSION: i64 = 1;

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
    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError>;
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

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        self.model
            .lock()
            .map_err(|_| {
                LlmError::new(
                    "knowledge_embedding_lock_error",
                    "local knowledge embedding model lock is poisoned",
                    "knowledge",
                )
            })?
            .embed(texts, None)
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

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        self.embed(texts)
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, LlmError> {
        // BGE 的公开检索指令是模型能力的一部分，不承载业务词或注入 Gate。
        let text = format!("为这个句子生成表示以用于检索相关文章：{query}");
        self.embed(&[text])?.into_iter().next().ok_or_else(|| {
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
        let sources = store
            .missing_embedding_sources(self.embedder.model_id(), SEMANTIC_EMBEDDING_VERSION)
            .map_err(embedding_db_error)?;
        if sources.is_empty() {
            return Ok(0);
        }
        let texts = sources
            .iter()
            .map(|source| source.text.clone())
            .collect::<Vec<_>>();
        let vectors = self.embedder.embed_documents(&texts)?;
        if vectors.len() != sources.len() {
            return Err(LlmError::new(
                "knowledge_embedding_count_mismatch",
                "local knowledge embedding result count does not match chunks",
                "knowledge",
            ));
        }
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
            .upsert_embeddings(
                self.embedder.model_id(),
                SEMANTIC_EMBEDDING_VERSION,
                &records,
            )
            .map_err(embedding_db_error)?;
        Ok(records.len())
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

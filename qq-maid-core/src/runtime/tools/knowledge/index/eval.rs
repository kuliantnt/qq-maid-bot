//! Knowledge FTS5 可复跑评测。
//!
//! 评测集使用合成 Markdown，不读取生产知识目录，也不输出查询正文或证据正文。

use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::storage::database::SqliteDatabase;

use super::{KnowledgeEvidenceStatus, KnowledgeIndex, KnowledgeSemanticConfig};
use crate::runtime::tools::knowledge::storage::{KNOWLEDGE_MIGRATIONS, KnowledgeStore};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct KnowledgeEvalDataset {
    pub version: u32,
    pub documents: Vec<KnowledgeEvalDocument>,
    pub cases: Vec<KnowledgeEvalCase>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct KnowledgeEvalDocument {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct KnowledgeEvalCase {
    pub id: String,
    pub category: String,
    pub query: String,
    #[serde(default)]
    pub additional_queries: Vec<String>,
    #[serde(default)]
    pub expected_sources: Vec<String>,
    #[serde(default)]
    pub expect_no_hit: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KnowledgeEvalReport {
    pub dataset_version: u32,
    pub engine: String,
    pub case_count: usize,
    pub metrics: KnowledgeEvalMetrics,
    pub cases: Vec<KnowledgeEvalCaseResult>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KnowledgeEvalMetrics {
    pub source_recall_at_k: f64,
    pub source_hit_rate: f64,
    pub no_evidence_accuracy: f64,
    pub low_relevance_injection_rate: f64,
    pub preflight_injection_accuracy: f64,
    pub preflight_false_positive_rate: f64,
    pub preflight_false_negative_rate: f64,
    pub truncated_rate: f64,
    pub duplicate_rate: f64,
    pub latency_ms_p50: u64,
    pub latency_ms_p95: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KnowledgeEvalCaseResult {
    pub id: String,
    pub category: String,
    pub status: KnowledgeEvidenceStatus,
    pub expected_sources: Vec<String>,
    pub retrieved_sources: Vec<String>,
    pub source_recall: f64,
    pub no_evidence_correct: Option<bool>,
    pub preflight_allowed: bool,
    pub preflight_expected_allowed: bool,
    pub preflight_correct: bool,
    pub evidence_item_count: usize,
    pub duplicate_count: usize,
    pub top_lexical_coverage: Option<f64>,
    pub top_semantic_similarity: Option<f64>,
    pub injection_reason: String,
    pub preflight_sources: Vec<String>,
    pub latency_ms: u64,
}

impl KnowledgeEvalReport {
    /// 只对确定性正确性指标设门槛；机器负载相关的延迟只记录，不参与退出码。
    pub fn passes_correctness_gate(&self) -> bool {
        self.metrics.source_recall_at_k >= 0.6
            && self.metrics.source_hit_rate >= 0.6
            && self.metrics.no_evidence_accuracy >= 1.0
            && self.metrics.low_relevance_injection_rate <= 0.0
            && self.metrics.preflight_false_positive_rate <= 0.0
    }
}

pub fn parse_dataset(json: &str) -> Result<KnowledgeEvalDataset, String> {
    let dataset = serde_json::from_str::<KnowledgeEvalDataset>(json)
        .map_err(|error| format!("invalid knowledge eval dataset: {error}"))?;
    validate_dataset(&dataset)?;
    Ok(dataset)
}

/// 在隔离的临时索引中运行评测，避免接触或修改真实 `APP_DB_FILE` 和知识目录。
pub fn run_fts5_baseline(dataset: &KnowledgeEvalDataset) -> Result<KnowledgeEvalReport, String> {
    run_evaluation(dataset, None, "fts5_bm25")
}

pub fn run_knowledge_v3(
    dataset: &KnowledgeEvalDataset,
    embedding_cache_dir: PathBuf,
) -> Result<KnowledgeEvalReport, String> {
    run_evaluation(
        dataset,
        Some(KnowledgeSemanticConfig::local(embedding_cache_dir)),
        "fts5_bm25+local_embedding+rrf",
    )
}

fn run_evaluation(
    dataset: &KnowledgeEvalDataset,
    semantic: Option<KnowledgeSemanticConfig>,
    engine: &str,
) -> Result<KnowledgeEvalReport, String> {
    validate_dataset(dataset)?;
    let workspace = EvalWorkspace::create()?;
    for document in &dataset.documents {
        let path = workspace.knowledge_dir.join(&document.path);
        let parent = path
            .parent()
            .ok_or_else(|| format!("invalid eval document path: {}", document.path))?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        fs::write(path, &document.content).map_err(|error| error.to_string())?;
    }
    let database = SqliteDatabase::open(
        workspace.root.join("knowledge-eval.db"),
        KNOWLEDGE_MIGRATIONS,
    )
    .map_err(|error| error.to_string())?;
    let mut index = KnowledgeIndex::new(KnowledgeStore::new(database), &workspace.knowledge_dir);
    if let Some(config) = semantic {
        index = index
            .with_semantic_config(config)
            .map_err(|error| error.to_string())?;
    }
    index.sync().map_err(|error| error.to_string())?;

    let mut results = Vec::with_capacity(dataset.cases.len());
    for case in &dataset.cases {
        let queries = std::iter::once(case.query.clone())
            .chain(case.additional_queries.iter().cloned())
            .collect::<Vec<_>>();
        let evidence = index.search_evidence_many(&queries);
        let evidence_item_count = evidence.items.len();
        // 重复率只统计同一 chunk_id 重复返回；同章节的不同补充 chunk 是合法证据。
        let duplicate_count =
            duplicate_chunk_id_count(evidence.items.iter().map(|item| item.chunk_id.as_str()));
        let mut retrieved_sources = evidence
            .items
            .iter()
            .map(|item| item.relative_path.clone())
            .collect::<Vec<_>>();
        retrieved_sources.sort();
        retrieved_sources.dedup();
        let matched = case
            .expected_sources
            .iter()
            .filter(|source| retrieved_sources.contains(source))
            .count();
        let source_recall = if case.expected_sources.is_empty() {
            1.0
        } else {
            matched as f64 / case.expected_sources.len() as f64
        };
        let preflight = index.search_preflight_evidence(&case.query);
        let preflight_expected_allowed = !case.expect_no_hit && !case.expected_sources.is_empty();
        let preflight_allowed = preflight.injection.allow_injection;
        let mut preflight_sources = preflight
            .items
            .iter()
            .map(|item| item.relative_path.clone())
            .collect::<Vec<_>>();
        preflight_sources.sort();
        preflight_sources.dedup();
        results.push(KnowledgeEvalCaseResult {
            id: case.id.clone(),
            category: case.category.clone(),
            status: evidence.status,
            expected_sources: case.expected_sources.clone(),
            retrieved_sources,
            source_recall,
            no_evidence_correct: case.expect_no_hit.then_some(
                evidence.items.is_empty()
                    && matches!(
                        evidence.status,
                        KnowledgeEvidenceStatus::NoHit | KnowledgeEvidenceStatus::LowRelevance
                    ),
            ),
            preflight_allowed,
            preflight_expected_allowed,
            preflight_correct: preflight_allowed == preflight_expected_allowed,
            evidence_item_count,
            duplicate_count,
            top_lexical_coverage: preflight.diagnostics.top_lexical_coverage,
            top_semantic_similarity: preflight.diagnostics.top_semantic_similarity,
            injection_reason: preflight.injection.reason.as_str().to_owned(),
            preflight_sources,
            latency_ms: evidence.diagnostics.latency_ms,
        });
    }
    Ok(build_report(dataset.version, engine, results))
}

fn build_report(
    version: u32,
    engine: &str,
    cases: Vec<KnowledgeEvalCaseResult>,
) -> KnowledgeEvalReport {
    let evidence_cases = cases
        .iter()
        .filter(|case| !case.expected_sources.is_empty())
        .collect::<Vec<_>>();
    let no_hit_cases = cases
        .iter()
        .filter(|case| case.no_evidence_correct.is_some())
        .collect::<Vec<_>>();
    let source_recall_at_k = average(evidence_cases.iter().map(|case| case.source_recall));
    let source_hit_rate = average(
        evidence_cases
            .iter()
            .map(|case| f64::from(case.source_recall > 0.0)),
    );
    let no_evidence_accuracy = average(
        no_hit_cases
            .iter()
            .map(|case| f64::from(case.no_evidence_correct == Some(true))),
    );
    let low_relevance_injection_rate = average(
        no_hit_cases
            .iter()
            .map(|case| f64::from(case.preflight_allowed)),
    );
    let truncated_rate = average(
        cases
            .iter()
            .map(|case| f64::from(case.status == KnowledgeEvidenceStatus::Truncated)),
    );
    let preflight_cases = cases.iter().collect::<Vec<_>>();
    let preflight_injection_accuracy = average(
        preflight_cases
            .iter()
            .map(|case| f64::from(case.preflight_correct)),
    );
    let preflight_false_positive_rate = average(
        preflight_cases
            .iter()
            .filter(|case| !case.preflight_expected_allowed)
            .map(|case| f64::from(case.preflight_allowed)),
    );
    let preflight_false_negative_rate = average(
        preflight_cases
            .iter()
            .filter(|case| case.preflight_expected_allowed)
            .map(|case| f64::from(!case.preflight_allowed)),
    );
    let evidence_item_count = cases
        .iter()
        .map(|case| case.evidence_item_count)
        .sum::<usize>();
    let duplicate_count = cases.iter().map(|case| case.duplicate_count).sum::<usize>();
    let duplicate_rate = if evidence_item_count == 0 {
        0.0
    } else {
        duplicate_count as f64 / evidence_item_count as f64
    };
    let mut latencies = cases.iter().map(|case| case.latency_ms).collect::<Vec<_>>();
    latencies.sort_unstable();
    KnowledgeEvalReport {
        dataset_version: version,
        engine: engine.to_owned(),
        case_count: cases.len(),
        metrics: KnowledgeEvalMetrics {
            source_recall_at_k,
            source_hit_rate,
            no_evidence_accuracy,
            low_relevance_injection_rate,
            preflight_injection_accuracy,
            preflight_false_positive_rate,
            preflight_false_negative_rate,
            truncated_rate,
            duplicate_rate,
            latency_ms_p50: percentile(&latencies, 50),
            latency_ms_p95: percentile(&latencies, 95),
        },
        cases,
    }
}

fn validate_dataset(dataset: &KnowledgeEvalDataset) -> Result<(), String> {
    if dataset.version == 0 || dataset.documents.is_empty() || dataset.cases.is_empty() {
        return Err("knowledge eval dataset requires a version, documents, and cases".to_owned());
    }
    let mut paths = HashSet::new();
    for document in &dataset.documents {
        let path = PathBuf::from(&document.path);
        if path.is_absolute()
            || document.path.trim().is_empty()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::RootDir
                )
            })
            || !document.path.ends_with(".md")
            || !paths.insert(document.path.as_str())
        {
            return Err(format!(
                "unsafe or duplicate eval document path: {}",
                document.path
            ));
        }
    }
    let mut ids = HashSet::new();
    for case in &dataset.cases {
        if case.id.trim().is_empty()
            || case.query.trim().is_empty()
            || !ids.insert(case.id.as_str())
            || (case.expect_no_hit && !case.expected_sources.is_empty())
            || case.additional_queries.len() > 3
            || case
                .additional_queries
                .iter()
                .any(|query| query.trim().is_empty())
            || case
                .expected_sources
                .iter()
                .any(|source| !paths.contains(source.as_str()))
        {
            return Err(format!("invalid knowledge eval case: {}", case.id));
        }
    }
    Ok(())
}

fn duplicate_chunk_id_count<'a>(chunk_ids: impl IntoIterator<Item = &'a str>) -> usize {
    let mut seen = HashSet::new();
    chunk_ids
        .into_iter()
        .filter(|chunk_id| !seen.insert(*chunk_id))
        .count()
}

fn average(values: impl Iterator<Item = f64>) -> f64 {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        1.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn percentile(values: &[u64], percentile: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let index = (values.len() * percentile).div_ceil(100).saturating_sub(1);
    values[index.min(values.len() - 1)]
}

struct EvalWorkspace {
    root: PathBuf,
    knowledge_dir: PathBuf,
}

impl EvalWorkspace {
    fn create() -> Result<Self, String> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let root = std::env::temp_dir().join(format!("qq-maid-knowledge-eval-{nonce}"));
        let knowledge_dir = root.join("knowledge");
        fs::create_dir_all(&knowledge_dir).map_err(|error| error.to_string())?;
        Ok(Self {
            root,
            knowledge_dir,
        })
    }
}

impl Drop for EvalWorkspace {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            tracing::warn!(error = %error, "knowledge eval workspace cleanup failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DATASET: &str = include_str!("../fixtures/knowledge_eval_v1.json");

    #[test]
    fn fts5_baseline_dataset_is_valid_and_reproducible() {
        let dataset = parse_dataset(DATASET).unwrap();
        let first = run_fts5_baseline(&dataset).unwrap();
        let second = run_fts5_baseline(&dataset).unwrap();

        assert_eq!(first.dataset_version, 1);
        assert_eq!(first.case_count, 7);
        assert!(first.metrics.source_recall_at_k >= 0.6);
        assert_eq!(first.metrics.no_evidence_accuracy, 1.0);
        assert_eq!(first.metrics.low_relevance_injection_rate, 0.0);
        assert!(first.passes_correctness_gate());
        assert_eq!(
            first.metrics.source_recall_at_k,
            second.metrics.source_recall_at_k
        );
        assert_eq!(
            first.metrics.source_hit_rate,
            second.metrics.source_hit_rate
        );
        assert_eq!(
            first.metrics.no_evidence_accuracy,
            second.metrics.no_evidence_accuracy
        );
        assert_eq!(
            first.metrics.low_relevance_injection_rate,
            second.metrics.low_relevance_injection_rate
        );
        let multi_query = first
            .cases
            .iter()
            .find(|case| case.id == "multi_query_duplicate_recall")
            .unwrap();
        assert_eq!(multi_query.source_recall, 1.0);
        assert_eq!(multi_query.evidence_item_count, 2);
        assert_eq!(multi_query.duplicate_count, 0);
        assert_eq!(first.metrics.duplicate_rate, 0.0);
        assert_eq!(
            first
                .cases
                .iter()
                .map(stable_case_result)
                .collect::<Vec<_>>(),
            second
                .cases
                .iter()
                .map(stable_case_result)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn dataset_rejects_parent_directory_paths() {
        let mut dataset = parse_dataset(DATASET).unwrap();
        dataset.documents[0].path = "../secret.md".to_owned();

        assert!(run_fts5_baseline(&dataset).is_err());
    }

    #[test]
    fn duplicate_count_uses_complete_chunk_ids() {
        assert_eq!(
            duplicate_chunk_id_count(["section:chunk-1", "section:chunk-2", "section:chunk-1"]),
            1
        );
        assert_eq!(
            duplicate_chunk_id_count(["section:chunk-1", "section:chunk-2"]),
            0
        );
    }

    fn stable_case_result(
        case: &KnowledgeEvalCaseResult,
    ) -> (&str, KnowledgeEvidenceStatus, &[String], f64, Option<bool>) {
        (
            &case.id,
            case.status,
            &case.retrieved_sources,
            case.source_recall,
            case.no_evidence_correct,
        )
    }
}

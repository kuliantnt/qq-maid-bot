//! Memory 确定性后台整理任务。
//!
//! 当前阶段不调用模型，也不读取聊天正文：只在完整作用域内归档语义键与正文完全
//! 相同的重复长期记忆。更开放的会话候选沉淀需要独立的保留期、opt-out 和写前授权。

use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::time::{MissedTickBehavior, interval_at};
use tracing::{debug, info, warn};

use super::storage::{ConsolidationLimits, ConsolidationRunStats, MemoryStore};

#[derive(Debug, Clone, Copy)]
pub struct MemoryConsolidationConfig {
    pub enabled: bool,
    pub check_interval_seconds: u64,
    pub min_interval_seconds: u64,
    pub min_new_records: usize,
    pub min_distinct_sources: usize,
    pub max_records: usize,
    pub max_input_chars: usize,
}

#[derive(Clone)]
pub struct MemoryConsolidationWorker {
    store: MemoryStore,
    config: MemoryConsolidationConfig,
}

impl MemoryConsolidationWorker {
    pub fn new(store: MemoryStore, config: MemoryConsolidationConfig) -> Self {
        Self { store, config }
    }

    pub fn spawn(self) {
        if !self.config.enabled {
            info!("memory consolidation disabled");
            return;
        }
        tokio::spawn(async move {
            info!(
                check_interval_seconds = self.config.check_interval_seconds,
                min_interval_seconds = self.config.min_interval_seconds,
                min_new_records = self.config.min_new_records,
                min_distinct_sources = self.config.min_distinct_sources,
                max_records = self.config.max_records,
                max_input_chars = self.config.max_input_chars,
                mode = "exact_duplicate",
                "memory consolidation worker enabled"
            );
            self.run_loop().await;
        });
    }

    async fn run_loop(self) {
        let mut ticker = interval_at(
            tokio::time::Instant::now() + Duration::from_secs(30),
            Duration::from_secs(self.config.check_interval_seconds.max(60)),
        );
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(error) = self.run_once().await {
                warn!(
                    error_code = "memory_consolidation_failed",
                    stage = "storage_transaction",
                    error = %error,
                    "memory consolidation cycle failed"
                );
            }
        }
    }

    pub async fn run_once(&self) -> Result<MemoryConsolidationRunStats, String> {
        if !self.config.enabled {
            return Ok(MemoryConsolidationRunStats::default());
        }
        let started = Instant::now();
        let store = self.store.clone();
        let limits = ConsolidationLimits {
            min_interval_seconds: self.config.min_interval_seconds,
            min_new_records: self.config.min_new_records,
            min_distinct_sources: self.config.min_distinct_sources,
            max_records: self.config.max_records,
            max_input_chars: self.config.max_input_chars,
        };
        let now_epoch = Utc::now().timestamp();
        let stats = tokio::task::spawn_blocking(move || store.consolidate_due(limits, now_epoch))
            .await
            .map_err(|error| format!("memory consolidation task join failed: {error}"))?
            .map_err(|error| format!("{}: {}", error.code(), error.message()))?;
        let stats = MemoryConsolidationRunStats::from(stats);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if stats.candidate_target_count == 0 {
            debug!(
                skip_reason = "gates_not_met",
                elapsed_ms, "memory consolidation skipped"
            );
        } else {
            info!(
                candidate_target_count = stats.candidate_target_count,
                processed_target_count = stats.processed_target_count,
                input_record_count = stats.input_record_count,
                output_record_count = stats.output_record_count,
                duplicate_count = stats.archived_duplicate_count,
                conflict_count = stats.conflict_count,
                truncated_target_count = stats.truncated_target_count,
                provider = "local",
                model = "deterministic_exact_duplicate",
                elapsed_ms,
                status = "success",
                "memory consolidation cycle completed"
            );
        }
        Ok(stats)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryConsolidationRunStats {
    pub candidate_target_count: usize,
    pub processed_target_count: usize,
    pub input_record_count: usize,
    pub output_record_count: usize,
    pub archived_duplicate_count: usize,
    pub conflict_count: usize,
    pub truncated_target_count: usize,
}

impl From<ConsolidationRunStats> for MemoryConsolidationRunStats {
    fn from(stats: ConsolidationRunStats) -> Self {
        Self {
            candidate_target_count: stats.candidate_target_count,
            processed_target_count: stats.processed_target_count,
            input_record_count: stats.input_record_count,
            output_record_count: stats.output_record_count,
            archived_duplicate_count: stats.archived_duplicate_count,
            conflict_count: stats.conflict_count,
            truncated_target_count: stats.truncated_target_count,
        }
    }
}

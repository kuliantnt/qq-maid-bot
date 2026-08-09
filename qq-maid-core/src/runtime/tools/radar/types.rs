use std::sync::Arc;

use async_trait::async_trait;

use crate::error::LlmError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarTarget {
    All,
    Codex,
    Claude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarIssueTarget {
    Codex,
    Claude,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadarSourceKind {
    Codex,
    Claude,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadarSourceFailure {
    pub source: RadarSourceKind,
    pub code: String,
    pub stage: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RadarSnapshot {
    pub codex: Option<CodexRadarSummary>,
    pub claude: Option<ClaudeRadarSummary>,
    pub failures: Vec<RadarSourceFailure>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexRadarSummary {
    pub status: Option<String>,
    pub updated_at: Option<String>,
    pub action: Option<String>,
    pub window_message: Option<String>,
    pub prediction_level: Option<String>,
    pub probability_24h: Option<f64>,
    pub probability_48h: Option<f64>,
    pub prediction_summary: Option<String>,
    pub model_score: Option<f64>,
    pub model_status: Option<String>,
    pub model_passed: Option<u64>,
    pub model_tasks: Option<u64>,
    pub model_label: Option<String>,
    pub iq_models: Vec<CodexModelMetric>,
    pub quota_5h_20x: Option<f64>,
    pub quota_7d_20x: Option<f64>,
    pub quota_updated_at: Option<String>,
    pub quota_policy_5h: Option<String>,
    pub quota_rows: Vec<CodexQuotaMetric>,
    pub rating_updated_at: Option<String>,
    pub rating_models: Vec<CodexRatingMetric>,
    pub source_url: String,
    pub feedback_url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexQuotaMetric {
    pub tier: String,
    pub basis: Option<String>,
    pub five_h: Option<f64>,
    pub seven_d: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexRatingMetric {
    pub label: String,
    pub average: Option<f64>,
    pub count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexModelMetric {
    pub label: String,
    pub score: Option<f64>,
    pub status: Option<String>,
    pub passed: Option<u64>,
    pub tasks: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeRadarSummary {
    pub status: Option<String>,
    pub updated_at: Option<String>,
    pub quota_updated_at: Option<String>,
    pub quota_5h: Option<f64>,
    pub quota_7d: Option<f64>,
    pub usage_5h: Option<String>,
    pub usage_7d: Option<String>,
    pub top_iq_model: Option<ClaudeModelMetric>,
    pub top_rating_model: Option<ClaudeModelMetric>,
    pub source_url: String,
    pub feedback_url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeModelMetric {
    pub name: String,
    pub score: Option<f64>,
    pub passed: Option<u64>,
    pub valid: Option<u64>,
    pub invalid: Option<u64>,
    pub updated_at: Option<String>,
}

#[async_trait]
pub trait RadarExecutor: Send + Sync {
    async fn radar(&self, target: RadarTarget) -> Result<RadarSnapshot, LlmError>;

    fn provider_name(&self) -> &'static str {
        "radar-public"
    }
}

pub type DynRadarExecutor = Arc<dyn RadarExecutor>;

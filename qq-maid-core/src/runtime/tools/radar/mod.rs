//! Codex / Claude Code Radar 公开数据读取。
//!
//! 这里虽然不注册为模型 Tool，但仍放在 `tools` 目录：slash 入口只做解析和展示，
//! 外部看板接入的 HTTP、字段兼容和错误映射集中在本模块，后续同类雷达可沿用。

mod client;
pub(crate) mod flow;
mod format;
mod parse;
mod types;

pub use client::{build_radar_executor, radar_feedback_url, radar_site_url};
pub use types::{
    ClaudeModelMetric, ClaudeRadarSummary, CodexModelMetric, CodexQuotaMetric, CodexRadarSummary,
    CodexRatingMetric, DynRadarExecutor, RadarExecutor, RadarIssueTarget, RadarSnapshot,
    RadarSourceFailure, RadarSourceKind, RadarTarget,
};

#[cfg(test)]
mod tests;

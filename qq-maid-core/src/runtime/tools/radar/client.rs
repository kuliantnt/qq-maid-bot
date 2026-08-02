use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::Value;

use crate::error::LlmError;

use super::{
    ClaudeRadarSummary, CodexRadarSummary, DynRadarExecutor, RadarExecutor, RadarIssueTarget,
    RadarSnapshot, RadarSourceFailure, RadarSourceKind, RadarTarget,
    parse::{apply_codex_ratings, parse_claude_summary, parse_codex_summary},
};

const CODEX_CURRENT_URL: &str = "https://codexradar.com/current.json";
const CODEX_RATINGS_URL: &str = "https://codexradar.com/api/model-ratings";
pub(super) const CODEX_SITE_URL: &str = "https://codexradar.com/";
pub(super) const CODEX_FEEDBACK_URL: &str = "https://codexradar.com/";
const CLAUDE_DATA_URL: &str = "https://claudecoderadar.com/data/claude-code-radar.json";
const CLAUDE_RATINGS_URL: &str = "https://claudecoderadar.com/api/model-ratings";
pub(super) const CLAUDE_SITE_URL: &str = "https://claudecoderadar.com/";
pub(super) const CLAUDE_FEEDBACK_URL: &str = "https://claudecoderadar.com/";
const RADAR_USER_AGENT: &str = concat!("qq-maid-core/", env!("CARGO_PKG_VERSION"));

pub fn build_radar_executor() -> Result<DynRadarExecutor, LlmError> {
    Ok(Arc::new(HttpRadarExecutor::new()?))
}

pub struct HttpRadarExecutor {
    client: reqwest::Client,
}

impl HttpRadarExecutor {
    pub fn new() -> Result<Self, LlmError> {
        let client = qq_maid_common::http_client::try_builder()
            .map_err(|err| LlmError::provider(err.to_string(), "radar_tls"))?
            .user_agent(RADAR_USER_AGENT)
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|err| LlmError::provider(err.to_string(), "radar_client"))?;
        Ok(Self { client })
    }

    async fn fetch_json(&self, url: &str, stage: &'static str) -> Result<Value, LlmError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|err| map_radar_request_error(err, stage))?;
        if !response.status().is_success() {
            return Err(LlmError::http(format!(
                "radar upstream returned {} at {stage}",
                response.status()
            )));
        }
        response
            .json::<Value>()
            .await
            .map_err(|err| LlmError::provider(err.to_string(), stage))
    }

    async fn fetch_codex(&self) -> Result<(CodexRadarSummary, Vec<RadarSourceFailure>), LlmError> {
        let json = self.fetch_json(CODEX_CURRENT_URL, "radar_codex").await?;
        let mut summary = parse_codex_summary(&json);
        let mut failures = Vec::new();
        match self
            .fetch_json(CODEX_RATINGS_URL, "radar_codex_ratings")
            .await
        {
            Ok(ratings) => apply_codex_ratings(&mut summary, &ratings),
            Err(err) => failures.push(failure(RadarSourceKind::Codex, &err)),
        }
        Ok((summary, failures))
    }

    async fn fetch_claude(
        &self,
    ) -> Result<(ClaudeRadarSummary, Vec<RadarSourceFailure>), LlmError> {
        let data = self
            .fetch_json(CLAUDE_DATA_URL, "radar_claude_data")
            .await?;
        let mut failures = Vec::new();
        let ratings = match self
            .fetch_json(CLAUDE_RATINGS_URL, "radar_claude_ratings")
            .await
        {
            Ok(ratings) => ratings,
            Err(err) => {
                failures.push(failure(RadarSourceKind::Claude, &err));
                Value::Null
            }
        };
        Ok((parse_claude_summary(&data, &ratings), failures))
    }
}

#[async_trait]
impl RadarExecutor for HttpRadarExecutor {
    async fn radar(&self, target: RadarTarget) -> Result<RadarSnapshot, LlmError> {
        let mut snapshot = RadarSnapshot {
            codex: None,
            claude: None,
            failures: Vec::new(),
        };

        if matches!(target, RadarTarget::All | RadarTarget::Codex) {
            match self.fetch_codex().await {
                Ok((summary, failures)) => {
                    snapshot.codex = Some(summary);
                    snapshot.failures.extend(failures);
                }
                Err(err) => snapshot
                    .failures
                    .push(failure(RadarSourceKind::Codex, &err)),
            }
        }
        if matches!(target, RadarTarget::All | RadarTarget::Claude) {
            match self.fetch_claude().await {
                Ok((summary, failures)) => {
                    snapshot.claude = Some(summary);
                    snapshot.failures.extend(failures);
                }
                Err(err) => snapshot
                    .failures
                    .push(failure(RadarSourceKind::Claude, &err)),
            }
        }

        if snapshot.codex.is_none() && snapshot.claude.is_none() {
            let first = snapshot
                .failures
                .first()
                .cloned()
                .unwrap_or(RadarSourceFailure {
                    source: match target {
                        RadarTarget::Claude => RadarSourceKind::Claude,
                        RadarTarget::All | RadarTarget::Codex => RadarSourceKind::Codex,
                    },
                    code: "radar_empty".to_owned(),
                    stage: "radar".to_owned(),
                });
            return Err(LlmError::new(
                first.code,
                "radar data unavailable",
                first.stage,
            ));
        }

        Ok(snapshot)
    }
}

pub fn radar_feedback_url(target: RadarIssueTarget) -> &'static str {
    match target {
        RadarIssueTarget::Codex => CODEX_FEEDBACK_URL,
        RadarIssueTarget::Claude => CLAUDE_FEEDBACK_URL,
    }
}

pub fn radar_site_url(target: RadarIssueTarget) -> &'static str {
    match target {
        RadarIssueTarget::Codex => CODEX_SITE_URL,
        RadarIssueTarget::Claude => CLAUDE_SITE_URL,
    }
}

fn map_radar_request_error(err: reqwest::Error, stage: &'static str) -> LlmError {
    if err.is_timeout() {
        return LlmError::timeout(stage);
    }
    LlmError::http(err.to_string())
}

fn failure(source: RadarSourceKind, err: &LlmError) -> RadarSourceFailure {
    RadarSourceFailure {
        source,
        code: err.code.clone(),
        stage: err.stage.clone(),
    }
}

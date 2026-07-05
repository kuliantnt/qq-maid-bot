//! Codex / Claude Code Radar 公开数据读取。
//!
//! 这里虽然不注册为模型 Tool，但仍放在 `tools` 目录：slash 入口只做解析和展示，
//! 外部看板接入的 HTTP、字段兼容和错误映射集中在本模块，后续同类雷达可沿用。

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::Value;

use crate::error::LlmError;

const CODEX_CURRENT_URL: &str = "https://codexradar.com/current.json";
const CODEX_SITE_URL: &str = "https://codexradar.com/";
const CODEX_FEEDBACK_URL: &str = "https://codexradar.com/";
const CLAUDE_DATA_URL: &str = "https://claudecoderadar.com/data/claude-code-radar.json";
const CLAUDE_RATINGS_URL: &str = "https://claudecoderadar.com/api/model-ratings";
const CLAUDE_SITE_URL: &str = "https://claudecoderadar.com/";
const CLAUDE_FEEDBACK_URL: &str = "https://claudecoderadar.com/";
const RADAR_USER_AGENT: &str = concat!("qq-maid-core/", env!("CARGO_PKG_VERSION"));

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
    pub model_score: Option<f64>,
    pub model_status: Option<String>,
    pub model_passed: Option<u64>,
    pub model_tasks: Option<u64>,
    pub model_label: Option<String>,
    pub iq_models: Vec<CodexModelMetric>,
    pub quota_5h_20x: Option<f64>,
    pub quota_7d_20x: Option<f64>,
    pub source_url: String,
    pub feedback_url: String,
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

pub fn build_radar_executor() -> Result<DynRadarExecutor, LlmError> {
    Ok(Arc::new(HttpRadarExecutor::new()?))
}

pub struct HttpRadarExecutor {
    client: reqwest::Client,
}

impl HttpRadarExecutor {
    pub fn new() -> Result<Self, LlmError> {
        let client = reqwest::Client::builder()
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

    async fn fetch_codex(&self) -> Result<CodexRadarSummary, LlmError> {
        let json = self.fetch_json(CODEX_CURRENT_URL, "radar_codex").await?;
        Ok(parse_codex_summary(&json))
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
                Ok(summary) => snapshot.codex = Some(summary),
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

fn parse_codex_summary(json: &Value) -> CodexRadarSummary {
    let window = json.get("window").unwrap_or(&Value::Null);
    let prediction = json.get("prediction").unwrap_or(&Value::Null);
    let model_iq = json.get("model_iq").unwrap_or(&Value::Null);
    let model_latest = json
        .pointer("/model_iq/latest")
        .or_else(|| json.pointer("/model_iq/comparisons/gpt_55_high/latest"))
        .unwrap_or(&Value::Null);
    let quota = json
        .pointer("/model_iq/quota_radar")
        .unwrap_or(&Value::Null);
    let first_row = quota
        .get("rows")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .unwrap_or(&Value::Null);

    CodexRadarSummary {
        status: str_value(window.get("status")).or_else(|| str_value(json.get("status"))),
        updated_at: str_value(json.get("monitored_at"))
            .or_else(|| str_value(prediction.get("updated_at")))
            .or_else(|| str_value(quota.get("updated_at"))),
        action: str_value(window.get("action"))
            .or_else(|| str_value(json.get("recommended_action"))),
        window_message: str_value(window.get("message")),
        prediction_level: str_value(prediction.get("level")),
        probability_24h: f64_value(prediction.get("probability_24h")),
        model_score: f64_value(model_latest.get("score")),
        model_status: str_value(model_latest.get("status")),
        model_passed: u64_value(model_latest.get("passed")),
        model_tasks: u64_value(model_latest.get("tasks"))
            .or_else(|| u64_value(model_latest.get("valid_tasks"))),
        model_label: codex_model_label(None, model_latest),
        iq_models: codex_iq_models(model_iq),
        quota_5h_20x: f64_value(first_row.get("five_h")),
        quota_7d_20x: f64_value(first_row.get("seven_d")),
        source_url: str_value(json.pointer("/links/html"))
            .unwrap_or_else(|| CODEX_SITE_URL.to_owned()),
        feedback_url: CODEX_FEEDBACK_URL.to_owned(),
    }
}

fn codex_iq_models(model_iq: &Value) -> Vec<CodexModelMetric> {
    let mut models = Vec::new();
    if let Some(metric) = codex_model_metric(
        codex_model_label(None, model_iq.get("latest").unwrap_or(&Value::Null)),
        model_iq.get("latest").unwrap_or(&Value::Null),
    ) {
        models.push(metric);
    }
    if let Some(comparisons) = model_iq.get("comparisons").and_then(Value::as_object) {
        for comparison in comparisons.values() {
            let latest = comparison.get("latest").unwrap_or(&Value::Null);
            if let Some(metric) =
                codex_model_metric(codex_model_label(comparison.get("label"), latest), latest)
            {
                models.push(metric);
            }
        }
    }
    models
}

fn codex_model_metric(label: Option<String>, latest: &Value) -> Option<CodexModelMetric> {
    Some(CodexModelMetric {
        label: label?,
        score: f64_value(latest.get("score")),
        status: str_value(latest.get("status")),
        passed: u64_value(latest.get("passed")),
        tasks: u64_value(latest.get("tasks")).or_else(|| u64_value(latest.get("valid_tasks"))),
    })
}

fn codex_model_label(config_label: Option<&Value>, latest: &Value) -> Option<String> {
    str_value(config_label).or_else(|| {
        let model = codex_display_model_name(&str_value(latest.get("model"))?);
        let effort = str_value(latest.get("reasoning_effort"));
        Some(match effort {
            Some(effort) => format!("{model} {effort}"),
            None => model,
        })
    })
}

fn codex_display_model_name(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("gpt-"))
    {
        format!("GPT-{}", &trimmed[4..])
    } else {
        trimmed.to_owned()
    }
}

fn parse_claude_summary(data: &Value, ratings: &Value) -> ClaudeRadarSummary {
    let quota = data.get("quota").unwrap_or(&Value::Null);
    let models = data
        .pointer("/iq/models")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let top_iq_model = models
        .iter()
        .filter_map(claude_iq_model_metric)
        .max_by(|left, right| compare_optional_score(left.score, right.score));
    let top_rating_model = ratings
        .get("models")
        .and_then(Value::as_array)
        .and_then(|models| {
            models
                .iter()
                .filter_map(claude_rating_model_metric)
                .max_by(|left, right| compare_optional_score(left.score, right.score))
        });

    ClaudeRadarSummary {
        status: data
            .get("ok")
            .and_then(Value::as_bool)
            .map(|ok| if ok { "ok" } else { "error" }.to_owned()),
        updated_at: str_value(data.get("updated_at"))
            .or_else(|| str_value(ratings.get("updated_at"))),
        quota_updated_at: str_value(quota.get("updated_at")),
        quota_5h: f64_value(quota.get("base_h5")),
        quota_7d: f64_value(quota.get("base_d7")),
        usage_5h: usage_line(quota, "h5"),
        usage_7d: usage_line(quota, "d7"),
        top_iq_model,
        top_rating_model,
        source_url: CLAUDE_SITE_URL.to_owned(),
        feedback_url: CLAUDE_FEEDBACK_URL.to_owned(),
    }
}

fn claude_iq_model_metric(value: &Value) -> Option<ClaudeModelMetric> {
    Some(ClaudeModelMetric {
        name: str_value(value.get("name"))?,
        score: f64_value(value.get("score")),
        passed: latest_array_u64(value.get("pass")).or_else(|| u64_value(value.get("passed"))),
        valid: latest_array_u64(value.get("valid")).or_else(|| u64_value(value.get("valid"))),
        invalid: latest_array_u64(value.get("invalid")).or_else(|| u64_value(value.get("invalid"))),
        updated_at: str_value(value.get("latest_at")),
    })
}

fn claude_rating_model_metric(value: &Value) -> Option<ClaudeModelMetric> {
    Some(ClaudeModelMetric {
        name: str_value(value.get("label"))?,
        score: f64_value(value.get("average")),
        passed: None,
        valid: u64_value(value.get("count")),
        invalid: None,
        updated_at: None,
    })
}

fn usage_line(quota: &Value, key: &str) -> Option<String> {
    let usage = quota.get("usage")?.as_array()?;
    let item = usage
        .iter()
        .find(|item| item.get("key").and_then(Value::as_str) == Some(key))?;
    let label = str_value(item.get("label_zh"))?;
    let used = u64_value(item.get("used_pct"))?;
    let reset = str_value(item.get("reset_text_zh"));
    Some(match reset {
        Some(reset) => format!("{label} 已用 {used}% · {reset}"),
        None => format!("{label} 已用 {used}%"),
    })
}

fn latest_array_u64(value: Option<&Value>) -> Option<u64> {
    value?.as_array()?.iter().rev().find_map(|value| {
        if value.is_null() {
            None
        } else {
            u64_value(Some(value))
        }
    })
}

fn compare_optional_score(left: Option<f64>, right: Option<f64>) -> std::cmp::Ordering {
    left.unwrap_or(f64::NEG_INFINITY)
        .partial_cmp(&right.unwrap_or(f64::NEG_INFINITY))
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn str_value(value: Option<&Value>) -> Option<String> {
    value?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn f64_value(value: Option<&Value>) -> Option<f64> {
    value?.as_f64()
}

fn u64_value(value: Option<&Value>) -> Option<u64> {
    value?.as_u64()
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parse_codex_summary_maps_public_current_json_fields() {
        let summary = parse_codex_summary(&json!({
            "status": "community_confirmed",
            "monitored_at": "2026-06-30T18:39:12+08:00",
            "recommended_action": "reset_completed",
            "window": {
                "status": "community_confirmed",
                "action": "reset_completed",
                "message": "社区反馈已完成重置"
            },
            "prediction": {"level": "high", "probability_24h": 0.36},
            "links": {"html": "https://codexradar.com/"},
            "model_iq": {
                "latest": {"score": 60.0, "status": "red", "passed": 4, "tasks": 10, "model": "gpt-5.5", "reasoning_effort": "xhigh"},
                "comparisons": {
                    "gpt_55_high": {
                        "label": "GPT-5.5 high",
                        "latest": {"score": 75.0, "status": "red", "passed": 5, "tasks": 10}
                    },
                    "gpt_54_xhigh": {
                        "label": "GPT-5.4 xhigh",
                        "latest": {"score": 90.0, "status": "yellow", "passed": 6, "tasks": 10}
                    }
                },
                "quota_radar": {"rows": [{"five_h": 281.91, "seven_d": 1691.46}]}
            }
        }));

        assert_eq!(summary.status.as_deref(), Some("community_confirmed"));
        assert_eq!(summary.action.as_deref(), Some("reset_completed"));
        assert_eq!(summary.model_score, Some(60.0));
        assert_eq!(summary.model_passed, Some(4));
        assert_eq!(summary.model_label.as_deref(), Some("GPT-5.5 xhigh"));
        assert_eq!(summary.iq_models.len(), 3);
        let best = summary
            .iq_models
            .iter()
            .find(|model| model.label == "GPT-5.4 xhigh")
            .unwrap();
        assert_eq!(best.score, Some(90.0));
        assert_eq!(summary.quota_5h_20x, Some(281.91));
    }

    #[test]
    fn parse_claude_summary_uses_data_and_model_ratings() {
        let summary = parse_claude_summary(
            &json!({
                "ok": true,
                "updated_at": "2026-07-05T09:37:50+08:00",
                "quota": {
                    "updated_at": "2026-07-04T09:46:15+08:00",
                    "base_h5": 332.29,
                    "base_d7": 2270.63,
                    "usage": [{"key": "h5", "label_zh": "当前 5h 共享池", "used_pct": 41, "reset_text_zh": "13:00 重置"}]
                },
                "iq": {"models": [
                    {"name": "Opus", "score": 60.0, "pass": [null, 4], "valid": [null, 10], "latest_at": "2026-07-04T09:46:15+08:00"},
                    {"name": "Sonnet", "score": 120.0, "pass": [null, 8], "valid": [null, 10], "latest_at": "2026-07-01T13:10:00+08:00"}
                ]}
            }),
            &json!({
                "models": [
                    {"label": "Opus 4.8 max", "average": 6.5, "count": 8},
                    {"label": "Fable 5 xhigh", "average": 9.1, "count": 9}
                ]
            }),
        );

        assert_eq!(summary.status.as_deref(), Some("ok"));
        assert_eq!(summary.quota_5h, Some(332.29));
        assert_eq!(summary.top_iq_model.unwrap().name, "Sonnet");
        assert_eq!(summary.top_rating_model.unwrap().name, "Fable 5 xhigh");
        assert!(summary.usage_5h.unwrap().contains("已用 41%"));
    }
}

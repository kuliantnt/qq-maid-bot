use serde_json::Value;

use super::{
    ClaudeModelMetric, ClaudeRadarSummary, CodexModelMetric, CodexQuotaMetric, CodexRadarSummary,
    CodexRatingMetric,
    client::{CLAUDE_FEEDBACK_URL, CLAUDE_SITE_URL, CODEX_FEEDBACK_URL, CODEX_SITE_URL},
};

pub(super) fn parse_codex_summary(json: &Value) -> CodexRadarSummary {
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
        // 首页的模型雷达会独立于重置窗口刷新；优先展示模型数据时间，避免窗口监控
        // 长时间无事件时仍向用户显示旧的顶层 monitored_at。
        updated_at: str_value(model_iq.get("updated_at"))
            .or_else(|| str_value(json.get("monitored_at")))
            .or_else(|| str_value(prediction.get("updated_at")))
            .or_else(|| str_value(quota.get("updated_at"))),
        action: str_value(window.get("action"))
            .or_else(|| str_value(json.get("recommended_action"))),
        window_message: str_value(window.get("message")),
        prediction_level: str_value(prediction.get("level")),
        probability_24h: f64_value(prediction.get("probability_24h")),
        probability_48h: f64_value(prediction.get("probability_48h")),
        prediction_summary: str_value(prediction.get("summary")),
        model_score: f64_value(model_latest.get("score")),
        model_status: str_value(model_latest.get("status")),
        model_passed: u64_value(model_latest.get("passed")),
        model_tasks: u64_value(model_latest.get("tasks"))
            .or_else(|| u64_value(model_latest.get("valid_tasks"))),
        model_label: codex_model_label(None, model_latest),
        iq_models: codex_iq_models(model_iq),
        quota_5h_20x: f64_value(first_row.get("five_h")),
        quota_7d_20x: f64_value(first_row.get("seven_d")),
        quota_updated_at: str_value(quota.get("updated_at")),
        quota_policy_5h: str_value(quota.get("five_hour_policy")),
        quota_rows: codex_quota_rows(quota),
        rating_updated_at: None,
        rating_models: Vec::new(),
        source_url: str_value(json.pointer("/links/html"))
            .unwrap_or_else(|| CODEX_SITE_URL.to_owned()),
        feedback_url: CODEX_FEEDBACK_URL.to_owned(),
    }
}

fn codex_quota_rows(quota: &Value) -> Vec<CodexQuotaMetric> {
    quota
        .get("rows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some(CodexQuotaMetric {
                tier: str_value(row.get("tier"))?,
                basis: str_value(row.get("basis")),
                five_h: f64_value(row.get("five_h")),
                seven_d: f64_value(row.get("seven_d")),
            })
        })
        .collect()
}

pub(super) fn apply_codex_ratings(summary: &mut CodexRadarSummary, ratings: &Value) {
    summary.rating_updated_at = str_value(ratings.get("updated_at"));
    summary.rating_models = ratings
        .get("models")
        .and_then(Value::as_array)
        .map(|models| models.iter().filter_map(codex_rating_metric).collect())
        .unwrap_or_default();
}

fn codex_rating_metric(value: &Value) -> Option<CodexRatingMetric> {
    Some(CodexRatingMetric {
        label: str_value(value.get("label"))?,
        average: f64_value(value.get("average")),
        count: u64_value(value.get("count")),
    })
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

pub(super) fn codex_display_model_name(model: &str) -> String {
    let trimmed = model.trim();
    let Some(suffix) = trimmed.get(4..).filter(|_| {
        trimmed
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("gpt-"))
    }) else {
        return trimmed.to_owned();
    };
    let mut parts = suffix.split('-');
    let Some(version) = parts.next() else {
        return format!("GPT-{suffix}");
    };
    let family = parts
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    if family.is_empty() {
        format!("GPT-{version}")
    } else {
        format!("GPT-{version} {}", family.join(" "))
    }
}

pub(super) fn parse_claude_summary(data: &Value, ratings: &Value) -> ClaudeRadarSummary {
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

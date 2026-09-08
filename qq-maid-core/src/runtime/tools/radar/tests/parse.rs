use serde_json::json;

use super::super::parse::*;

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
        "prediction": {
            "level": "high",
            "probability_24h": 0.36,
            "probability_48h": 0.52,
            "summary": "短线仍需关注额度异常反馈。"
        },
        "links": {"html": "https://codexradar.com/"},
        "model_iq": {
            "updated_at": "2026-08-02T19:35:13+08:00",
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
            "quota_radar": {
                "updated_at": "2026-07-30T08:20:35+00:00",
                "five_hour_policy": "temporarily_paused_hidden",
                "rows": [
                    {"tier": "20x Pro", "basis": "distributed radar", "five_h": null, "seven_d": 1693.25},
                    {"tier": "Plus", "basis": "estimated", "five_h": null, "seven_d": 84.66}
                ]
            }
        }
    }));

    assert_eq!(summary.status.as_deref(), Some("community_confirmed"));
    assert_eq!(
        summary.updated_at.as_deref(),
        Some("2026-08-02T19:35:13+08:00")
    );
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
    assert_eq!(summary.probability_48h, Some(0.52));
    assert_eq!(
        summary.prediction_summary.as_deref(),
        Some("短线仍需关注额度异常反馈。")
    );
    assert_eq!(summary.quota_5h_20x, None);
    assert_eq!(summary.quota_7d_20x, Some(1693.25));
    assert_eq!(summary.quota_rows.len(), 2);
    assert_eq!(summary.quota_rows[1].tier, "Plus");
    assert_eq!(
        summary.quota_policy_5h.as_deref(),
        Some("temporarily_paused_hidden")
    );
}

#[test]
fn apply_codex_ratings_selects_highest_24h_score() {
    let mut summary = parse_codex_summary(&json!({}));
    apply_codex_ratings(
        &mut summary,
        &json!({
            "updated_at": "2026-08-02T12:42:22Z",
            "models": [
                {"label": "GPT-5.6 Sol max", "average": 8.2, "count": 24},
                {"label": "GPT-5.6 Sol ultra", "average": 9.0, "count": 27}
            ]
        }),
    );

    assert_eq!(
        summary.rating_updated_at.as_deref(),
        Some("2026-08-02T12:42:22Z")
    );
    let top = summary
        .rating_models
        .iter()
        .max_by(|left, right| {
            left.average
                .partial_cmp(&right.average)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();
    assert_eq!(top.label, "GPT-5.6 Sol ultra");
    assert_eq!(top.average, Some(9.0));
    assert_eq!(top.count, Some(27));
    assert_eq!(summary.rating_models.len(), 2);
}

#[test]
fn apply_codex_metrics_replaces_legacy_iq_with_astra_and_filters_station_models() {
    let mut summary = parse_codex_summary(&json!({
        "model_iq": {
            "updated_at": "2026-08-02T19:35:13+08:00",
            "latest": {"score": 60.0, "status": "green", "passed": 4, "tasks": 10, "model": "gpt-5.6-sol", "reasoning_effort": "max"},
            "comparisons": {}
        }
    }));
    assert_eq!(summary.model_label.as_deref(), Some("GPT-5.6 Sol max"));

    apply_codex_metrics(
        &mut summary,
        &json!({
            "source_updated_at": "2026-09-08T12:32:03+00:00",
            "points": [
                {"model": "gpt-6-astra", "effort": "high", "passed": 82, "total": 114, "iq": 107.89},
                {"model": "gpt-6-astra", "effort": "max", "passed": 76, "total": 106, "iq": 107.55},
                {"model": "gpt-5.6-sol", "effort": "max", "passed": 239, "total": 336, "iq": 106.7},
                {"model": "deepseek-v4-flash", "effort": "off", "passed": 50, "total": 64, "iq": 88.5},
                {"model": "claude-sonnet-5", "effort": "high", "passed": 2, "total": 2, "iq": 150.0}
            ]
        }),
    );

    assert_eq!(
        summary.updated_at.as_deref(),
        Some("2026-09-08T12:32:03+00:00")
    );
    assert_eq!(summary.model_label.as_deref(), Some("GPT-6 Astra high"));
    assert_eq!(summary.model_score, Some(107.89));
    assert_eq!(summary.model_passed, Some(82));
    assert_eq!(summary.model_tasks, Some(114));
    assert_eq!(summary.model_status, None);
    assert_eq!(summary.iq_models.len(), 4);
    assert!(
        summary
            .iq_models
            .iter()
            .any(|model| model.label == "GPT-6 Astra high")
    );
    assert!(
        summary
            .iq_models
            .iter()
            .any(|model| model.label == "DeepSeek V4 Flash off")
    );
    assert!(
        !summary
            .iq_models
            .iter()
            .any(|model| model.label.contains("claude-sonnet-5"))
    );
}

#[test]
fn apply_codex_metrics_without_station_models_keeps_legacy_fallback() {
    let mut summary = parse_codex_summary(&json!({
        "model_iq": {
            "updated_at": "2026-08-02T19:35:13+08:00",
            "latest": {"score": 60.0, "status": "green", "passed": 4, "tasks": 10, "model": "gpt-5.5", "reasoning_effort": "xhigh"},
            "comparisons": {}
        }
    }));
    apply_codex_metrics(
        &mut summary,
        &json!({
            "source_updated_at": "2026-09-08T12:32:03+00:00",
            "points": [
                {"model": "claude-sonnet-5", "effort": "high", "passed": 2, "total": 2, "iq": 150.0}
            ]
        }),
    );

    assert_eq!(
        summary.updated_at.as_deref(),
        Some("2026-08-02T19:35:13+08:00")
    );
    assert_eq!(summary.model_label.as_deref(), Some("GPT-5.5 xhigh"));
    assert_eq!(summary.model_status.as_deref(), Some("green"));
    assert_eq!(summary.iq_models.len(), 1);
}

#[test]
fn codex_display_model_name_formats_model_family() {
    assert_eq!(codex_display_model_name("gpt-5.6-sol"), "GPT-5.6 Sol");
    assert_eq!(codex_display_model_name("gpt-5.5"), "GPT-5.5");
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

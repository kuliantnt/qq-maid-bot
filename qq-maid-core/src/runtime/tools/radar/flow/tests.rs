use crate::runtime::tools::{
    ClaudeRadarSummary, CodexModelMetric, CodexQuotaMetric, CodexRadarSummary, CodexRatingMetric,
    RadarSnapshot, RadarSourceFailure, RadarSourceKind,
};

use super::*;

#[test]
fn parse_radar_action_accepts_required_variants() {
    assert_eq!(parse_radar_action(""), RadarCommand::Show(RadarTarget::All));
    assert_eq!(
        parse_radar_action("codex"),
        RadarCommand::Show(RadarTarget::Codex)
    );
    assert_eq!(
        parse_radar_action("claude"),
        RadarCommand::Show(RadarTarget::Claude)
    );
    assert_eq!(
        parse_radar_action("issue codex"),
        RadarCommand::Issue(RadarIssueTarget::Codex)
    );
    assert_eq!(
        parse_radar_action("issue claude"),
        RadarCommand::Issue(RadarIssueTarget::Claude)
    );
    assert_eq!(parse_radar_action("unknown"), RadarCommand::Usage);
}

#[test]
fn format_radar_reply_hides_missing_fields_and_surfaces_partial_failure() {
    let body = format_radar_reply(
        &RadarSnapshot {
            codex: Some(CodexRadarSummary {
                status: None,
                updated_at: None,
                action: None,
                window_message: None,
                prediction_level: None,
                probability_24h: None,
                probability_48h: None,
                prediction_summary: None,
                model_score: None,
                model_status: None,
                model_passed: None,
                model_tasks: None,
                model_label: None,
                iq_models: Vec::new(),
                quota_5h_20x: None,
                quota_7d_20x: None,
                quota_updated_at: None,
                quota_policy_5h: None,
                quota_rows: Vec::new(),
                rating_updated_at: None,
                rating_models: Vec::new(),
                source_url: "https://codexradar.com/".to_owned(),
                feedback_url: "https://codexradar.com/".to_owned(),
            }),
            claude: None,
            failures: vec![RadarSourceFailure {
                source: RadarSourceKind::Claude,
                code: "timeout".to_owned(),
                stage: "radar_claude_data".to_owned(),
            }],
        },
        RadarTarget::All,
    );

    assert!(body.text.contains("AI 雷达速览"));
    assert!(body.text.contains("Codex Radar 当前只有部分公开数据可读。"));
    assert!(!body.text.contains("额度：未返回"));
    assert!(!body.text.contains("IQ：未返回"));
    assert!(!body.text.contains("状态未返回"));
    assert!(body.text.contains("Claude Code Radar：读取超时"));
    assert!(body.markdown.unwrap().contains("## 读取提示"));
}

#[test]
fn format_codex_detail_adds_single_hidden_field_hint() {
    let body = format_radar_reply(
        &RadarSnapshot {
            codex: Some(CodexRadarSummary {
                status: Some("community_confirmed".to_owned()),
                updated_at: Some("2026-06-30T18:39:12+08:00".to_owned()),
                action: Some("reset_completed".to_owned()),
                window_message: Some("社区反馈已完成重置".to_owned()),
                prediction_level: Some("high".to_owned()),
                probability_24h: Some(0.36),
                probability_48h: None,
                prediction_summary: None,
                model_score: None,
                model_status: None,
                model_passed: None,
                model_tasks: None,
                model_label: None,
                iq_models: Vec::new(),
                quota_5h_20x: None,
                quota_7d_20x: None,
                quota_updated_at: None,
                quota_policy_5h: None,
                quota_rows: Vec::new(),
                rating_updated_at: None,
                rating_models: Vec::new(),
                source_url: "https://codexradar.com/".to_owned(),
                feedback_url: "https://codexradar.com/".to_owned(),
            }),
            claude: None,
            failures: Vec::new(),
        },
        RadarTarget::Codex,
    );

    assert!(body.text.contains("Codex：社区确认 · 重置已完成"));
    assert!(body.text.contains("短线概率：偏高 · 24h 36%"));
    assert!(
        body.text
            .contains("部分指标当前公开接口未返回，已隐藏空字段。")
    );
    assert!(!body.text.contains("community_confirmed"));
    assert!(!body.text.contains("reset_completed"));
    assert!(!body.text.contains("额度：未返回"));
    assert!(!body.text.contains("IQ：未返回"));
}

#[test]
fn format_codex_detail_shows_ranked_iq_quota_and_cross_vendor_ratings() {
    let body = format_radar_reply(
        &RadarSnapshot {
            codex: Some(CodexRadarSummary {
                status: Some("community_confirmed".to_owned()),
                updated_at: Some("2026-06-30T18:39:12+08:00".to_owned()),
                action: Some("reset_completed".to_owned()),
                window_message: None,
                prediction_level: None,
                probability_24h: None,
                probability_48h: None,
                prediction_summary: None,
                model_score: Some(60.0),
                model_status: Some("red".to_owned()),
                model_passed: Some(4),
                model_tasks: Some(10),
                model_label: Some("GPT-5.5 xhigh".to_owned()),
                iq_models: vec![
                    CodexModelMetric {
                        label: "GPT-5.5 xhigh".to_owned(),
                        score: Some(60.0),
                        status: Some("red".to_owned()),
                        passed: Some(4),
                        tasks: Some(10),
                    },
                    CodexModelMetric {
                        label: "GPT-5.4 xhigh".to_owned(),
                        score: Some(90.0),
                        status: Some("yellow".to_owned()),
                        passed: Some(6),
                        tasks: Some(10),
                    },
                ],
                quota_5h_20x: None,
                quota_7d_20x: None,
                quota_updated_at: Some("2026-07-30T08:20:35Z".to_owned()),
                quota_policy_5h: Some("temporarily_paused_hidden".to_owned()),
                quota_rows: vec![CodexQuotaMetric {
                    tier: "Plus".to_owned(),
                    basis: Some("estimated".to_owned()),
                    five_h: None,
                    seven_d: Some(84.66),
                }],
                rating_updated_at: Some("2026-08-02T12:42:22Z".to_owned()),
                rating_models: vec![
                    CodexRatingMetric {
                        label: "GPT-5.6 Sol ultra".to_owned(),
                        average: Some(9.0),
                        count: Some(27),
                    },
                    CodexRatingMetric {
                        label: "DeepSeek V4 Flash max".to_owned(),
                        average: Some(8.1),
                        count: Some(57),
                    },
                ],
                source_url: "https://codexradar.com/".to_owned(),
                feedback_url: "https://codexradar.com/".to_owned(),
            }),
            claude: None,
            failures: Vec::new(),
        },
        RadarTarget::Codex,
    );

    assert!(
        body.text
            .contains("最高模型：GPT-5.4 xhigh · IQ 90 · 略低 · 6/10")
    );
    assert!(body.text.contains("IQ 前五配置："));
    assert!(body.text.contains("GPT-5.5 xhigh · IQ 60 · 偏低 · 4/10"));
    assert!(body.text.contains("GPT-5.4 xhigh · IQ 90 · 略低 · 6/10"));
    assert!(body.text.contains("Plus · 7d 84.66 · 估算"));
    assert!(body.text.contains("5h 限制当前暂停"));
    assert!(body.text.contains("24h 社区评分前五："));
    assert!(
        body.text
            .contains("DeepSeek V4 Flash max · 8.10/10 · 57 票")
    );
    assert!(
        body.text
            .contains("数据来自 Codex 雷达：https://codexradar.com/")
    );
    assert!(body.text.contains("模型数据：2026-06-30 18:39:12"));
    assert!(body.text.contains("额度数据：2026-07-30 16:20:35"));
    assert!(body.text.contains("社区评分：2026-08-02 20:42:22"));
}

#[test]
fn format_claude_detail_uses_trial_copy_when_metrics_are_missing() {
    let body = format_radar_reply(
        &RadarSnapshot {
            codex: None,
            claude: Some(ClaudeRadarSummary {
                status: Some("ok".to_owned()),
                updated_at: Some("2026-07-05T09:37:50+08:00".to_owned()),
                quota_updated_at: None,
                quota_5h: None,
                quota_7d: None,
                usage_5h: None,
                usage_7d: None,
                top_iq_model: None,
                top_rating_model: None,
                source_url: "https://claudecoderadar.com/".to_owned(),
                feedback_url: "https://claudecoderadar.com/".to_owned(),
            }),
            failures: Vec::new(),
        },
        RadarTarget::Claude,
    );

    assert!(body.text.contains("状态：🧪 试运行中"));
    assert!(body.text.contains("额度雷达：等待真实数据"));
    assert!(body.text.contains("降智雷达：等待真实数据"));
    assert!(body.text.contains("社区体感分：正在读取"));
    assert!(!body.text.contains("额度：未返回"));
    assert!(!body.text.contains("IQ：未返回"));
    assert!(body.text.contains("更新时间：2026-07-05 09:37:50"));
}

//! Radar 用户可见卡片渲染。
//!
//! 具体指标选择、排序、兼容文案和来源标注属于 Radar 领域规则，Respond 层只消费
//! 已渲染的双通道命令正文。

use qq_maid_common::{
    markdown::{escape_inline, escape_text},
    time_context::format_local_time_for_display,
};

use crate::{
    error::LlmError,
    runtime::respond::{
        command_render::CommandRender,
        common::{CommandBody, truncate_chars},
    },
};

use super::{
    ClaudeModelMetric, ClaudeRadarSummary, CodexModelMetric, CodexQuotaMetric, CodexRadarSummary,
    CodexRatingMetric, RadarIssueTarget, RadarSnapshot, RadarSourceFailure, RadarSourceKind,
    RadarTarget, radar_feedback_url, radar_site_url,
};

const RADAR_SUMMARY_MAX_CHARS: usize = 110;

pub(super) fn format_radar_reply(snapshot: &RadarSnapshot, target: RadarTarget) -> CommandBody {
    let mut render = CommandRender::new();
    match target {
        RadarTarget::All => append_radar_overview(&mut render, snapshot),
        RadarTarget::Codex => {
            render.title("🛰️ Codex Radar");
            if let Some(codex) = snapshot.codex.as_ref() {
                append_codex_detail_card(&mut render, codex);
            }
        }
        RadarTarget::Claude => {
            render.title("🛰️ Claude Code Radar");
            if let Some(claude) = snapshot.claude.as_ref() {
                append_claude_detail_card(&mut render, claude);
            }
        }
    }
    if !snapshot.failures.is_empty() {
        render.blank();
        render.subtitle("读取提示");
        for failure in &snapshot.failures {
            render.bullet(&format_failure(failure));
        }
    }
    render.build()
}

fn append_radar_overview(render: &mut CommandRender, snapshot: &RadarSnapshot) {
    render.title("🛰️ AI 雷达速览");
    if let Some(codex) = snapshot.codex.as_ref() {
        render.blank();
        render.subtitle("Codex Radar");
        render.paragraph(&codex_conclusion(codex));
        if let Some(metrics) = codex_key_metrics(codex) {
            render.bullet(&metrics);
        }
    }
    if let Some(claude) = snapshot.claude.as_ref() {
        render.blank();
        render.subtitle("Claude Code Radar");
        render.paragraph(&claude_conclusion(claude));
        append_claude_overview_metrics(render, claude);
    }
    render.blank();
    render.paragraph("详细看 /rader codex 或 /rader claude");
    append_overview_sources(render, snapshot);
}

pub(super) fn format_radar_issue_reply(target: RadarIssueTarget) -> CommandBody {
    let (name, markdown_name) = match target {
        RadarIssueTarget::Codex => ("Codex Radar", "Codex Radar"),
        RadarIssueTarget::Claude => ("Claude Code Radar", "Claude Code Radar"),
    };
    let site_url = radar_site_url(target);
    let feedback_url = radar_feedback_url(target);
    let markdown = format!(
        "# {markdown_name} 反馈\n\n- 反馈入口：{feedback_url}\n- 来源站点：{site_url}\n\n当前未发现该站点公开 GitHub Issue 仓库或匿名代发 API，请从站点公开反馈入口继续。"
    );
    let text = format!(
        "{name} 反馈\n\n反馈入口：{feedback_url}\n来源站点：{site_url}\n\n当前未发现该站点公开 GitHub Issue 仓库或匿名代发 API，请从站点公开反馈入口继续。"
    );
    CommandBody::dual(text, markdown)
}

pub(super) fn format_radar_total_failure(err: &LlmError) -> CommandBody {
    let message = match err.code.as_str() {
        "timeout" => "雷达数据读取超时了，请稍后再试。",
        "http_error" => "雷达公开数据源暂时不可用，可能是上游接口或网络异常。",
        _ => "雷达数据解析失败或字段缺失，请稍后再试。",
    };
    let markdown = format!("# 🛰️ 雷达摘要\n\n{message}");
    CommandBody::dual(message.to_owned(), markdown)
}

fn append_codex_detail_card(render: &mut CommandRender, summary: &CodexRadarSummary) {
    let mut hidden = false;
    render.blank();
    render.subtitle("结论");
    render.paragraph(&codex_conclusion(summary));

    render.blank();
    render.subtitle("短线判断");
    if let Some(line) = codex_prediction_line(summary) {
        render.bullet(&line);
    } else {
        hidden = true;
        render.paragraph("短线概率当前数据不足。");
    }
    if let Some(prediction) = display_optional(summary.prediction_summary.as_deref()) {
        render.paragraph(&prediction);
    }

    render.blank();
    render.subtitle("额度估算");
    if !summary.quota_rows.is_empty() {
        for quota in &summary.quota_rows {
            render.bullet(&codex_quota_metric_line(quota));
        }
        if summary.quota_policy_5h.as_deref() == Some("temporarily_paused_hidden") {
            render.bullet("5h 限制当前暂停，站点暂不展示该档额度。");
        }
    } else if let Some(line) = codex_quota_line(summary) {
        render.bullet(&line);
    } else {
        hidden = true;
        render.paragraph("额度雷达当前没有可展示数据。");
    }

    render.blank();
    render.subtitle("模型与社区体感");
    let mut has_model_data = false;
    // 两个摘要字段来自不同数据语义，即使当前模型相同也要分别保留。
    if let Some(line) = codex_model_line(summary) {
        has_model_data = true;
        render.paragraph(&line);
    }
    if let Some(line) = codex_top_iq_line(summary) {
        has_model_data = true;
        render.paragraph(&line);
    }
    let ranked_models = codex_ranked_iq_models(&summary.iq_models, 5);
    if !ranked_models.is_empty() {
        has_model_data = true;
        render.blank();
        render.tertiary_title("IQ 前五配置：");
        for (index, model) in ranked_models.into_iter().enumerate() {
            render.paragraph(&format!(
                "{}. {}",
                index + 1,
                codex_model_metric_line(model)
            ));
        }
    }
    let ranked_ratings = codex_ranked_ratings(&summary.rating_models, 5);
    if !ranked_ratings.is_empty() {
        has_model_data = true;
        render.blank();
        render.tertiary_title("24h 社区评分前五：");
        for (index, model) in ranked_ratings.into_iter().enumerate() {
            if let Some(line) = codex_rating_line(Some(model)) {
                render.paragraph(&format!("{}. {line}", index + 1));
            }
        }
    }
    if !has_model_data {
        hidden = true;
        render.paragraph("模型体感当前没有可展示数据。");
    }

    render.blank();
    render.subtitle("更新 / 来源");
    if let Some(updated) = display_timestamp(summary.updated_at.as_deref()) {
        render.bullet(&format!("模型数据：{updated}"));
    }
    if let Some(updated) = display_timestamp(summary.quota_updated_at.as_deref()) {
        render.bullet(&format!("额度数据：{updated}"));
    }
    if let Some(updated) = display_timestamp(summary.rating_updated_at.as_deref()) {
        render.bullet(&format!("社区评分：{updated}"));
    }
    append_link(render, "数据来自 Codex 雷达", &summary.source_url);
    if hidden {
        render.bullet("部分指标当前公开接口未返回，已隐藏空字段。");
    }
}

fn append_claude_detail_card(render: &mut CommandRender, summary: &ClaudeRadarSummary) {
    render.blank();
    render.subtitle("结论");
    render.paragraph(&claude_conclusion(summary));

    render.blank();
    render.subtitle("额度与用量");
    if let Some(line) = claude_quota_line(summary) {
        render.bullet(&line);
    } else {
        render.bullet("额度雷达：等待真实数据");
    }
    append_claude_usage_lines(render, summary);

    render.blank();
    render.subtitle("模型与评分");
    let mut has_model_metric = false;
    if let Some(line) = claude_model_line(summary.top_iq_model.as_ref(), true) {
        has_model_metric = true;
        render.bullet(&format!("IQ 最高模型：{line}"));
    }
    if let Some(line) = claude_model_line(summary.top_rating_model.as_ref(), false) {
        has_model_metric = true;
        render.bullet(&format!("24h 社区评分：{line}"));
    }
    if !has_model_metric {
        render.bullet("降智雷达：等待真实数据");
        render.bullet("社区体感分：正在读取");
    }

    render.blank();
    render.subtitle("更新 / 来源");
    if let Some(updated) = display_timestamp(summary.updated_at.as_deref()) {
        render.bullet(&format!("更新时间：{updated}"));
    }
    if let Some(updated) = display_timestamp(summary.quota_updated_at.as_deref()) {
        render.bullet(&format!("额度更新时间：{updated}"));
    }
    append_link(render, "来源", &summary.source_url);
}

fn codex_conclusion(summary: &CodexRadarSummary) -> String {
    let mut parts = Vec::new();
    if let Some(status) = summary.status.as_deref().and_then(status_label) {
        parts.push(status.to_owned());
    }
    if let Some(action) = summary.action.as_deref().and_then(action_label) {
        parts.push(action.to_owned());
    }
    if let Some(message) = display_optional(summary.window_message.as_deref()) {
        parts.push(message);
    }
    if parts.is_empty() {
        "Codex Radar 当前只有部分公开数据可读。".to_owned()
    } else {
        format!("Codex：{}", parts.join(" · "))
    }
}

fn claude_conclusion(summary: &ClaudeRadarSummary) -> String {
    if claude_has_live_metrics(summary) {
        let status = summary
            .status
            .as_deref()
            .and_then(status_label)
            .unwrap_or("有公开数据更新");
        format!("Claude Code：{status}")
    } else {
        "状态：🧪 试运行中".to_owned()
    }
}

fn codex_key_metrics(summary: &CodexRadarSummary) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(quota) = codex_quota_line(summary) {
        parts.push(quota);
    }
    if let Some(top) = codex_top_iq_line(summary).or_else(|| codex_model_line(summary)) {
        parts.push(top);
    }
    if let Some(rating) = codex_rating_line(codex_top_rating_model(&summary.rating_models)) {
        parts.push(format!("24h 社区评分 {rating}"));
    }
    if let Some(prediction) = codex_prediction_line(summary) {
        parts.push(prediction);
    }
    (!parts.is_empty()).then(|| format!("关键指标：{}", parts.join("；")))
}

fn append_claude_overview_metrics(render: &mut CommandRender, summary: &ClaudeRadarSummary) {
    let mut parts = Vec::new();
    if let Some(quota) = claude_quota_line(summary) {
        parts.push(quota);
    }
    if let Some(usage) = summary
        .usage_5h
        .as_deref()
        .and_then(|value| display_optional(Some(value)))
    {
        parts.push(format!("5h 用量 {usage}"));
    }
    if let Some(model) = claude_model_line(summary.top_iq_model.as_ref(), true) {
        parts.push(format!("IQ 最高 {model}"));
    }
    if let Some(rating) = claude_model_line(summary.top_rating_model.as_ref(), false) {
        parts.push(format!("24h 评分 {rating}"));
    }
    if parts.is_empty() {
        render.bullet("关键指标：🧪 试运行 / 数据不足；额度雷达等待真实数据；降智雷达等待真实数据");
    } else {
        render.bullet(&format!("关键指标：{}", parts.join("；")));
    }
}

fn append_claude_usage_lines(render: &mut CommandRender, summary: &ClaudeRadarSummary) {
    let mut has_usage = false;
    if let Some(usage) = summary
        .usage_5h
        .as_deref()
        .and_then(|value| display_optional(Some(value)))
    {
        has_usage = true;
        render.bullet(&format!("用量 5h：{usage}"));
    }
    if let Some(usage) = summary
        .usage_7d
        .as_deref()
        .and_then(|value| display_optional(Some(value)))
    {
        has_usage = true;
        render.bullet(&format!("用量 7d：{usage}"));
    }
    if !has_usage && !claude_has_live_metrics(summary) {
        render.bullet("用量：等待真实数据");
    }
}

fn append_overview_sources(render: &mut CommandRender, snapshot: &RadarSnapshot) {
    let mut sources = Vec::new();
    if let Some(codex) = snapshot.codex.as_ref() {
        sources.push(format!("Codex {}", codex.source_url));
    }
    if let Some(claude) = snapshot.claude.as_ref() {
        sources.push(format!("Claude {}", claude.source_url));
    }
    if !sources.is_empty() {
        render.bullet(&format!("来源：{}", sources.join("；")));
    }
}

fn append_link(render: &mut CommandRender, label: &str, url: &str) {
    let text = format!("{label}：{url}");
    let markdown = format!("- {}：{}", escape_inline(label), escape_text(url));
    render.push_pair(format!("· {text}"), markdown);
}

fn codex_prediction_line(summary: &CodexRadarSummary) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(level) = summary
        .prediction_level
        .as_deref()
        .and_then(prediction_label)
    {
        parts.push(format!("短线概率：{level}"));
    }
    if let Some(probability) = format_probability(summary.probability_24h) {
        parts.push(format!("24h {probability}"));
    }
    if let Some(probability) = format_probability(summary.probability_48h) {
        parts.push(format!("48h {probability}"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn codex_quota_metric_line(quota: &CodexQuotaMetric) -> String {
    let mut parts = vec![quota.tier.clone()];
    if let Some(five_h) = format_number(quota.five_h) {
        parts.push(format!("5h {five_h}"));
    }
    if let Some(seven_d) = format_number(quota.seven_d) {
        parts.push(format!("7d {seven_d}"));
    }
    if let Some(basis) = display_optional(quota.basis.as_deref()) {
        parts.push(codex_quota_basis_label(&basis).to_owned());
    }
    parts.join(" · ")
}

fn codex_quota_basis_label(value: &str) -> &str {
    match value.trim().to_ascii_lowercase().as_str() {
        "distributed radar" => "分布式雷达实测",
        "estimated" => "估算",
        _ => value,
    }
}

fn codex_quota_line(summary: &CodexRadarSummary) -> Option<String> {
    match (
        format_number(summary.quota_5h_20x),
        format_number(summary.quota_7d_20x),
    ) {
        (Some(five_h), Some(seven_d)) => Some(format!("额度：20x Pro 5h {five_h} / 7d {seven_d}")),
        (Some(five_h), None) => Some(format!("额度：20x Pro 5h {five_h}")),
        (None, Some(seven_d)) => Some(format!("额度：20x Pro 7d {seven_d}")),
        (None, None) => None,
    }
}

fn codex_model_line(summary: &CodexRadarSummary) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(label) = display_optional(summary.model_label.as_deref()) {
        parts.push(label);
    }
    if let Some(score) = format_number(summary.model_score) {
        parts.push(format!("IQ {score}"));
    }
    if let Some(status) = summary.model_status.as_deref().and_then(status_label) {
        parts.push(status.to_owned());
    }
    if let (Some(passed), Some(tasks)) = (summary.model_passed, summary.model_tasks) {
        parts.push(format!("{passed}/{tasks}"));
    }
    (!parts.is_empty()).then(|| format!("模型体感：{}", parts.join(" · ")))
}

fn codex_top_iq_line(summary: &CodexRadarSummary) -> Option<String> {
    let top_models = codex_top_iq_models(&summary.iq_models);
    if top_models.is_empty() {
        return None;
    }
    Some(format!(
        "最高模型：{}",
        top_models
            .iter()
            .map(|model| codex_model_metric_line(model))
            .collect::<Vec<_>>()
            .join("；")
    ))
}

fn codex_top_iq_models(models: &[CodexModelMetric]) -> Vec<&CodexModelMetric> {
    let Some(best_score) = models
        .iter()
        .filter_map(|model| model.score)
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
    else {
        return Vec::new();
    };
    models
        .iter()
        .filter(|model| {
            model
                .score
                .is_some_and(|score| (score - best_score).abs() < f64::EPSILON)
        })
        .collect()
}

fn codex_ranked_iq_models(models: &[CodexModelMetric], limit: usize) -> Vec<&CodexModelMetric> {
    let mut ranked = models.iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .unwrap_or(f64::NEG_INFINITY)
            .partial_cmp(&left.score.unwrap_or(f64::NEG_INFINITY))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.label.cmp(&right.label))
    });
    ranked.truncate(limit);
    ranked
}

fn codex_model_metric_line(model: &CodexModelMetric) -> String {
    let mut parts = vec![model.label.clone()];
    if let Some(score) = format_number(model.score) {
        parts.push(format!("IQ {score}"));
    }
    if let Some(status) = model.status.as_deref().and_then(status_label) {
        parts.push(status.to_owned());
    }
    if let (Some(passed), Some(tasks)) = (model.passed, model.tasks) {
        parts.push(format!("{passed}/{tasks}"));
    }
    parts.join(" · ")
}

fn codex_rating_line(model: Option<&CodexRatingMetric>) -> Option<String> {
    let model = model?;
    let mut parts = vec![model.label.clone()];
    if let Some(score) = format_number(model.average) {
        parts.push(format!("{score}/10"));
    }
    if let Some(count) = model.count {
        parts.push(format!("{count} 票"));
    }
    Some(parts.join(" · "))
}

fn codex_ranked_ratings(models: &[CodexRatingMetric], limit: usize) -> Vec<&CodexRatingMetric> {
    let mut ranked = models.iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .average
            .unwrap_or(f64::NEG_INFINITY)
            .partial_cmp(&left.average.unwrap_or(f64::NEG_INFINITY))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.count.cmp(&left.count))
            .then_with(|| left.label.cmp(&right.label))
    });
    ranked.truncate(limit);
    ranked
}

fn codex_top_rating_model(models: &[CodexRatingMetric]) -> Option<&CodexRatingMetric> {
    models
        .iter()
        .max_by(|left, right| compare_optional_rating(left.average, right.average))
}

fn compare_optional_rating(left: Option<f64>, right: Option<f64>) -> std::cmp::Ordering {
    left.unwrap_or(f64::NEG_INFINITY)
        .partial_cmp(&right.unwrap_or(f64::NEG_INFINITY))
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn claude_quota_line(summary: &ClaudeRadarSummary) -> Option<String> {
    match (
        format_number(summary.quota_5h),
        format_number(summary.quota_7d),
    ) {
        (Some(five_h), Some(seven_d)) => Some(format!("额度：5h {five_h} / 7d {seven_d}")),
        (Some(five_h), None) => Some(format!("额度：5h {five_h}")),
        (None, Some(seven_d)) => Some(format!("额度：7d {seven_d}")),
        (None, None) => None,
    }
}

fn claude_model_line(model: Option<&ClaudeModelMetric>, include_pass: bool) -> Option<String> {
    let model = model?;
    let mut line = model.name.clone();
    if let Some(score) = format_number(model.score) {
        line.push_str(&format!(" {score}"));
    }
    let pass = if include_pass {
        match (model.passed, model.valid, model.invalid) {
            (Some(passed), Some(valid), Some(invalid)) if invalid > 0 => {
                format!(" · {passed}/{valid} · {invalid} invalid")
            }
            (Some(passed), Some(valid), _) => format!(" · {passed}/{valid}"),
            _ => String::new(),
        }
    } else {
        model
            .valid
            .map(|count| format!(" · 样本 {count}"))
            .unwrap_or_default()
    };
    line.push_str(&pass);
    Some(line)
}

fn claude_has_live_metrics(summary: &ClaudeRadarSummary) -> bool {
    summary.quota_5h.is_some()
        || summary.quota_7d.is_some()
        || summary.usage_5h.is_some()
        || summary.usage_7d.is_some()
        || summary.top_iq_model.is_some()
        || summary.top_rating_model.is_some()
}

fn format_failure(failure: &RadarSourceFailure) -> String {
    let source = match failure.source {
        RadarSourceKind::Codex => "Codex Radar",
        RadarSourceKind::Claude => "Claude Code Radar",
    };
    let reason = match failure.code.as_str() {
        "timeout" => "读取超时",
        "http_error" => "公开接口不可用",
        _ => "解析失败或字段缺失",
    };
    format!("{source}：{reason}（{}）", failure.stage)
}
fn display_optional(value: Option<&str>) -> Option<String> {
    value
        .map(|value| truncate_chars(value, RADAR_SUMMARY_MAX_CHARS))
        .filter(|value| !value.trim().is_empty())
}

/// 统一把上游时间戳转成本地可读时间，避免 Radar 卡片直接暴露 RFC3339 的 `T`/`Z`。
/// 无法解析的值仍保留原文，便于排查上游字段格式变化。
fn display_timestamp(value: Option<&str>) -> Option<String> {
    value
        .map(format_local_time_for_display)
        .map(|value| truncate_chars(&value, RADAR_SUMMARY_MAX_CHARS))
        .filter(|value| !value.trim().is_empty())
}

fn status_label(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "community_confirmed" => Some("社区确认"),
        "reset_completed" => Some("重置已完成"),
        "red" => Some("偏低"),
        "yellow" => Some("略低"),
        "green" => Some("正常"),
        "ok" => Some("正常"),
        "error" => Some("异常"),
        _ => None,
    }
}

fn action_label(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "reset_completed" => Some("重置已完成"),
        "wait" | "hold" => Some("继续观察"),
        "avoid" => Some("暂缓使用"),
        "use" | "go" => Some("可以使用"),
        _ => None,
    }
}

fn prediction_label(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "high" => Some("偏高"),
        "medium" | "moderate" => Some("中等"),
        "low" => Some("偏低"),
        _ => None,
    }
}

fn format_probability(value: Option<f64>) -> Option<String> {
    value.map(|value| format!("{:.0}%", value * 100.0))
}

fn format_number(value: Option<f64>) -> Option<String> {
    let value = value?;
    if value.fract().abs() < 0.005 {
        Some(format!("{value:.0}"))
    } else {
        Some(format!("{value:.2}"))
    }
}

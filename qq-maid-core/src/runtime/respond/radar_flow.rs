//! `/rader` / `/radar` / `/雷达` 指令处理。
//!
//! slash 层只解析命令、调用雷达执行器并负责展示；公开数据源读取和字段兼容在
//! `runtime::tools::radar` 中维护，避免把外部看板接入细节散落到 respond 主流程。

use serde_json::json;

use crate::{
    error::LlmError,
    runtime::{
        command::{ParsedCommand, parse_slash_command},
        session::SessionRecord,
        tools::{
            ClaudeModelMetric, ClaudeRadarSummary, CodexModelMetric, CodexRadarSummary,
            RadarIssueTarget, RadarSnapshot, RadarSourceFailure, RadarSourceKind, RadarTarget,
            radar_feedback_url, radar_site_url,
        },
    },
};

use super::{
    RespondResponse, RustRespondService,
    command_render::{CommandRender, escape_markdown_inline, escape_markdown_text},
    common::{CommandBody, command_response, session_error, truncate_chars},
};

const RADAR_USAGE_REPLY: &str = "用法：/rader [codex|claude]，或 /rader issue [codex|claude]
别名：/radar、/雷达";
const RADAR_SUMMARY_MAX_CHARS: usize = 110;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RadarCommand {
    Show(RadarTarget),
    Issue(RadarIssueTarget),
    Usage,
}

impl RustRespondService {
    pub(super) async fn handle_radar_command(
        &self,
        command: ParsedCommand,
        user_text: &str,
        session: &mut SessionRecord,
    ) -> Result<RespondResponse, LlmError> {
        match parse_radar_action(&command.argument) {
            RadarCommand::Usage => Ok(command_response(
                RADAR_USAGE_REPLY,
                Some(session.session_id.clone()),
                Some(command.action),
            )),
            RadarCommand::Issue(target) => {
                let body = format_radar_issue_reply(target);
                self.session_store
                    .append_exchange(session, user_text, &body.text)
                    .map_err(session_error)?;
                Ok(command_response(
                    body,
                    Some(session.session_id.clone()),
                    Some(command.action),
                ))
            }
            RadarCommand::Show(target) => {
                let outcome = match self.radar_executor.radar(target).await {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        tracing::warn!(
                            error_code = %err.code,
                            error_stage = %err.stage,
                            radar_provider = self.radar_executor.provider_name(),
                            "radar command failed"
                        );
                        let body = format_radar_total_failure(&err);
                        self.session_store
                            .append_exchange(session, user_text, &body.text)
                            .map_err(session_error)?;
                        let mut response = command_response(
                            body,
                            Some(session.session_id.clone()),
                            Some(command.action),
                        );
                        response.diagnostics = Some(json!({
                            "backend": "rust",
                            "session_backend": "rust",
                            "used_memory": false,
                            "used_search": false,
                            "used_weather": false,
                            "used_radar": true,
                            "radar_provider": self.radar_executor.provider_name(),
                            "radar_error_code": err.code,
                            "radar_error_stage": err.stage,
                        }));
                        return Ok(response);
                    }
                };
                let body = format_radar_reply(&outcome, target);
                self.session_store
                    .append_exchange(session, user_text, &body.text)
                    .map_err(session_error)?;
                let mut response =
                    command_response(body, Some(session.session_id.clone()), Some(command.action));
                response.diagnostics = Some(json!({
                    "backend": "rust",
                    "session_backend": "rust",
                    "used_memory": false,
                    "used_search": false,
                    "used_weather": false,
                    "used_radar": true,
                    "radar_provider": self.radar_executor.provider_name(),
                    "radar_target": radar_target_label(target),
                    "radar_codex_ok": outcome.codex.is_some(),
                    "radar_claude_ok": outcome.claude.is_some(),
                    "radar_failure_count": outcome.failures.len(),
                }));
                Ok(response)
            }
        }
    }
}

pub(super) fn parse_radar_command(text: &str) -> Option<ParsedCommand> {
    let command = parse_slash_command(text)?;
    (command.action == "radar").then_some(command)
}

pub(super) fn parse_radar_action(argument: &str) -> RadarCommand {
    let mut parts = argument.split_whitespace();
    let Some(first) = parts.next() else {
        return RadarCommand::Show(RadarTarget::All);
    };
    let first = first.to_ascii_lowercase();
    if first == "issue" || first == "反馈" {
        return parts
            .next()
            .and_then(parse_issue_target)
            .map(RadarCommand::Issue)
            .unwrap_or(RadarCommand::Usage);
    }
    parse_show_target(&first)
        .map(RadarCommand::Show)
        .unwrap_or(RadarCommand::Usage)
}

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

fn format_radar_issue_reply(target: RadarIssueTarget) -> CommandBody {
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

fn format_radar_total_failure(err: &LlmError) -> CommandBody {
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

    render.blank();
    render.subtitle("额度估算");
    if let Some(line) = codex_quota_line(summary) {
        render.bullet(&line);
    } else {
        hidden = true;
        render.paragraph("额度雷达当前没有可展示数据。");
    }

    render.blank();
    render.subtitle("模型体感");
    let mut has_model_data = false;
    if let Some(line) = codex_model_line(summary) {
        has_model_data = true;
        render.bullet(&line);
    }
    if let Some(line) = codex_top_iq_line(summary) {
        has_model_data = true;
        render.bullet(&line);
    }
    if !summary.iq_models.is_empty() {
        has_model_data = true;
        render.bullet("完整模型列表：");
        for model in &summary.iq_models {
            render.bullet(&codex_model_metric_line(model));
        }
    }
    if !has_model_data {
        hidden = true;
        render.paragraph("模型体感当前没有可展示数据。");
    }

    render.blank();
    render.subtitle("更新 / 来源");
    if let Some(updated) = display_optional(summary.updated_at.as_deref()) {
        render.bullet(&format!("更新时间：{updated}"));
    }
    append_link(render, "来源", &summary.source_url);
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
    if let Some(updated) = display_optional(summary.updated_at.as_deref()) {
        render.bullet(&format!("更新时间：{updated}"));
    }
    if let Some(updated) = display_optional(summary.quota_updated_at.as_deref()) {
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
    let markdown = format!(
        "- {}：{}",
        escape_markdown_inline(label),
        escape_markdown_text(url)
    );
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
    (!parts.is_empty()).then(|| parts.join(" · "))
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

fn parse_show_target(token: &str) -> Option<RadarTarget> {
    match token {
        "all" | "全部" => Some(RadarTarget::All),
        "codex" | "code" => Some(RadarTarget::Codex),
        "claude" | "cc" | "claude-code" => Some(RadarTarget::Claude),
        _ => None,
    }
}

fn parse_issue_target(token: &str) -> Option<RadarIssueTarget> {
    match token.to_ascii_lowercase().as_str() {
        "codex" | "code" => Some(RadarIssueTarget::Codex),
        "claude" | "cc" | "claude-code" => Some(RadarIssueTarget::Claude),
        _ => None,
    }
}

fn radar_target_label(target: RadarTarget) -> &'static str {
    match target {
        RadarTarget::All => "all",
        RadarTarget::Codex => "codex",
        RadarTarget::Claude => "claude",
    }
}

fn display_optional(value: Option<&str>) -> Option<String> {
    value
        .map(|value| truncate_chars(value, RADAR_SUMMARY_MAX_CHARS))
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

#[cfg(test)]
mod tests {
    use crate::runtime::tools::{
        ClaudeRadarSummary, CodexModelMetric, CodexRadarSummary, RadarSnapshot, RadarSourceFailure,
        RadarSourceKind,
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
                    model_score: None,
                    model_status: None,
                    model_passed: None,
                    model_tasks: None,
                    model_label: None,
                    iq_models: Vec::new(),
                    quota_5h_20x: None,
                    quota_7d_20x: None,
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
                    model_score: None,
                    model_status: None,
                    model_passed: None,
                    model_tasks: None,
                    model_label: None,
                    iq_models: Vec::new(),
                    quota_5h_20x: None,
                    quota_7d_20x: None,
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
    fn format_codex_detail_shows_top_model_and_complete_current_list() {
        let body = format_radar_reply(
            &RadarSnapshot {
                codex: Some(CodexRadarSummary {
                    status: Some("community_confirmed".to_owned()),
                    updated_at: Some("2026-06-30T18:39:12+08:00".to_owned()),
                    action: Some("reset_completed".to_owned()),
                    window_message: None,
                    prediction_level: None,
                    probability_24h: None,
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
        assert!(body.text.contains("完整模型列表："));
        assert!(body.text.contains("GPT-5.5 xhigh · IQ 60 · 偏低 · 4/10"));
        assert!(body.text.contains("GPT-5.4 xhigh · IQ 90 · 略低 · 6/10"));
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
    }
}

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
            ClaudeModelMetric, ClaudeRadarSummary, CodexRadarSummary, RadarIssueTarget,
            RadarSnapshot, RadarSourceFailure, RadarSourceKind, RadarTarget, radar_feedback_url,
            radar_site_url,
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
                let body = format_radar_reply(&outcome);
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

pub(super) fn format_radar_reply(snapshot: &RadarSnapshot) -> CommandBody {
    let mut render = CommandRender::new();
    render.title("🛰️ 雷达摘要");
    if let Some(codex) = snapshot.codex.as_ref() {
        render.blank();
        append_codex_card(&mut render, codex);
    }
    if let Some(claude) = snapshot.claude.as_ref() {
        render.blank();
        append_claude_card(&mut render, claude);
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

fn append_codex_card(render: &mut CommandRender, summary: &CodexRadarSummary) {
    render.push_pair(
        format!(
            "Codex Radar · {}",
            display_value(summary.status.as_deref(), "状态未返回")
        ),
        format!(
            "## Codex Radar · {}",
            escape_markdown_inline(&display_value(summary.status.as_deref(), "状态未返回"))
        ),
    );
    render.bullet(&format!(
        "更新时间：{}",
        display_value(summary.updated_at.as_deref(), "未返回")
    ));
    render.bullet(&format!(
        "窗口：{}",
        display_value(summary.window_message.as_deref(), "未返回窗口说明")
    ));
    render.bullet(&format!(
        "预测：{}{}",
        display_value(summary.prediction_level.as_deref(), "未返回"),
        summary
            .probability_24h
            .map(|value| format!(" · 24h {:.0}%", value * 100.0))
            .unwrap_or_default()
    ));
    render.bullet(&format!(
        "IQ：{}",
        format_score_line(
            summary.model_score,
            summary.model_status.as_deref(),
            summary.model_passed,
            summary.model_tasks,
        )
    ));
    render.bullet(&format!(
        "额度：20x Pro 5h {} / 7d {}",
        format_number(summary.quota_5h_20x),
        format_number(summary.quota_7d_20x)
    ));
    append_link(render, "来源", &summary.source_url);
}

fn append_claude_card(render: &mut CommandRender, summary: &ClaudeRadarSummary) {
    render.push_pair(
        format!(
            "Claude Code Radar · {}",
            display_value(summary.status.as_deref(), "状态未返回")
        ),
        format!(
            "## Claude Code Radar · {}",
            escape_markdown_inline(&display_value(summary.status.as_deref(), "状态未返回"))
        ),
    );
    render.bullet(&format!(
        "更新时间：{}",
        display_value(summary.updated_at.as_deref(), "未返回")
    ));
    render.bullet(&format!(
        "额度：5h {} / 7d {}",
        format_number(summary.quota_5h),
        format_number(summary.quota_7d)
    ));
    if let Some(usage) = summary.usage_5h.as_deref() {
        render.bullet(&format!(
            "用量：{}",
            truncate_chars(usage, RADAR_SUMMARY_MAX_CHARS)
        ));
    }
    if let Some(usage) = summary.usage_7d.as_deref() {
        render.bullet(&format!(
            "用量：{}",
            truncate_chars(usage, RADAR_SUMMARY_MAX_CHARS)
        ));
    }
    render.bullet(&format!(
        "IQ 最高：{}",
        format_claude_model(summary.top_iq_model.as_ref(), true)
    ));
    render.bullet(&format!(
        "24h 评分：{}",
        format_claude_model(summary.top_rating_model.as_ref(), false)
    ));
    append_link(render, "来源", &summary.source_url);
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

fn format_claude_model(model: Option<&ClaudeModelMetric>, include_pass: bool) -> String {
    let Some(model) = model else {
        return "未返回".to_owned();
    };
    let score = model
        .score
        .map(|score| format_number(Some(score)))
        .unwrap_or_else(|| "未返回".to_owned());
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
    format!("{} {score}{pass}", model.name)
}

fn format_score_line(
    score: Option<f64>,
    status: Option<&str>,
    passed: Option<u64>,
    tasks: Option<u64>,
) -> String {
    let score = score
        .map(|score| format_number(Some(score)))
        .unwrap_or_else(|| "未返回".to_owned());
    let status = status
        .map(|status| format!(" · {status}"))
        .unwrap_or_default();
    let pass = match (passed, tasks) {
        (Some(passed), Some(tasks)) => format!(" · {passed}/{tasks}"),
        _ => String::new(),
    };
    format!("{score}{status}{pass}")
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

fn display_value(value: Option<&str>, fallback: &str) -> String {
    value
        .map(|value| truncate_chars(value, RADAR_SUMMARY_MAX_CHARS))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

fn format_number(value: Option<f64>) -> String {
    let Some(value) = value else {
        return "未返回".to_owned();
    };
    if value.fract().abs() < 0.005 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::tools::{
        CodexRadarSummary, RadarSnapshot, RadarSourceFailure, RadarSourceKind,
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
    fn format_radar_reply_surfaces_missing_fields_and_partial_failure() {
        let body = format_radar_reply(&RadarSnapshot {
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
        });

        assert!(body.text.contains("Codex Radar · 状态未返回"));
        assert!(body.text.contains("窗口：未返回窗口说明"));
        assert!(body.text.contains("IQ：未返回"));
        assert!(body.text.contains("Claude Code Radar：读取超时"));
        assert!(body.markdown.unwrap().contains("## 读取提示"));
    }
}

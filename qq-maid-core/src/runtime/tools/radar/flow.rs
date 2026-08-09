//! Radar slash 指令入口。
//!
//! 这里只负责命令解析、执行器调用、会话写入和诊断装配；公开数据读取与卡片展示
//! 分别由同领域的客户端和 format 模块维护。

use serde_json::json;

use crate::{
    error::LlmError,
    runtime::{
        command::{ParsedCommand, parse_slash_command},
        respond::{
            RespondResponse, RustRespondService,
            common::{command_response, session_error},
        },
        session::SessionRecord,
    },
};

use super::{
    RadarIssueTarget, RadarTarget,
    format::{format_radar_issue_reply, format_radar_reply, format_radar_total_failure},
};

const RADAR_USAGE_REPLY: &str = "用法：/rader [codex|claude]，或 /rader issue [codex|claude]
别名：/radar、/雷达";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RadarCommand {
    Show(RadarTarget),
    Issue(RadarIssueTarget),
    Usage,
}

impl RustRespondService {
    pub(crate) async fn handle_radar_command(
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
                            "Radar 命令执行失败"
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

pub(crate) fn parse_radar_command(text: &str) -> Option<ParsedCommand> {
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

#[cfg(test)]
mod tests;

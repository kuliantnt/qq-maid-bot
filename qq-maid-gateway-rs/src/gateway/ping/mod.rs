//! Gateway 本地 `/ping` 诊断入口。
//!
//! 该模块只负责识别命令、采集 auth / Core health snapshot 并编排渲染；
//! 运行事实、健康评估、Markdown 展示和上游状态判断分别放在子模块中。

mod assess;
mod healthz;
mod render;
mod status;
mod time;

#[cfg(test)]
mod tests;

use std::time::Duration;

use crate::{
    auth::{AccessTokenSnapshot, AccessTokenSnapshotState},
    config::AppConfig,
    gateway::command::GatewayCommandContext,
};
use qq_maid_common::time_context::now_unix_seconds_marker;
use qq_maid_core::service::CoreHealthSnapshot;

use self::healthz::{LlmUpstreamSnapshot, core_health_snapshot};

pub use self::status::{GatewayRuntimeSnapshot, GatewayRuntimeStatus, InvalidSessionSnapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PingMode {
    Summary,
    All,
    Check,
}

pub fn is_ping_command(text: &str) -> bool {
    parse_ping_mode(text).is_some()
}

pub fn is_ping_check_command(text: &str) -> bool {
    matches!(parse_ping_mode(text), Some(PingMode::Check))
}

fn parse_ping_mode(text: &str) -> Option<PingMode> {
    let mut parts = text.split_whitespace();
    let command = parts.next()?;
    if !command.eq_ignore_ascii_case("/ping") {
        return None;
    }
    match (parts.next(), parts.next()) {
        (None, None) => Some(PingMode::Summary),
        (Some(arg), None) if arg.eq_ignore_ascii_case("all") => Some(PingMode::All),
        (Some(arg), None) if arg.eq_ignore_ascii_case("check") => Some(PingMode::Check),
        _ => None,
    }
}

pub(crate) fn empty_token_snapshot(refresh_margin: Duration) -> AccessTokenSnapshot {
    AccessTokenSnapshot {
        state: AccessTokenSnapshotState::Empty,
        expires_in_seconds: None,
        refresh_margin_seconds: refresh_margin.as_secs(),
    }
}

pub(crate) fn build_ping_reply(
    command_text: &str,
    context: &GatewayCommandContext,
    config: &AppConfig,
    runtime: &GatewayRuntimeStatus,
    token_snapshot: &AccessTokenSnapshot,
    core_health: &CoreHealthSnapshot,
    check_failure: Option<&str>,
    application_version: &str,
) -> String {
    let mut llm_health = core_health_snapshot(core_health);
    if let Some(summary) = check_failure {
        // 主动检查的直接失败必须覆盖旧 healthz 快照，避免 `/ping check` 误报绿色。
        llm_health.upstream = LlmUpstreamSnapshot::Error {
            last_checked_at: Some(now_unix_seconds_marker()),
            error_summary: summary.to_owned(),
        };
    }
    render::render_ping_reply_at_with_version(
        context,
        config,
        runtime,
        token_snapshot,
        &llm_health,
        parse_ping_mode(command_text).unwrap_or(PingMode::Summary),
        qq_maid_common::time_context::now_unix_seconds(),
        application_version,
    )
}

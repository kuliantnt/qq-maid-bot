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

use crate::{auth::AccessTokenManager, config::AppConfig, gateway::event::C2cMessage};
use qq_maid_common::time_context::now_unix_seconds_marker;
use qq_maid_core::service::CoreHealthSnapshot;

use self::{
    healthz::{LlmUpstreamSnapshot, core_health_snapshot},
    render::render_c2c_ping_reply,
};

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

pub async fn build_c2c_ping_reply(
    message: &C2cMessage,
    config: &AppConfig,
    runtime: &GatewayRuntimeStatus,
    auth: &AccessTokenManager,
) -> String {
    let snapshot = qq_maid_core::service::CoreHealthSnapshot {
        ok: false,
        provider: String::new(),
        model: String::new(),
        stream: false,
        upstream: Default::default(),
    };
    build_c2c_ping_reply_with_check_failure(message, config, runtime, auth, &snapshot, None).await
}

pub async fn build_c2c_ping_reply_with_check_failure(
    message: &C2cMessage,
    config: &AppConfig,
    runtime: &GatewayRuntimeStatus,
    auth: &AccessTokenManager,
    core_health: &CoreHealthSnapshot,
    check_failure: Option<&str>,
) -> String {
    let token_snapshot = auth.snapshot().await;
    let mut llm_health = core_health_snapshot(core_health);
    if let Some(summary) = check_failure {
        // 主动检查的直接失败必须覆盖旧 healthz 快照，避免 `/ping check` 误报绿色。
        llm_health.upstream = LlmUpstreamSnapshot::Error {
            last_checked_at: Some(now_unix_seconds_marker()),
            error_summary: summary.to_owned(),
        };
    }
    render_c2c_ping_reply(message, config, runtime, &token_snapshot, &llm_health)
}

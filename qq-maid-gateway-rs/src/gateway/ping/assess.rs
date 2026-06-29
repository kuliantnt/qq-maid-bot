use qq_maid_common::time_context::{
    diagnostic_time_unix_seconds, format_diagnostic_clock_time_for_display,
    format_diagnostic_elapsed_between_for_display,
};

use crate::auth::{AccessTokenSnapshot, AccessTokenSnapshotState};

use super::{
    healthz::{LlmHealthSnapshot, LlmUpstreamSnapshot, healthz_status_detail, llm_health_ok},
    status::{GatewayRuntimeSnapshot, GatewayRuntimeStatus},
    time::{age_seconds, time_ago},
};

const HEARTBEAT_ACK_WARN_SECONDS: i64 = 90;
const HEARTBEAT_ACK_ERROR_SECONDS: i64 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum PingSeverity {
    Normal,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PingTableRow {
    pub(super) module: String,
    pub(super) status: String,
    pub(super) detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PingAssessment {
    pub(super) overall: PingSeverity,
    pub(super) summary: String,
    pub(super) rows: Vec<PingTableRow>,
    pub(super) events: Vec<String>,
}

pub(super) fn assess_ping_status(
    snapshot: &GatewayRuntimeSnapshot,
    runtime: &GatewayRuntimeStatus,
    token_snapshot: &AccessTokenSnapshot,
    llm_health: &LlmHealthSnapshot,
    now_seconds: i64,
) -> PingAssessment {
    let mut overall = PingSeverity::Normal;
    let mut notes = Vec::new();

    // 状态判定只使用已有采集字段，避免 `/ping` 为了展示改动底层连接和重连语义。
    let gateway_row = gateway_row(snapshot, runtime);
    collect_row_severity(&gateway_row, &mut overall);
    if snapshot.state_error.is_some() {
        notes.push("运行时状态读取失败".to_owned());
        overall = overall.max(PingSeverity::Error);
    }

    let qq_row = qq_connection_row(snapshot, now_seconds);
    collect_row_severity(&qq_row, &mut overall);
    collect_reconnect_note(snapshot, &mut notes, &mut overall);

    let heartbeat_row = heartbeat_row(snapshot, runtime, now_seconds);
    collect_row_severity(&heartbeat_row, &mut overall);

    let (llm_service_row, llm_upstream_row) = llm_rows(llm_health, now_seconds);
    collect_row_severity(&llm_service_row, &mut overall);
    collect_row_severity(&llm_upstream_row, &mut overall);
    match &llm_health.upstream {
        LlmUpstreamSnapshot::Unverified if llm_health_ok(llm_health) => {
            notes.push("LLM 上游尚未验证".to_owned());
        }
        LlmUpstreamSnapshot::Available {
            fallback_used: true,
            ..
        } => notes.push("LLM 上游最近一次调用发生过降级".to_owned()),
        LlmUpstreamSnapshot::Error { .. } => notes.push("LLM 上游最近一次调用失败".to_owned()),
        _ => {}
    }

    let receive_row = receive_row(snapshot, now_seconds);
    collect_row_severity(&receive_row, &mut overall);

    let send_row = send_row(snapshot, now_seconds);
    collect_row_severity(&send_row, &mut overall);

    collect_token_note(token_snapshot, &mut notes, &mut overall);
    collect_attempt_note(
        latest_attempt(
            snapshot.last_qq_send_success_at.as_deref(),
            snapshot.last_qq_send_failure_at.as_deref(),
        ),
        snapshot.last_qq_send_failure_at.is_some(),
        "最近一次 QQ 发送尝试失败",
        "QQ 发送失败已恢复",
        PingSeverity::Error,
        &mut notes,
        &mut overall,
    );
    collect_attempt_note(
        latest_attempt(
            snapshot.last_respond_success_at.as_deref(),
            snapshot.last_respond_failure_at.as_deref(),
        ),
        snapshot.last_respond_failure_at.is_some(),
        "最近一次 LLM respond 失败",
        "LLM respond 失败已恢复",
        PingSeverity::Warning,
        &mut notes,
        &mut overall,
    );

    let rows = vec![
        gateway_row,
        qq_row,
        heartbeat_row,
        llm_service_row,
        llm_upstream_row,
        receive_row,
        send_row,
    ];
    let events = recent_events(snapshot, llm_health, now_seconds);
    let summary = summary_text(overall, &notes);

    PingAssessment {
        overall,
        summary,
        rows,
        events,
    }
}

fn gateway_row(snapshot: &GatewayRuntimeSnapshot, runtime: &GatewayRuntimeStatus) -> PingTableRow {
    match snapshot.state_error.as_deref() {
        Some(error) => row("Gateway", PingSeverity::Error, "异常", error),
        None => row(
            "Gateway",
            PingSeverity::Normal,
            "正常",
            &format!("已运行 {}", runtime.uptime_text()),
        ),
    }
}

fn qq_connection_row(snapshot: &GatewayRuntimeSnapshot, now_seconds: i64) -> PingTableRow {
    let Some(connected_at) = snapshot.last_gateway_connected_at.as_deref() else {
        return row(
            "QQ 连接",
            PingSeverity::Warning,
            "待确认",
            "尚未记录 WebSocket 连接",
        );
    };

    let mut detail = format!("WebSocket 已连接于 {}", time_ago(connected_at, now_seconds));
    if let Some(reconnect_at) = snapshot.last_reconnect_at.as_deref() {
        let reconnect_ago = time_ago(reconnect_at, now_seconds);
        if let Some(recovered_at) = reconnect_recovered_at(snapshot, reconnect_at) {
            let recovered = recovery_elapsed_text(reconnect_at, recovered_at, "后恢复", "已恢复");
            detail = format!("{reconnect_ago}发生重连，{recovered}");
        } else {
            return row(
                "QQ 连接",
                PingSeverity::Error,
                "异常",
                &format!("{reconnect_ago}发生重连，当前未发现恢复记录"),
            );
        }
    }

    row("QQ 连接", PingSeverity::Normal, "已连接", &detail)
}

fn heartbeat_row(
    snapshot: &GatewayRuntimeSnapshot,
    runtime: &GatewayRuntimeStatus,
    now_seconds: i64,
) -> PingTableRow {
    let Some(ack_at) = snapshot.last_heartbeat_ack_at.as_deref() else {
        let severity = if runtime.started_elapsed().as_secs() > HEARTBEAT_ACK_ERROR_SECONDS as u64 {
            PingSeverity::Error
        } else {
            PingSeverity::Warning
        };
        return row("心跳", severity, "待确认", "尚未收到 ACK");
    };

    let age = age_seconds(ack_at, now_seconds);
    let severity = if age.is_some_and(|age| age > HEARTBEAT_ACK_ERROR_SECONDS) {
        PingSeverity::Error
    } else if age.is_some_and(|age| age > HEARTBEAT_ACK_WARN_SECONDS) {
        PingSeverity::Warning
    } else {
        PingSeverity::Normal
    };
    let label = match severity {
        PingSeverity::Normal => "正常",
        PingSeverity::Warning => "延迟偏高",
        PingSeverity::Error => "超时",
    };
    row(
        "心跳",
        severity,
        label,
        &format!("{}收到 ACK", time_ago(ack_at, now_seconds)),
    )
}

fn llm_rows(llm_health: &LlmHealthSnapshot, now_seconds: i64) -> (PingTableRow, PingTableRow) {
    if !llm_health_ok(llm_health) {
        return (
            row(
                "LLM 服务",
                PingSeverity::Error,
                "异常",
                &healthz_status_detail(llm_health),
            ),
            row(
                "LLM 上游",
                PingSeverity::Warning,
                "无法确认",
                "本地服务异常，无法读取上游状态",
            ),
        );
    }

    let service = row(
        "LLM 服务",
        PingSeverity::Normal,
        "在线",
        &healthz_status_detail(llm_health),
    );
    let upstream = match &llm_health.upstream {
        LlmUpstreamSnapshot::Unavailable => row(
            "LLM 上游",
            PingSeverity::Warning,
            "无法确认",
            "healthz 未返回上游状态",
        ),
        LlmUpstreamSnapshot::Unverified => row(
            "LLM 上游",
            PingSeverity::Warning,
            "未验证",
            "当前进程尚无真实调用记录，可发送 /ping check 验证",
        ),
        LlmUpstreamSnapshot::Available {
            last_success_at,
            provider,
            model,
            fallback_used,
        } => {
            let when = last_success_at
                .as_deref()
                .map(|value| time_ago(value, now_seconds))
                .unwrap_or_else(|| "未知时间".to_owned());
            let route = match (provider.as_deref(), model.as_deref()) {
                (Some(provider), Some(model)) => format!("{provider}/{model}"),
                _ => "provider/model 未提供".to_owned(),
            };
            if *fallback_used {
                row(
                    "LLM 上游",
                    PingSeverity::Warning,
                    "可用（已降级）",
                    &format!("最近成功于 {when}；最终使用 {route}"),
                )
            } else {
                row(
                    "LLM 上游",
                    PingSeverity::Normal,
                    "可用",
                    &format!("最近成功于 {when}；使用 {route}"),
                )
            }
        }
        LlmUpstreamSnapshot::Error {
            last_checked_at,
            error_summary,
        } => {
            let when = last_checked_at
                .as_deref()
                .map(|value| time_ago(value, now_seconds))
                .unwrap_or_else(|| "未知时间".to_owned());
            row(
                "LLM 上游",
                PingSeverity::Error,
                "异常",
                &format!("最近失败于 {when}；{error_summary}"),
            )
        }
    };
    (service, upstream)
}

fn receive_row(snapshot: &GatewayRuntimeSnapshot, now_seconds: i64) -> PingTableRow {
    match snapshot.last_c2c_received_at.as_deref() {
        Some(received_at) => row(
            "消息接收",
            PingSeverity::Normal,
            "正常",
            &format!("{}收到当前消息", time_ago(received_at, now_seconds)),
        ),
        None => row(
            "消息接收",
            PingSeverity::Warning,
            "待确认",
            "尚未记录收到消息",
        ),
    }
}

fn send_row(snapshot: &GatewayRuntimeSnapshot, now_seconds: i64) -> PingTableRow {
    match latest_attempt(
        snapshot.last_qq_send_success_at.as_deref(),
        snapshot.last_qq_send_failure_at.as_deref(),
    ) {
        Some(AttemptStatus::Success { at }) => row(
            "消息发送",
            PingSeverity::Normal,
            "正常",
            &format!("最近一次发送尝试成功于 {}", time_ago(at, now_seconds)),
        ),
        Some(AttemptStatus::Failure { at }) => {
            let mut detail = format!("最近一次发送尝试失败于 {}", time_ago(at, now_seconds));
            if let Some(summary) = snapshot.last_qq_send_failure_summary.as_deref() {
                detail.push_str(&format!("：{summary}"));
            }
            row("消息发送", PingSeverity::Error, "异常", &detail)
        }
        None => row(
            "消息发送",
            PingSeverity::Normal,
            "未发现失败",
            "暂无发送尝试记录",
        ),
    }
}

fn row(module: &str, severity: PingSeverity, label: &str, detail: &str) -> PingTableRow {
    PingTableRow {
        module: module.to_owned(),
        status: format!("{} {label}", severity_icon(severity)),
        detail: detail.to_owned(),
    }
}

fn severity_icon(severity: PingSeverity) -> &'static str {
    match severity {
        PingSeverity::Normal => "🟢",
        PingSeverity::Warning => "🟡",
        PingSeverity::Error => "🔴",
    }
}

fn collect_row_severity(row: &PingTableRow, overall: &mut PingSeverity) {
    if row.status.starts_with("🔴") {
        *overall = (*overall).max(PingSeverity::Error);
    } else if row.status.starts_with("🟡") {
        *overall = (*overall).max(PingSeverity::Warning);
    }
}

fn collect_reconnect_note(
    snapshot: &GatewayRuntimeSnapshot,
    notes: &mut Vec<String>,
    overall: &mut PingSeverity,
) {
    if let Some(reconnect_at) = snapshot.last_reconnect_at.as_deref() {
        if reconnect_recovered_at(snapshot, reconnect_at).is_some() {
            notes.push("最近发生过重连并已恢复".to_owned());
            *overall = (*overall).max(PingSeverity::Warning);
        } else {
            notes.push("最近重连尚未发现恢复记录".to_owned());
            *overall = (*overall).max(PingSeverity::Error);
        }
    }
    if let Some(invalid) = snapshot.last_invalid_session.as_ref() {
        if session_recovered_at(snapshot, &invalid.at).is_some() {
            notes.push("最近 invalid session 已恢复".to_owned());
            *overall = (*overall).max(PingSeverity::Warning);
        } else {
            notes.push("invalid session 尚未恢复".to_owned());
            *overall = (*overall).max(PingSeverity::Error);
        }
    }
}

fn collect_token_note(
    token_snapshot: &AccessTokenSnapshot,
    notes: &mut Vec<String>,
    overall: &mut PingSeverity,
) {
    if matches!(token_snapshot.state, AccessTokenSnapshotState::RefreshDue) {
        notes.push("访问令牌即将刷新".to_owned());
        *overall = (*overall).max(PingSeverity::Warning);
    }
}

fn collect_attempt_note(
    attempt: Option<AttemptStatus<'_>>,
    had_failure: bool,
    failure_note: &str,
    recovered_note: &str,
    failure_severity: PingSeverity,
    notes: &mut Vec<String>,
    overall: &mut PingSeverity,
) {
    match attempt {
        Some(AttemptStatus::Failure { .. }) => {
            notes.push(failure_note.to_owned());
            *overall = (*overall).max(failure_severity);
        }
        Some(AttemptStatus::Success { .. }) if had_failure => {
            notes.push(recovered_note.to_owned());
            *overall = (*overall).max(PingSeverity::Warning);
        }
        _ => {}
    }
}

fn summary_text(overall: PingSeverity, notes: &[String]) -> String {
    match overall {
        PingSeverity::Normal => {
            "Gateway、QQ WebSocket、LLM 服务和上游模型均正常，未发现未恢复异常。".to_owned()
        }
        PingSeverity::Warning => {
            // 顶部异常摘要需呈现同一严重度下的全部 note，避免漏报并发的 LLM 降级、
            // Gateway 重连/invalid session、发送失败等条目（见 Issue #68）。
            // 使用中文分号内联拼接，避免在 Markdown 引用块内产生多段 `>`，
            // 以保证现有 `/ping` 回复结构对 QQ 富文本渲染的兼容性。
            let detail = join_summary_notes(notes, "存在需要关注的状态");
            format!("服务当前可用，但需要关注：{detail}。")
        }
        PingSeverity::Error => {
            let detail = join_summary_notes(notes, "存在影响服务的异常");
            format!("检测到影响服务的异常：{detail}。")
        }
    }
}

/// 将多条 note 拼接为顶部摘要正文。
///
/// 约束：摘要行嵌入在 Markdown 引用块 `> {summary}` 中，因此只使用内联分隔符
/// 「；」，不引入换行或多段 `>`。条目顺序沿用 notes 原序。空列表时回退到
/// `fallback` 文案，保持单 note 场景与历史行为一致。
fn join_summary_notes(notes: &[String], fallback: &str) -> String {
    if notes.is_empty() {
        return fallback.to_owned();
    }
    notes.join("；")
}

fn recent_events(
    snapshot: &GatewayRuntimeSnapshot,
    llm_health: &LlmHealthSnapshot,
    now_seconds: i64,
) -> Vec<String> {
    let mut events = Vec::new();
    if let Some(state_error) = snapshot.state_error.as_deref() {
        events.push(format!("状态读取失败：{state_error}"));
    }
    if !llm_health_ok(llm_health) {
        events.push(format!("LLM healthz 异常：{}", llm_health.status));
    }
    match &llm_health.upstream {
        LlmUpstreamSnapshot::Unverified if llm_health_ok(llm_health) => {
            events.push("LLM 上游尚未验证，可发送 `/ping check` 主动验证".to_owned());
        }
        LlmUpstreamSnapshot::Available {
            provider,
            model,
            fallback_used: true,
            ..
        } => events.push(format!(
            "LLM 上游最近成功，但发生模型降级：{}/{}",
            provider.as_deref().unwrap_or("unknown"),
            model.as_deref().unwrap_or("unknown")
        )),
        LlmUpstreamSnapshot::Error { error_summary, .. } => {
            events.push(format!("LLM 上游异常：{error_summary}"));
        }
        _ => {}
    }
    if let Some(reconnect_at) = snapshot.last_reconnect_at.as_deref() {
        events.push(format!(
            "`{}` QQ WebSocket 断线重连（{}）",
            format_diagnostic_clock_time_for_display(reconnect_at),
            time_ago(reconnect_at, now_seconds)
        ));
        if let Some(recovered_at) = reconnect_recovered_at(snapshot, reconnect_at) {
            let elapsed = recovery_elapsed_text(reconnect_at, recovered_at, "后", "");
            events.push(format!(
                "`{}` Session 恢复成功{}",
                format_diagnostic_clock_time_for_display(recovered_at),
                elapsed
            ));
        } else {
            events.push("QQ WebSocket 重连后尚未发现 READY 或 RESUMED".to_owned());
        }
    }
    if let Some(invalid) = snapshot.last_invalid_session.as_ref() {
        let resume_text = if invalid.can_resume {
            "can_resume=true"
        } else {
            "can_resume=false"
        };
        if session_recovered_at(snapshot, &invalid.at).is_some() {
            events.push(format!(
                "`{}` invalid session 已恢复（{}）",
                format_diagnostic_clock_time_for_display(&invalid.at),
                resume_text
            ));
        } else {
            events.push(format!(
                "`{}` invalid session 尚未恢复（{}）",
                format_diagnostic_clock_time_for_display(&invalid.at),
                resume_text
            ));
        }
    }
    append_attempt_event(
        &mut events,
        "QQ 发送",
        snapshot.last_qq_send_success_at.as_deref(),
        snapshot.last_qq_send_failure_at.as_deref(),
        snapshot.last_qq_send_failure_summary.as_deref(),
        now_seconds,
    );
    append_attempt_event(
        &mut events,
        "LLM respond",
        snapshot.last_respond_success_at.as_deref(),
        snapshot.last_respond_failure_at.as_deref(),
        snapshot.last_respond_failure_summary.as_deref(),
        now_seconds,
    );
    if llm_health_ok(llm_health)
        && snapshot.last_qq_send_failure_at.is_none()
        && snapshot.last_respond_failure_at.is_none()
        && snapshot.last_invalid_session.is_none()
    {
        events.push("未发现发送、LLM 或 Session 异常".to_owned());
    }
    if events.is_empty() {
        events.push("未发现需要关注的事件".to_owned());
    }
    events
}

fn append_attempt_event(
    events: &mut Vec<String>,
    label: &str,
    success_at: Option<&str>,
    failure_at: Option<&str>,
    failure_summary: Option<&str>,
    now_seconds: i64,
) {
    match latest_attempt(success_at, failure_at) {
        Some(AttemptStatus::Failure { at }) => {
            let summary = failure_summary
                .filter(|value| !value.trim().is_empty())
                .map(|value| format!("：{value}"))
                .unwrap_or_default();
            events.push(format!(
                "{}失败于 {}{}",
                label,
                time_ago(at, now_seconds),
                summary
            ));
        }
        Some(AttemptStatus::Success { at }) if failure_at.is_some() => {
            events.push(format!(
                "{}曾失败，最近一次尝试成功于 {}",
                label,
                time_ago(at, now_seconds)
            ));
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptStatus<'a> {
    Success { at: &'a str },
    Failure { at: &'a str },
}

fn latest_attempt<'a>(
    success_at: Option<&'a str>,
    failure_at: Option<&'a str>,
) -> Option<AttemptStatus<'a>> {
    match (success_at, failure_at) {
        (Some(success), Some(failure)) => {
            let success_seconds = diagnostic_time_unix_seconds(success);
            let failure_seconds = diagnostic_time_unix_seconds(failure);
            if failure_seconds >= success_seconds {
                Some(AttemptStatus::Failure { at: failure })
            } else {
                Some(AttemptStatus::Success { at: success })
            }
        }
        (Some(success), None) => Some(AttemptStatus::Success { at: success }),
        (None, Some(failure)) => Some(AttemptStatus::Failure { at: failure }),
        (None, None) => None,
    }
}

fn reconnect_recovered_at<'a>(
    snapshot: &'a GatewayRuntimeSnapshot,
    reconnect_at: &str,
) -> Option<&'a str> {
    latest_after(
        [
            snapshot.last_resumed_at.as_deref(),
            snapshot.last_ready_at.as_deref(),
        ],
        reconnect_at,
    )
}

fn session_recovered_at<'a>(
    snapshot: &'a GatewayRuntimeSnapshot,
    invalid_at: &str,
) -> Option<&'a str> {
    latest_after(
        [
            snapshot.last_resumed_at.as_deref(),
            snapshot.last_ready_at.as_deref(),
        ],
        invalid_at,
    )
}

fn latest_after<'a, const N: usize>(values: [Option<&'a str>; N], base: &str) -> Option<&'a str> {
    let base_seconds = diagnostic_time_unix_seconds(base)?;
    values
        .into_iter()
        .flatten()
        .filter(|value| {
            diagnostic_time_unix_seconds(value).is_some_and(|seconds| seconds >= base_seconds)
        })
        .max_by_key(|value| diagnostic_time_unix_seconds(value).unwrap_or(i64::MIN))
}

fn recovery_elapsed_text(start: &str, recovered_at: &str, suffix: &str, fallback: &str) -> String {
    format_diagnostic_elapsed_between_for_display(start, recovered_at)
        .map(|elapsed| format!("{elapsed}{suffix}"))
        .unwrap_or_else(|| fallback.to_owned())
}

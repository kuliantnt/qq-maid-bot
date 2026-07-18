use qq_maid_common::time_context::{
    format_diagnostic_time_for_display, now_diagnostic_time_for_display, now_unix_seconds,
};

use crate::{
    auth::{AccessTokenSnapshot, AccessTokenSnapshotState},
    config::{AppConfig, OneBot11Config, WechatServiceConfig},
    gateway::{
        command::{GatewayCommandContext, GatewayCommandConversation},
        logging::{mask_identifier, mask_scope_key},
    },
};

use super::{
    PingMode,
    assess::{PingSeverity, assess_ping_status},
    healthz::{LlmHealthSnapshot, LlmUpstreamSnapshot},
    status::{GatewayRuntimeSnapshot, GatewayRuntimeStatus},
    time::{diagnostic_time_option_text, time_or_placeholder},
};

pub(super) fn render_ping_reply(
    command_text: &str,
    context: &GatewayCommandContext,
    config: &AppConfig,
    runtime: &GatewayRuntimeStatus,
    token_snapshot: &AccessTokenSnapshot,
    llm_health: &LlmHealthSnapshot,
) -> String {
    let mode = super::parse_ping_mode(command_text).unwrap_or(PingMode::Summary);
    render_ping_reply_at(
        context,
        config,
        runtime,
        token_snapshot,
        llm_health,
        mode,
        now_unix_seconds(),
    )
}

pub(super) fn render_ping_reply_at(
    context: &GatewayCommandContext,
    config: &AppConfig,
    runtime: &GatewayRuntimeStatus,
    token_snapshot: &AccessTokenSnapshot,
    llm_health: &LlmHealthSnapshot,
    mode: PingMode,
    now_seconds: i64,
) -> String {
    let snapshot = runtime.snapshot();
    let current_scope = context.scope_key().unwrap_or_else(|| "unknown".to_owned());
    // 私聊 `/ping` 直接回显当前用户自己的稳定 ID，便于配置本地运维白名单；
    // 消息 ID、scope、URL、Unix 秒等其他诊断细节仍只在 `/ping all` 脱敏展示。
    let assessment =
        assess_ping_status(&snapshot, runtime, token_snapshot, llm_health, now_seconds);
    let title = match assessment.overall {
        PingSeverity::Normal => "# 🟢 服务运行正常",
        PingSeverity::Warning => "# 🟡 服务可用，但存在警告",
        PingSeverity::Error => "# 🔴 服务异常",
    };

    let mut lines = vec![
        title.to_owned(),
        String::new(),
        format!("> {}", assessment.summary),
        String::new(),
        "## 核心链路".to_owned(),
        "| 模块 | 状态 | 详情 |".to_owned(),
        "|---|---|---|".to_owned(),
    ];
    for row in &assessment.rows {
        lines.push(format!(
            "| {} | {} | {} |",
            markdown_cell(&row.module),
            markdown_cell(&row.status),
            markdown_cell(&row.detail)
        ));
    }

    lines.extend([String::new(), "## 最近事件".to_owned()]);
    for event in &assessment.events {
        lines.push(format!("- {event}"));
    }

    lines.extend([
        String::new(),
        "## 当前消息".to_owned(),
        "| 项目 | 内容 |".to_owned(),
        "|---|---|".to_owned(),
        format!("| 平台 | {} |", markdown_cell(context.platform_name)),
        format!("| 场景 | {} |", context.conversation.label()),
        format!("| 事件 | {} |", markdown_cell(context.event_name)),
    ]);
    if let Some(user_id) = context.user_id.as_deref() {
        let visible_user_id = if matches!(
            context.conversation,
            GatewayCommandConversation::Private | GatewayCommandConversation::ServiceAccount
        ) {
            user_id.to_owned()
        } else {
            mask_identifier(user_id)
        };
        lines.push(format!("| user_id | {} |", markdown_cell(&visible_user_id)));
    }
    if let Some(group_id) = context.group_id.as_deref() {
        lines.push(format!(
            "| group_id | {} |",
            markdown_cell(&mask_identifier(group_id))
        ));
    }
    lines.extend([
        format!("| 附件 | {} |", context.attachment_count),
        format!(
            "| 接收时间 | {} |",
            markdown_cell(&time_or_placeholder(context.timestamp.as_deref()))
        ),
    ]);

    if matches!(mode, PingMode::All) {
        lines.extend([String::new(), "## 调试详情".to_owned()]);
        lines.extend(render_ping_debug_details(
            context,
            config,
            runtime,
            token_snapshot,
            llm_health,
            &snapshot,
            &current_scope,
        ));
    }

    lines.join("\n")
}

fn render_ping_debug_details(
    context: &GatewayCommandContext,
    config: &AppConfig,
    runtime: &GatewayRuntimeStatus,
    token_snapshot: &AccessTokenSnapshot,
    llm_health: &LlmHealthSnapshot,
    snapshot: &GatewayRuntimeSnapshot,
    current_scope: &str,
) -> Vec<String> {
    let mut lines = render_debug_overview(runtime, llm_health, snapshot);
    lines.push(String::new());
    lines.extend(render_debug_gateway(runtime, snapshot));
    lines.push(String::new());
    lines.extend(render_debug_message(context, snapshot, current_scope));
    lines.push(String::new());
    lines.extend(render_debug_send(snapshot));
    lines.push(String::new());
    lines.extend(render_debug_llm(config, llm_health, snapshot));
    if !matches!(context.conversation, GatewayCommandConversation::Group) {
        lines.push(String::new());
        lines.extend(render_debug_config(config, token_snapshot));
        lines.push(String::new());
        lines.extend(render_debug_wechat_service(&config.wechat_service));
        lines.push(String::new());
        lines.extend(render_debug_onebot11(&config.onebot11, snapshot));
    }
    lines
}

fn render_debug_overview(
    runtime: &GatewayRuntimeStatus,
    llm_health: &LlmHealthSnapshot,
    snapshot: &GatewayRuntimeSnapshot,
) -> Vec<String> {
    vec![
        "### 概览".to_owned(),
        format!(
            "- Gateway：{}",
            runtime_status_text(snapshot.state_error.as_deref())
        ),
        format!("- LLM healthz：{}", llm_health.status),
        format!("- 当前时间：{}", now_diagnostic_time_for_display()),
        format!("- pid：{}", runtime.pid),
        format!("- 运行时长：{}", runtime.uptime_text()),
    ]
}

fn render_debug_gateway(
    runtime: &GatewayRuntimeStatus,
    snapshot: &GatewayRuntimeSnapshot,
) -> Vec<String> {
    let invalid_session = snapshot
        .last_invalid_session
        .as_ref()
        .map(|item| {
            format!(
                "{} can_resume={}",
                format_diagnostic_time_for_display(&item.at),
                bool_text(item.can_resume)
            )
        })
        .unwrap_or_else(|| "无".to_owned());
    let state_error = snapshot.state_error.as_deref().unwrap_or("无");

    vec![
        "### Gateway".to_owned(),
        format!("- instance：{}", runtime.instance_id),
        format!(
            "- started_at：{}",
            format_diagnostic_time_for_display(&runtime.started_at)
        ),
        format!(
            "- websocket connected：{}",
            diagnostic_time_option_text(snapshot.last_gateway_connected_at.as_deref())
        ),
        format!(
            "- READY：{}",
            diagnostic_time_option_text(snapshot.last_ready_at.as_deref())
        ),
        format!(
            "- RESUMED：{}",
            diagnostic_time_option_text(snapshot.last_resumed_at.as_deref())
        ),
        format!(
            "- heartbeat ack：{}",
            diagnostic_time_option_text(snapshot.last_heartbeat_ack_at.as_deref())
        ),
        format!(
            "- reconnect：{}",
            diagnostic_time_option_text(snapshot.last_reconnect_at.as_deref())
        ),
        format!("- invalid session：{invalid_session}"),
        format!("- 状态读取错误：{state_error}"),
    ]
}

fn render_debug_message(
    context: &GatewayCommandContext,
    snapshot: &GatewayRuntimeSnapshot,
    current_scope: &str,
) -> Vec<String> {
    vec![
        "### 消息".to_owned(),
        format!("- 平台：{}", context.platform_code),
        format!("- 事件类型：{}", context.event_name),
        format!("- 会话类型：{}", context.conversation.label()),
        format!(
            "- 当前消息 id：{}",
            context
                .message_id
                .as_deref()
                .map(mask_identifier)
                .unwrap_or_else(|| "无".to_owned())
        ),
        format!(
            "- 当前用户 user_id：{}",
            context
                .user_id
                .as_deref()
                .map(|user_id| {
                    if matches!(context.conversation, GatewayCommandConversation::Group) {
                        mask_identifier(user_id)
                    } else {
                        user_id.to_owned()
                    }
                })
                .unwrap_or_else(|| "无".to_owned())
        ),
        format!(
            "- 当前群 group_id：{}",
            context
                .group_id
                .as_deref()
                .map(mask_identifier)
                .unwrap_or_else(|| "无".to_owned())
        ),
        format!("- 当前 scope_key：{}", mask_scope_key(current_scope)),
        format!(
            "- 当前消息时间：{}",
            diagnostic_time_option_text(context.timestamp.as_deref())
        ),
        format!(
            "- 最近收到：{}",
            diagnostic_time_option_text(snapshot.last_c2c_received_at.as_deref())
        ),
        format!(
            "- 最近消息 id：{}",
            option_text(snapshot.last_c2c_message_id.as_deref())
        ),
        format!("- 附件数量：{}", context.attachment_count),
    ]
}

fn render_debug_send(snapshot: &GatewayRuntimeSnapshot) -> Vec<String> {
    vec![
        "### 发送".to_owned(),
        format!(
            "- 最近 QQ 发送成功：{}",
            diagnostic_time_option_text(snapshot.last_qq_send_success_at.as_deref())
        ),
        format!(
            "- 最近 QQ 发送失败：{}",
            diagnostic_time_option_text(snapshot.last_qq_send_failure_at.as_deref())
        ),
        format!(
            "- 失败摘要：{}",
            option_text(snapshot.last_qq_send_failure_summary.as_deref())
        ),
    ]
}

fn render_debug_llm(
    _config: &AppConfig,
    llm_health: &LlmHealthSnapshot,
    snapshot: &GatewayRuntimeSnapshot,
) -> Vec<String> {
    vec![
        "### LLM".to_owned(),
        format!("- core：{}", llm_health.healthz_url),
        format!("- health：{}", llm_health.status),
        format!("- 上游状态：{}", upstream_debug_text(&llm_health.upstream)),
        format!(
            "- 最近 respond 成功：{}",
            diagnostic_time_option_text(snapshot.last_respond_success_at.as_deref())
        ),
        format!(
            "- 最近 respond 失败：{}",
            diagnostic_time_option_text(snapshot.last_respond_failure_at.as_deref())
        ),
        format!(
            "- 失败摘要：{}",
            option_text(snapshot.last_respond_failure_summary.as_deref())
        ),
    ]
}

fn upstream_debug_text(upstream: &LlmUpstreamSnapshot) -> String {
    match upstream {
        LlmUpstreamSnapshot::Unavailable => "unavailable".to_owned(),
        LlmUpstreamSnapshot::Unverified => "unverified".to_owned(),
        LlmUpstreamSnapshot::Available {
            last_success_at,
            provider,
            model,
            fallback_used,
        } => format!(
            "available, last_success={}, provider={}, model={}, fallback={}",
            diagnostic_time_option_text(last_success_at.as_deref()),
            option_text(provider.as_deref()),
            option_text(model.as_deref()),
            fallback_used
        ),
        LlmUpstreamSnapshot::Error {
            last_checked_at,
            error_summary,
        } => format!(
            "error, last_checked={}, summary={error_summary}",
            diagnostic_time_option_text(last_checked_at.as_deref())
        ),
    }
}

fn render_debug_config(config: &AppConfig, token_snapshot: &AccessTokenSnapshot) -> Vec<String> {
    vec![
        "### 配置".to_owned(),
        format!("- sandbox：{}", bool_text(config.sandbox)),
        format!("- api_base：{}", url_host_path(&config.api_base)),
        format!("- Markdown：{}", bool_text(config.enable_markdown)),
        format!("- Image：{}", bool_text(config.enable_image)),
        format!("- verbose_log：{}", bool_text(config.verbose_log)),
        format!("- 访问令牌缓存：{}", token_snapshot_text(token_snapshot)),
        format!(
            "- refresh margin：{}s",
            token_snapshot.refresh_margin_seconds
        ),
    ]
}

fn render_debug_wechat_service(config: &WechatServiceConfig) -> Vec<String> {
    vec![
        "### 微信服务号".to_owned(),
        format!("- 入口：{}", bool_text(config.enabled)),
        format!("- 监听：{}:{}", config.bind_host, config.bind_port),
        format!("- callback path：{}", config.callback_path),
        format!("- token：{}", secret_state_text(config.token.as_deref())),
        format!("- app_id：{}", secret_state_text(config.app_id.as_deref())),
        format!(
            "- app_secret：{}",
            secret_state_text(config.app_secret.as_deref())
        ),
        format!("- 消息加解密：{}", config.encryption_mode.as_str()),
        format!(
            "- encoding_aes_key：{}",
            secret_state_text(config.encoding_aes_key.as_deref())
        ),
        format!("- access_token：{}", wechat_access_token_text(config)),
        format!("- 同步回复预算：{}ms", config.reply_timeout.as_millis()),
        format!("- 客服消息：{}", wechat_customer_message_text(config)),
        "- 支持消息模式：明文或 AES 安全模式 text-only，同步 XML 文本回复；慢请求可用客服文本消息异步补发；Markdown 会降级为 text"
            .to_owned(),
        "- 暂不支持：兼容模式、模板消息、图片/语音/视频、菜单事件、主动推送、流式输出"
            .to_owned(),
    ]
}

fn render_debug_onebot11(
    config: &OneBot11Config,
    snapshot: &GatewayRuntimeSnapshot,
) -> Vec<String> {
    vec![
        "### OneBot 11".to_owned(),
        format!("- 入口：{}", bool_text(config.enabled)),
        format!("- 监听：{}", bool_text(snapshot.onebot_listening)),
        format!("- 连接：{}", bool_text(snapshot.onebot_connected)),
        format!("- bind：{}:{}", config.bind_host, config.bind_port),
        format!("- websocket path：{}", config.websocket_path),
        format!(
            "- access token：{}",
            secret_state_text(config.access_token.as_deref())
        ),
        format!(
            "- self_id：{}",
            option_text(snapshot.onebot_self_id_summary.as_deref())
        ),
        format!(
            "- 最近心跳：{}",
            diagnostic_time_option_text(snapshot.last_onebot_heartbeat_at.as_deref())
        ),
        format!(
            "- 最近断开：{}",
            diagnostic_time_option_text(snapshot.last_onebot_disconnected_at.as_deref())
        ),
        format!(
            "- 断开摘要：{}",
            option_text(snapshot.last_onebot_disconnect_summary.as_deref())
        ),
        format!(
            "- request timeout：{}ms",
            config.request_timeout.as_millis()
        ),
        format!("- max message bytes：{}", config.max_message_bytes),
        "- 入站：私聊、群聊 @ / 进程内机器人消息引用，以及按序文本、图片、文件和未知消息段；安全远程图片可进入图片理解，文件和不可读媒体仅生成摘要"
            .to_owned(),
        "- 出站：私聊、群聊和 Todo / RSS 等主动推送仅发送纯文本；不支持图片、文件、Markdown、平台原生引用、@ 或流式输出"
            .to_owned(),
    ]
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn runtime_status_text(state_error: Option<&str>) -> &'static str {
    if state_error.is_some() { "ERROR" } else { "OK" }
}

fn token_snapshot_text(snapshot: &AccessTokenSnapshot) -> String {
    let state = match snapshot.state {
        AccessTokenSnapshotState::Empty => "empty",
        AccessTokenSnapshotState::Cached => "cached",
        AccessTokenSnapshotState::RefreshDue => "refresh_due",
    };
    match snapshot.expires_in_seconds {
        Some(seconds) => format!("{state}, expires_in={seconds}s"),
        None => state.to_owned(),
    }
}

fn url_host_path(url: &str) -> String {
    match reqwest::Url::parse(url.trim()) {
        Ok(parsed) => {
            let host = parsed.host_str().unwrap_or("unknown-host");
            let port = parsed
                .port()
                .map(|port| format!(":{port}"))
                .unwrap_or_default();
            format!("{host}{port}{}", parsed.path())
        }
        Err(_) => "invalid url".to_owned(),
    }
}

fn option_text(value: Option<&str>) -> &str {
    value.filter(|text| !text.trim().is_empty()).unwrap_or("无")
}

fn bool_text(value: bool) -> &'static str {
    if value { "enabled" } else { "disabled" }
}

fn secret_state_text(value: Option<&str>) -> &'static str {
    match value {
        Some(text) if !text.trim().is_empty() => "configured",
        _ => "missing",
    }
}

fn wechat_access_token_text(config: &WechatServiceConfig) -> &'static str {
    if config.app_id.is_some() && config.app_secret.is_some() {
        "on_demand（仅客服消息补发时获取，不在诊断中展示）"
    } else {
        "not_configured"
    }
}

fn wechat_customer_message_text(config: &WechatServiceConfig) -> &'static str {
    if config.app_id.is_some() && config.app_secret.is_some() {
        "configured（仅 text）"
    } else {
        "missing_credentials"
    }
}

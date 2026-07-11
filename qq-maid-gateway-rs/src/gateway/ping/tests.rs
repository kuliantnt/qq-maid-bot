use std::time::Duration;

use crate::{
    auth::{AccessTokenManager, AccessTokenSnapshot, AccessTokenSnapshotState},
    config::{AgentTypingConfig, AppConfig, WechatServiceConfig},
    gateway::event::C2cMessage,
};
use qq_maid_core::service::{CoreHealthSnapshot, UpstreamStatusSnapshot};

use super::{
    GatewayRuntimeStatus, PingMode, build_c2c_ping_reply_with_check_failure,
    healthz::{LlmHealthSnapshot, LlmUpstreamSnapshot},
    is_ping_check_command, is_ping_command,
    render::render_c2c_ping_reply_at,
};

fn config() -> AppConfig {
    AppConfig {
        app_id: "appid".to_owned(),
        app_secret: "app-secret-value".to_owned(),
        bot_mention_ids: Vec::new(),
        sandbox: false,
        api_base: "https://api.sgroup.qq.com".to_owned(),
        token_refresh_margin: Duration::from_secs(60),
        enable_markdown: false,
        enable_image: true,
        enable_group_messages: false,
        verbose_log: false,
        member_detail_enrich_enabled: false,
        group_message_mode: crate::config::GroupMessageMode::Off,
        group_active_keywords: vec!["小女仆".to_owned()],
        conversation_queue_capacity: 16,
        max_active_conversation_workers: 64,
        conversation_worker_idle_timeout: Duration::from_secs(300),
        message_aggregation: crate::config::MessageAggregationConfig {
            private_enabled: true,
            group_enabled: false,
            quiet: Duration::from_millis(1200),
            max_wait: Duration::from_millis(3000),
            max_messages: 10,
            max_chars: 12000,
            max_active_keys: 1024,
        },
        c2c_final_reply_stream_enabled: false,
        c2c_visible_progress_status_enabled: true,
        agent_typing: AgentTypingConfig {
            enabled: false,
            delay: Duration::from_secs(1),
        },
        markdown_chunk_soft_limit: 1800,
        text_chunk_soft_limit: 1800,
        media_dir: std::path::PathBuf::from("media/inbound"),
        media_download_timeout: Duration::from_secs(10),
        media_max_bytes: crate::config::DEFAULT_MEDIA_MAX_BYTES,
        wechat_service: crate::config::WechatServiceConfig::default(),
    }
}

fn core_health() -> CoreHealthSnapshot {
    CoreHealthSnapshot {
        ok: true,
        provider: "mock".to_owned(),
        model: "mock-model".to_owned(),
        stream: false,
        upstream: UpstreamStatusSnapshot::default(),
    }
}

fn message() -> C2cMessage {
    C2cMessage {
        message_id: "msg-sensitive-123456".to_owned(),
        current_msg_idx: None,
        event_id: Some("event-sensitive-123456".to_owned()),
        source_message_ids: vec!["msg-sensitive-123456".to_owned()],
        source_event_ids: vec!["event-sensitive-123456".to_owned()],
        user_openid: "user-openid-123456".to_owned(),
        content: "/ping".to_owned(),
        reply: None,
        timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
        first_message_timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
        last_message_timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
        input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("/ping")],
        attachments: Vec::new(),
    }
}

fn message_with_content(content: &str) -> C2cMessage {
    C2cMessage {
        content: content.to_owned(),
        ..message()
    }
}

fn token_snapshot() -> AccessTokenSnapshot {
    AccessTokenSnapshot {
        state: AccessTokenSnapshotState::Cached,
        expires_in_seconds: Some(120),
        refresh_margin_seconds: 60,
    }
}

fn health(status: &str, upstream: LlmUpstreamSnapshot) -> LlmHealthSnapshot {
    LlmHealthSnapshot {
        healthz_url: "in-process".to_owned(),
        status: status.to_owned(),
        upstream,
    }
}

#[test]
fn detects_ping_command_case_insensitively() {
    assert!(is_ping_command(" /PING "));
    assert!(is_ping_command(" /ping all "));
    assert!(is_ping_command(" /PING ALL "));
    assert!(is_ping_command(" /ping check "));
    assert!(is_ping_check_command(" /PING CHECK "));
    assert!(!is_ping_check_command(" /ping "));
    assert!(!is_ping_command("/ping now"));
    assert!(!is_ping_command("/ping all extra"));
}

#[test]
fn runtime_records_recent_events_without_full_message_id() {
    let runtime = GatewayRuntimeStatus::new_for_test();
    let message = message();

    runtime.record_gateway_connected();
    runtime.record_ready();
    runtime.record_resumed();
    runtime.record_heartbeat_ack();
    runtime.record_reconnect();
    runtime.record_invalid_session(false);
    runtime.record_c2c_message_received(&message);
    runtime.record_respond_success();
    runtime.record_respond_failure("http status 500\nwith details");
    runtime.record_qq_send_success();
    runtime.record_qq_send_failure("request failed timeout with a long but safe summary");

    let snapshot = runtime.snapshot();

    assert!(!snapshot.qq_connected);
    assert!(snapshot.last_gateway_connected_at.is_some());
    assert!(snapshot.last_ready_at.is_some());
    assert!(snapshot.last_resumed_at.is_some());
    assert!(snapshot.last_heartbeat_ack_at.is_some());
    assert!(snapshot.last_reconnect_at.is_some());
    assert_eq!(
        snapshot
            .last_invalid_session
            .as_ref()
            .map(|item| item.can_resume),
        Some(false)
    );
    assert_eq!(
        snapshot.last_c2c_message_id.as_deref(),
        Some("******123456")
    );
    assert!(snapshot.last_respond_success_at.is_some());
    assert_eq!(
        snapshot.last_respond_failure_summary.as_deref(),
        Some("http status 500 with details")
    );
    assert!(snapshot.last_qq_send_success_at.is_some());
    assert!(snapshot.last_qq_send_failure_at.is_some());

    runtime.record_gateway_connected();
    runtime.record_wechat_service_listening();
    let snapshot = runtime.snapshot();
    assert!(snapshot.qq_connected);
    assert!(snapshot.wechat_service_listening);

    runtime.record_gateway_disconnected();
    runtime.record_wechat_service_stopped();
    let snapshot = runtime.snapshot();
    assert!(!snapshot.qq_connected);
    assert!(!snapshot.wechat_service_listening);
}

#[test]
fn renders_summary_ping_reply_without_debug_noise_or_secrets() {
    let runtime = GatewayRuntimeStatus::new_for_test();
    runtime.update_state(|state| {
        state.last_gateway_connected_at = Some("unix:1000".to_owned());
        state.last_ready_at = Some("unix:1001".to_owned());
        state.last_heartbeat_ack_at = Some("unix:1190".to_owned());
        state.last_c2c_received_at = Some("unix:1200".to_owned());
        state.last_qq_send_success_at = Some("unix:1180".to_owned());
        state.last_respond_success_at = Some("unix:1170".to_owned());
    });

    let reply = render_c2c_ping_reply_at(
        &message(),
        &config(),
        &runtime,
        &token_snapshot(),
        &health(
            "ok(in-process)",
            LlmUpstreamSnapshot::Available {
                last_success_at: Some("unix:1175".to_owned()),
                provider: Some("openai".to_owned()),
                model: Some("gpt-test".to_owned()),
                fallback_used: false,
            },
        ),
        PingMode::Summary,
        1200,
    );

    assert!(reply.contains("# 🟢 服务运行正常"));
    assert!(reply.contains("## 核心链路"));
    assert!(reply.contains("| Gateway | 🟢 正常 | 已运行 "));
    assert!(reply.contains("| QQ 连接 | 🟢 已连接 | WebSocket 已连接于 3分钟20秒前 |"));
    assert!(reply.contains("| 心跳 | 🟢 正常 | 10秒前收到 ACK |"));
    assert!(reply.contains("| LLM 服务 | 🟢 在线 | in-process ok |"));
    assert!(reply.contains("| LLM 上游 | 🟢 可用 | 最近成功于 25秒前；使用 openai/gpt-test |"));
    assert!(reply.contains("| 消息接收 | 🟢 正常 | 刚刚收到当前消息 |"));
    assert!(reply.contains("| 消息发送 | 🟢 正常 | 最近一次发送尝试成功于 20秒前 |"));
    assert!(reply.contains("- 未发现发送、LLM 或 Session 异常"));
    assert!(reply.contains("| 接收时间 | 2026-06-10 12:00:00 +08:00 |"));
    assert!(!reply.contains("## 调试详情"));
    assert!(!reply.contains("### 微信服务号"));
    assert!(!reply.contains("pid："));
    assert!(!reply.contains("instance："));
    assert!(!reply.contains("当前用户："));
    assert!(!reply.contains("scope_key"));
    assert!(!reply.contains("unix:"));
    assert!(!reply.contains("user-openid-123456"));
    assert!(!reply.contains("msg-sensitive-123456"));
    assert!(!reply.contains("real-token"));
    assert!(!reply.contains("app-secret-value"));
    assert!(!reply.contains("Authorization"));
}

#[test]
fn renders_ping_all_with_debug_details_without_secrets() {
    let runtime = GatewayRuntimeStatus::new_for_test();
    runtime.update_state(|state| {
        state.last_gateway_connected_at = Some("unix:1000".to_owned());
        state.last_ready_at = Some("unix:1001".to_owned());
        state.last_resumed_at = Some("unix:1105".to_owned());
        state.last_heartbeat_ack_at = Some("unix:1190".to_owned());
        state.last_reconnect_at = Some("unix:1100".to_owned());
        state.last_c2c_received_at = Some("unix:1200".to_owned());
        state.last_c2c_message_id = Some("******123456".to_owned());
        state.last_respond_failure_at = Some("unix:1140".to_owned());
        state.last_respond_failure_summary = Some("connect failed".to_owned());
        state.last_respond_success_at = Some("unix:1170".to_owned());
        state.last_qq_send_failure_at = Some("unix:1130".to_owned());
        state.last_qq_send_failure_summary = Some("http status 429".to_owned());
        state.last_qq_send_success_at = Some("unix:1180".to_owned());
    });

    let reply = render_c2c_ping_reply_at(
        &message_with_content("/ping all"),
        &config(),
        &runtime,
        &token_snapshot(),
        &health(
            "ok(in-process)",
            LlmUpstreamSnapshot::Available {
                last_success_at: Some("unix:1175".to_owned()),
                provider: Some("openai".to_owned()),
                model: Some("gpt-test".to_owned()),
                fallback_used: false,
            },
        ),
        PingMode::All,
        1200,
    );

    assert!(reply.contains("# 🟡 服务可用，但存在警告"));
    assert!(reply.contains("5秒后恢复"));
    assert!(reply.contains("QQ 发送曾失败，最近一次尝试成功于 20秒前"));
    assert!(reply.contains("LLM respond曾失败，最近一次尝试成功于 30秒前"));
    assert!(reply.contains("## 调试详情"));
    assert!(reply.contains("### 概览"));
    assert!(reply.contains("### Gateway"));
    assert!(reply.contains("### 消息"));
    assert!(reply.contains("### 发送"));
    assert!(reply.contains("### LLM"));
    assert!(reply.contains("### 配置"));
    assert!(reply.contains("health：ok(in-process)"));
    assert!(reply.contains("当前时间："));
    assert!(reply.contains("+08:00 (unix:"));
    assert!(reply.contains("started_at：1970-01-01 08:00:01 +08:00 (unix:1)"));
    assert!(reply.contains("当前消息时间：2026-06-10 12:00:00 +08:00"));
    assert!(!reply.contains("当前消息时间：2026-06-10T12:00:00+08:00"));
    assert!(!reply.contains("最近收到：unix:"));
    assert!(!reply.contains("最近 respond 失败：unix:"));
    assert!(!reply.contains("最近 QQ 发送失败：unix:"));
    assert!(!reply.contains("respond query"));
    assert!(reply.contains("当前用户：******123456"));
    assert!(reply.contains("当前 scope_key：private:******123456"));
    assert!(reply.contains("访问令牌缓存：cached, expires_in=120s"));
    assert!(reply.contains("### 微信服务号"));
    assert!(reply.contains("- 入口：disabled"));
    assert!(reply.contains("- 监听：127.0.0.1:8788"));
    assert!(reply.contains("- callback path：/wechat/service"));
    assert!(reply.contains("- token：missing"));
    assert!(reply.contains("- app_id：missing"));
    assert!(reply.contains("- app_secret：missing"));
    assert!(reply.contains("- access_token：not_configured"));
    assert!(reply.contains("- 同步回复预算：4000ms"));
    assert!(reply.contains("- 客服消息：missing_credentials"));
    assert!(reply.contains("明文 text-only，同步 XML 文本回复"));
    assert!(reply.contains("慢请求可用客服文本消息异步补发"));
    assert!(reply.contains("加密 XML、模板消息、图片/语音/视频、菜单事件、主动推送、流式输出"));
    assert!(!reply.contains("记忆模块"));
    assert!(!reply.contains("存储"));
    assert!(!reply.contains("user-openid-123456"));
    assert!(!reply.contains("msg-sensitive-123456"));
    assert!(!reply.contains("real-token"));
    assert!(!reply.contains("app-secret-value"));
    assert!(!reply.contains("Authorization"));
}

#[test]
fn renders_ping_all_with_wechat_service_security_summary_without_secrets() {
    let mut config = config();
    config.wechat_service = WechatServiceConfig {
        enabled: true,
        token: Some("real-wechat-token".to_owned()),
        app_id: Some("wx-real-app-id".to_owned()),
        app_secret: Some("wx-real-app-secret".to_owned()),
        bind_host: "127.0.0.1".to_owned(),
        bind_port: 8788,
        callback_path: "/wechat/service".to_owned(),
        reply_timeout: std::time::Duration::from_millis(4000),
        api_base: "https://api.weixin.qq.com".to_owned(),
    };

    let reply = render_c2c_ping_reply_at(
        &message_with_content("/ping all"),
        &config,
        &GatewayRuntimeStatus::new_for_test(),
        &token_snapshot(),
        &health("ok(in-process)", LlmUpstreamSnapshot::Unverified),
        PingMode::All,
        1200,
    );

    assert!(reply.contains("### 微信服务号"));
    assert!(reply.contains("- 入口：enabled"));
    assert!(reply.contains("- 监听：127.0.0.1:8788"));
    assert!(reply.contains("- callback path：/wechat/service"));
    assert!(reply.contains("- token：configured"));
    assert!(reply.contains("- app_id：configured"));
    assert!(reply.contains("- app_secret：configured"));
    assert!(reply.contains("- access_token：on_demand（仅客服消息补发时获取，不在诊断中展示）"));
    assert!(reply.contains("- 客服消息：configured（仅 text）"));
    assert!(!reply.contains("real-wechat-token"));
    assert!(!reply.contains("wx-real-app-id"));
    assert!(!reply.contains("wx-real-app-secret"));
    assert!(!reply.contains("real-access-token"));
}

#[test]
fn renders_unverified_upstream_without_all_green() {
    let reply = render_c2c_ping_reply_at(
        &message(),
        &config(),
        &GatewayRuntimeStatus::new_for_test(),
        &token_snapshot(),
        &health("ok(in-process)", LlmUpstreamSnapshot::Unverified),
        PingMode::Summary,
        1200,
    );

    assert!(reply.contains("# 🟡 服务可用，但存在警告"));
    assert!(reply.contains("| LLM 服务 | 🟢 在线 | in-process ok |"));
    assert!(reply.contains("| LLM 上游 | 🟡 未验证 |"));
    assert!(reply.contains("/ping check"));
}

#[test]
fn renders_failed_upstream_with_defensively_redacted_summary() {
    let reply = render_c2c_ping_reply_at(
        &message(),
        &config(),
        &GatewayRuntimeStatus::new_for_test(),
        &token_snapshot(),
        &health(
            "ok(in-process)",
            LlmUpstreamSnapshot::Error {
                last_checked_at: Some("unix:1190".to_owned()),
                error_summary: super::healthz::safe_upstream_error_summary(Some(
                    "Authorization: Bearer sk-secret-token",
                )),
            },
        ),
        PingMode::Summary,
        1200,
    );

    assert!(reply.contains("# 🔴 服务异常"));
    assert!(
        reply
            .contains("| LLM 上游 | 🔴 异常 | 最近失败于 10秒前；上游调用失败（错误详情已隐藏） |")
    );
    assert!(!reply.contains("Authorization"));
    assert!(!reply.contains("Bearer"));
    assert!(!reply.contains("sk-secret"));
}

#[test]
fn renders_fallback_success_as_available_but_degraded() {
    let reply = render_c2c_ping_reply_at(
        &message(),
        &config(),
        &GatewayRuntimeStatus::new_for_test(),
        &token_snapshot(),
        &health(
            "ok(in-process)",
            LlmUpstreamSnapshot::Available {
                last_success_at: Some("unix:1195".to_owned()),
                provider: Some("deepseek".to_owned()),
                model: Some("deepseek-chat".to_owned()),
                fallback_used: true,
            },
        ),
        PingMode::Summary,
        1200,
    );

    assert!(reply.contains("# 🟡 服务可用，但存在警告"));
    assert!(reply.contains(
        "| LLM 上游 | 🟡 可用（已降级） | 最近成功于 5秒前；最终使用 deepseek/deepseek-chat |"
    ));
    assert!(reply.contains("发生模型降级"));
}

#[test]
fn renders_summary_with_both_llm_degradation_and_recovered_reconnect() {
    // Issue #68：LLM fallback 降级与 Gateway 重连已恢复同时出现时，顶部异常摘要
    // 必须同时呈现两条 note，不再只保留首条。
    let runtime = GatewayRuntimeStatus::new_for_test();
    runtime.update_state(|state| {
        state.last_gateway_connected_at = Some("unix:1000".to_owned());
        state.last_ready_at = Some("unix:1001".to_owned());
        state.last_heartbeat_ack_at = Some("unix:1190".to_owned());
        // 重连发生在 1100，随后 READY/RESUMED 在 1105 恢复，构成「已恢复」note。
        state.last_reconnect_at = Some("unix:1100".to_owned());
        state.last_resumed_at = Some("unix:1105".to_owned());
        state.last_c2c_received_at = Some("unix:1200".to_owned());
    });

    let reply = render_c2c_ping_reply_at(
        &message(),
        &config(),
        &runtime,
        &token_snapshot(),
        &health(
            "ok(in-process)",
            LlmUpstreamSnapshot::Available {
                last_success_at: Some("unix:1195".to_owned()),
                provider: Some("deepseek".to_owned()),
                model: Some("deepseek-chat".to_owned()),
                fallback_used: true,
            },
        ),
        PingMode::Summary,
        1200,
    );

    assert!(reply.contains("# 🟡 服务可用，但存在警告"));
    // 顶部引用摘要行同时包含 LLM 降级与重连恢复两条信息。
    assert!(reply.contains("LLM 上游最近一次调用发生过降级"));
    assert!(reply.contains("最近发生过重连并已恢复"));
    // 两条 note 必须出现在同一行（引用块内），不引入多段 `>`。
    assert!(!reply.contains("\n> 最近发生过重连并已恢复"));
}

#[test]
fn renders_summary_with_llm_degradation_and_unrecovered_reconnect() {
    // LLM fallback 降级 + 重连尚未恢复：overall 应为 Error，且摘要同时体现两条。
    let runtime = GatewayRuntimeStatus::new_for_test();
    runtime.update_state(|state| {
        state.last_gateway_connected_at = Some("unix:1000".to_owned());
        state.last_ready_at = Some("unix:1001".to_owned());
        state.last_heartbeat_ack_at = Some("unix:1190".to_owned());
        // 重连发生但无后续恢复记录（last_ready_at 先于重连，不计为恢复）。
        state.last_reconnect_at = Some("unix:1150".to_owned());
        state.last_c2c_received_at = Some("unix:1200".to_owned());
    });

    let reply = render_c2c_ping_reply_at(
        &message(),
        &config(),
        &runtime,
        &token_snapshot(),
        &health(
            "ok(in-process)",
            LlmUpstreamSnapshot::Available {
                last_success_at: Some("unix:1195".to_owned()),
                provider: Some("deepseek".to_owned()),
                model: Some("deepseek-chat".to_owned()),
                fallback_used: true,
            },
        ),
        PingMode::Summary,
        1200,
    );

    assert!(reply.contains("# 🔴 服务异常"));
    assert!(reply.contains("检测到影响服务的异常"));
    assert!(reply.contains("LLM 上游最近一次调用发生过降级"));
    assert!(reply.contains("最近重连尚未发现恢复记录"));
    assert!(!reply.contains("\n> 最近重连尚未发现恢复记录"));
}

#[tokio::test]
async fn ping_check_direct_failure_overrides_stale_healthz_status() {
    let config = config();
    let runtime = GatewayRuntimeStatus::new_for_test();
    let auth = AccessTokenManager::new(
        reqwest::Client::new(),
        "appid",
        "app-secret-value",
        Duration::from_secs(60),
    );

    let reply = build_c2c_ping_reply_with_check_failure(
        &message_with_content("/ping check"),
        &config,
        &runtime,
        &auth,
        &core_health(),
        Some("主动检查失败：timeout"),
    )
    .await;

    assert!(reply.contains("# 🔴 服务异常"));
    assert!(reply.contains("主动检查失败：timeout"));
    assert!(!reply.contains("stale-model"));
}

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use super::{
    event::C2cMessage,
    logging::{mask_identifier, mask_scope_key, mask_url, mask_url_query},
};
use crate::{
    auth::{AccessTokenManager, AccessTokenSnapshot, AccessTokenSnapshotState},
    config::AppConfig,
};
use qq_maid_common::time_context::{
    format_diagnostic_time_for_display, now_diagnostic_time_for_display, now_unix_seconds_marker,
};

const LLM_HEALTHZ_TIMEOUT: Duration = Duration::from_millis(800);

#[derive(Debug, Clone)]
pub struct GatewayRuntimeStatus {
    pub pid: u32,
    pub instance_id: String,
    pub started_at: String,
    started_instant: Instant,
    state: Arc<Mutex<GatewayRuntimeSnapshot>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatewayRuntimeSnapshot {
    pub state_error: Option<String>,
    pub last_gateway_connected_at: Option<String>,
    pub last_ready_at: Option<String>,
    pub last_resumed_at: Option<String>,
    pub last_heartbeat_ack_at: Option<String>,
    pub last_reconnect_at: Option<String>,
    pub last_invalid_session: Option<InvalidSessionSnapshot>,
    pub last_c2c_received_at: Option<String>,
    pub last_c2c_message_id: Option<String>,
    pub last_qq_send_success_at: Option<String>,
    pub last_qq_send_failure_at: Option<String>,
    pub last_qq_send_failure_summary: Option<String>,
    pub last_respond_success_at: Option<String>,
    pub last_respond_failure_at: Option<String>,
    pub last_respond_failure_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSessionSnapshot {
    pub at: String,
    pub can_resume: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LlmHealthSnapshot {
    healthz_url: String,
    status: String,
}

impl GatewayRuntimeStatus {
    pub fn new() -> Self {
        let started_at = now_unix_seconds_marker();
        Self {
            pid: std::process::id(),
            instance_id: format!("gateway-{}-{started_at}", std::process::id()),
            started_at,
            started_instant: Instant::now(),
            state: Arc::new(Mutex::new(GatewayRuntimeSnapshot::default())),
        }
    }

    pub fn uptime_text(&self) -> String {
        format!("{}s", self.started_instant.elapsed().as_secs())
    }

    pub fn snapshot(&self) -> GatewayRuntimeSnapshot {
        match self.state.lock() {
            Ok(state) => state.clone(),
            Err(_) => GatewayRuntimeSnapshot {
                state_error: Some("runtime state lock poisoned".to_owned()),
                ..GatewayRuntimeSnapshot::default()
            },
        }
    }

    pub fn record_gateway_connected(&self) {
        self.update_state(|state| {
            state.last_gateway_connected_at = Some(now_unix_seconds_marker())
        });
    }

    pub fn record_ready(&self) {
        self.update_state(|state| state.last_ready_at = Some(now_unix_seconds_marker()));
    }

    pub fn record_resumed(&self) {
        self.update_state(|state| state.last_resumed_at = Some(now_unix_seconds_marker()));
    }

    pub fn record_heartbeat_ack(&self) {
        self.update_state(|state| state.last_heartbeat_ack_at = Some(now_unix_seconds_marker()));
    }

    pub fn record_reconnect(&self) {
        self.update_state(|state| state.last_reconnect_at = Some(now_unix_seconds_marker()));
    }

    pub fn record_invalid_session(&self, can_resume: bool) {
        self.update_state(|state| {
            state.last_invalid_session = Some(InvalidSessionSnapshot {
                at: now_unix_seconds_marker(),
                can_resume,
            });
        });
    }

    pub fn record_c2c_message_received(&self, message: &C2cMessage) {
        self.update_state(|state| {
            state.last_c2c_received_at = Some(now_unix_seconds_marker());
            state.last_c2c_message_id = Some(mask_identifier(&message.message_id));
        });
    }

    pub fn record_qq_send_success(&self) {
        self.update_state(|state| state.last_qq_send_success_at = Some(now_unix_seconds_marker()));
    }

    pub fn record_qq_send_failure(&self, summary: impl Into<String>) {
        self.update_state(|state| {
            state.last_qq_send_failure_at = Some(now_unix_seconds_marker());
            state.last_qq_send_failure_summary = Some(compact_summary(summary.into()));
        });
    }

    pub fn record_respond_success(&self) {
        self.update_state(|state| state.last_respond_success_at = Some(now_unix_seconds_marker()));
    }

    pub fn record_respond_failure(&self, summary: impl Into<String>) {
        self.update_state(|state| {
            state.last_respond_failure_at = Some(now_unix_seconds_marker());
            state.last_respond_failure_summary = Some(compact_summary(summary.into()));
        });
    }

    fn update_state(&self, update: impl FnOnce(&mut GatewayRuntimeSnapshot)) {
        if let Ok(mut state) = self.state.lock() {
            update(&mut state);
        }
    }

    #[cfg(test)]
    fn new_for_test() -> Self {
        Self {
            pid: 42,
            instance_id: "gateway-test".to_owned(),
            started_at: "unix:1".to_owned(),
            started_instant: Instant::now() - Duration::from_secs(5),
            state: Arc::new(Mutex::new(GatewayRuntimeSnapshot::default())),
        }
    }
}

impl Default for GatewayRuntimeStatus {
    fn default() -> Self {
        Self::new()
    }
}

pub fn is_ping_command(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("/ping")
}

pub async fn build_c2c_ping_reply(
    message: &C2cMessage,
    config: &AppConfig,
    runtime: &GatewayRuntimeStatus,
    auth: &AccessTokenManager,
) -> String {
    let token_snapshot = auth.snapshot().await;
    let llm_health = probe_llm_healthz(&config.respond_url).await;
    render_c2c_ping_reply(message, config, runtime, &token_snapshot, &llm_health)
}

fn render_c2c_ping_reply(
    message: &C2cMessage,
    config: &AppConfig,
    runtime: &GatewayRuntimeStatus,
    token_snapshot: &AccessTokenSnapshot,
    llm_health: &LlmHealthSnapshot,
) -> String {
    let snapshot = runtime.snapshot();
    let current_scope = format!("private:{}", message.user_openid);
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

    [
        "pong".to_owned(),
        String::new(),
        "概览".to_owned(),
        "- Gateway：OK".to_owned(),
        format!("- LLM healthz：{}", llm_health.status),
        format!("- 当前时间：{}", now_diagnostic_time_for_display()),
        format!("- pid：{}", runtime.pid),
        format!("- 运行时长：{}", runtime.uptime_text()),
        String::new(),
        "Gateway".to_owned(),
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
        String::new(),
        "消息".to_owned(),
        "- 平台：qq_official_gateway_rs".to_owned(),
        "- 事件类型：c2c_message".to_owned(),
        "- 会话类型：私聊".to_owned(),
        format!("- 当前消息 id：{}", mask_identifier(&message.message_id)),
        format!("- 当前用户：{}", mask_identifier(&message.user_openid)),
        format!("- 当前 scope_key：{}", mask_scope_key(&current_scope)),
        format!(
            "- 当前消息时间：{}",
            diagnostic_time_option_text(message.timestamp.as_deref())
        ),
        format!(
            "- 最近收到：{}",
            diagnostic_time_option_text(snapshot.last_c2c_received_at.as_deref())
        ),
        format!(
            "- 最近消息 id：{}",
            option_text(snapshot.last_c2c_message_id.as_deref())
        ),
        format!("- 附件数量：{}", message.attachments.len()),
        String::new(),
        "发送".to_owned(),
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
        String::new(),
        "LLM".to_owned(),
        format!("- respond：{}", mask_url(&config.respond_url)),
        format!("- healthz URL：{}", llm_health.healthz_url),
        format!("- healthz：{}", llm_health.status),
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
        String::new(),
        "配置".to_owned(),
        format!("- sandbox：{}", bool_text(config.sandbox)),
        format!("- api_base：{}", url_host_path(&config.api_base)),
        format!("- respond_url：{}", url_host_path(&config.respond_url)),
        format!("- respond query：{}", mask_url_query(&config.respond_url)),
        format!("- Markdown：{}", bool_text(config.enable_markdown)),
        format!("- Image：{}", bool_text(config.enable_image)),
        format!("- verbose_log：{}", bool_text(config.verbose_log)),
        format!("- 访问令牌缓存：{}", token_snapshot_text(token_snapshot)),
        format!(
            "- refresh margin：{}s",
            token_snapshot.refresh_margin_seconds
        ),
    ]
    .join("\n")
}

async fn probe_llm_healthz(respond_url: &str) -> LlmHealthSnapshot {
    let Ok(healthz_url) = healthz_url_from_respond_url(respond_url) else {
        return LlmHealthSnapshot {
            healthz_url: "invalid url".to_owned(),
            status: "invalid url".to_owned(),
        };
    };
    let healthz_url_text = mask_url(healthz_url.as_str());

    let client = match reqwest::Client::builder()
        .timeout(LLM_HEALTHZ_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            return LlmHealthSnapshot {
                healthz_url: healthz_url_text,
                status: "client build failed".to_owned(),
            };
        }
    };

    match client.get(healthz_url.clone()).send().await {
        Ok(response) => {
            let status = response.status();
            let summary = if status.is_success() {
                format!("ok(status={})", status.as_u16())
            } else {
                format!("http status {}", status.as_u16())
            };
            LlmHealthSnapshot {
                healthz_url: healthz_url_text,
                status: summary,
            }
        }
        Err(error) => LlmHealthSnapshot {
            healthz_url: healthz_url_text,
            status: healthz_error_summary(&error),
        },
    }
}

fn healthz_url_from_respond_url(respond_url: &str) -> Result<reqwest::Url, ()> {
    let mut url = reqwest::Url::parse(respond_url.trim()).map_err(|_| ())?;
    url.set_path("/healthz");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn healthz_error_summary(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timeout".to_owned()
    } else if error.is_connect() {
        "connect failed".to_owned()
    } else if error.is_request() {
        "request failed".to_owned()
    } else {
        "healthz failed".to_owned()
    }
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

fn diagnostic_time_option_text(value: Option<&str>) -> String {
    value
        .filter(|text| !text.trim().is_empty())
        .map(format_diagnostic_time_for_display)
        .unwrap_or_else(|| "无".to_owned())
}

fn bool_text(value: bool) -> &'static str {
    if value { "enabled" } else { "disabled" }
}

fn compact_summary(summary: String) -> String {
    let text = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut compact = text.chars().take(120).collect::<String>();
    if text.chars().count() > 120 {
        compact.push_str("...");
    }
    compact
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };

    use super::*;

    fn config() -> AppConfig {
        AppConfig {
            app_id: "appid".to_owned(),
            app_secret: "app-secret-value".to_owned(),
            sandbox: false,
            api_base: "https://api.sgroup.qq.com".to_owned(),
            token_refresh_margin: Duration::from_secs(60),
            respond_url: "http://127.0.0.1:8787/v1/respond?debug=1&token=real-token&timeout=800"
                .to_owned(),
            enable_markdown: false,
            enable_image: true,
            verbose_log: false,
            push_enabled: true,
            push_host: "127.0.0.1".to_owned(),
            push_port: 8788,
            push_token: None,
        }
    }

    fn message() -> C2cMessage {
        C2cMessage {
            message_id: "msg-sensitive-123456".to_owned(),
            user_openid: "user-openid-123456".to_owned(),
            content: "/ping".to_owned(),
            reply: None,
            timestamp: Some("2026-06-10T12:00:00+08:00".to_owned()),
            attachments: Vec::new(),
        }
    }

    fn token_snapshot() -> AccessTokenSnapshot {
        AccessTokenSnapshot {
            state: AccessTokenSnapshotState::Cached,
            expires_in_seconds: Some(120),
            refresh_margin_seconds: 60,
        }
    }

    fn health(status: &str) -> LlmHealthSnapshot {
        LlmHealthSnapshot {
            healthz_url: "http://127.0.0.1:8787/healthz".to_owned(),
            status: status.to_owned(),
        }
    }

    fn spawn_one_response_server(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0; 1024];
            let _ = stream.read(&mut buffer);
            let _ = stream.write_all(response);
        });
        format!("http://{addr}/v1/respond?token=server-token&debug=1")
    }

    #[test]
    fn detects_ping_command_case_insensitively() {
        assert!(is_ping_command(" /PING "));
        assert!(!is_ping_command("/ping now"));
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
    }

    #[test]
    fn renders_structured_ping_reply_without_secrets() {
        let runtime = GatewayRuntimeStatus::new_for_test();
        let message = message();
        runtime.record_c2c_message_received(&message);
        runtime.record_respond_failure("connect failed");
        runtime.record_qq_send_failure("http status 429");

        let reply = render_c2c_ping_reply(
            &message,
            &config(),
            &runtime,
            &token_snapshot(),
            &health("ok(status=200)"),
        );

        assert!(reply.contains("概览"));
        assert!(reply.contains("Gateway"));
        assert!(reply.contains("消息"));
        assert!(reply.contains("发送"));
        assert!(reply.contains("LLM"));
        assert!(reply.contains("配置"));
        assert!(reply.contains("LLM healthz：ok(status=200)"));
        assert!(reply.contains("当前时间："));
        assert!(reply.contains("+08:00 (unix:"));
        assert!(reply.contains("started_at：1970-01-01 08:00:01 +08:00 (unix:1)"));
        assert!(reply.contains("当前消息时间：2026-06-10 12:00:00 +08:00"));
        assert!(!reply.contains("当前消息时间：2026-06-10T12:00:00+08:00"));
        assert!(!reply.contains("最近收到：unix:"));
        assert!(!reply.contains("最近 respond 失败：unix:"));
        assert!(!reply.contains("最近 QQ 发送失败：unix:"));
        assert!(reply.contains("respond query：debug=1&token=***&timeout=800"));
        assert!(reply.contains("当前用户：******123456"));
        assert!(reply.contains("当前 scope_key：private:******123456"));
        assert!(reply.contains("访问令牌缓存：cached, expires_in=120s"));
        assert!(!reply.contains("记忆模块"));
        assert!(!reply.contains("存储"));
        assert!(!reply.contains("user-openid-123456"));
        assert!(!reply.contains("msg-sensitive-123456"));
        assert!(!reply.contains("real-token"));
        assert!(!reply.contains("app-secret-value"));
        assert!(!reply.contains("Authorization"));
    }

    #[tokio::test]
    async fn build_ping_degrades_invalid_llm_url_to_field_summary() {
        let mut config = config();
        config.respond_url = "http://".to_owned();
        let runtime = GatewayRuntimeStatus::new_for_test();
        let auth = AccessTokenManager::new(
            reqwest::Client::new(),
            "appid",
            "app-secret-value",
            Duration::from_secs(60),
        );

        let reply = build_c2c_ping_reply(&message(), &config, &runtime, &auth).await;

        assert!(reply.contains("LLM healthz：invalid url"));
        assert!(reply.contains("respond_url：invalid url"));
        assert!(reply.contains("访问令牌缓存：empty"));
        assert!(!reply.contains("app-secret-value"));
    }

    #[tokio::test]
    async fn llm_healthz_probe_reports_http_status_and_masks_url() {
        let respond_url = spawn_one_response_server(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n",
        );

        let result = probe_llm_healthz(&respond_url).await;

        assert_eq!(result.status, "http status 503");
        assert!(result.healthz_url.ends_with("/healthz"));
        assert!(!result.healthz_url.contains("server-token"));
    }
}

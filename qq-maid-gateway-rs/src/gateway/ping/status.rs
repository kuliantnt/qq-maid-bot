use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use qq_maid_common::time_context::{format_duration_for_display, now_unix_seconds_marker};

use crate::gateway::{event::C2cMessage, logging::mask_identifier};

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
    /// 当前 QQ WebSocket 是否仍保持连接；与历史连接时间分开表达。
    pub qq_connected: bool,
    /// 微信服务号回调监听器是否已成功绑定且服务任务尚未退出。
    pub wechat_service_listening: bool,
    /// OneBot 11 监听器与活动客户端分开记录，客户端异常断开不会等同于渠道退出。
    pub onebot_listening: bool,
    pub onebot_connected: bool,
    /// 只保存脱敏后的 `self_id` 摘要，不能通过运行状态还原完整 QQ 号。
    pub onebot_self_id_summary: Option<String>,
    pub last_onebot_heartbeat_at: Option<String>,
    pub last_onebot_disconnected_at: Option<String>,
    pub last_onebot_disconnect_summary: Option<String>,
    pub last_onebot_replaced_at: Option<String>,
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
        format_duration_for_display(self.started_instant.elapsed())
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
            state.qq_connected = true;
            state.last_gateway_connected_at = Some(now_unix_seconds_marker());
        });
    }

    pub fn record_gateway_disconnected(&self) {
        self.update_state(|state| state.qq_connected = false);
    }

    pub fn record_wechat_service_listening(&self) {
        self.update_state(|state| state.wechat_service_listening = true);
    }

    pub fn record_wechat_service_stopped(&self) {
        self.update_state(|state| state.wechat_service_listening = false);
    }

    pub fn record_onebot_listening(&self) {
        self.update_state(|state| state.onebot_listening = true);
    }

    pub fn record_onebot_stopped(&self) {
        self.update_state(|state| {
            state.onebot_listening = false;
            state.onebot_connected = false;
        });
    }

    pub fn record_onebot_connected(&self, self_id_summary: String, replaced_existing: bool) {
        self.update_state(|state| {
            state.onebot_connected = true;
            state.onebot_self_id_summary = Some(self_id_summary);
            state.last_onebot_disconnect_summary = None;
            if replaced_existing {
                state.last_onebot_replaced_at = Some(now_unix_seconds_marker());
            }
        });
    }

    pub fn record_onebot_heartbeat(&self) {
        self.update_state(|state| {
            state.last_onebot_heartbeat_at = Some(now_unix_seconds_marker());
        });
    }

    pub fn record_onebot_disconnected(&self, summary: impl Into<String>) {
        self.update_state(|state| {
            state.onebot_connected = false;
            state.last_onebot_disconnected_at = Some(now_unix_seconds_marker());
            state.last_onebot_disconnect_summary = Some(compact_summary(summary.into()));
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
        self.update_state(|state| {
            state.qq_connected = false;
            state.last_reconnect_at = Some(now_unix_seconds_marker());
        });
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
            // runtime 快照只保留脱敏后的消息 ID，避免 `/ping all` 暴露原始 openid/message_id。
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

    pub(super) fn started_elapsed(&self) -> Duration {
        self.started_instant.elapsed()
    }

    pub(super) fn update_state(&self, update: impl FnOnce(&mut GatewayRuntimeSnapshot)) {
        if let Ok(mut state) = self.state.lock() {
            update(&mut state);
        }
    }

    #[cfg(test)]
    pub(super) fn new_for_test() -> Self {
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

fn compact_summary(summary: String) -> String {
    let text = summary.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut compact = text.chars().take(120).collect::<String>();
    if text.chars().count() > 120 {
        compact.push_str("...");
    }
    compact
}

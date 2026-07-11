//! 8787 控制台使用的 Gateway 安全只读摘要。

use qq_maid_core::http::console::{
    ConsoleCapabilities, ConsoleExternalSnapshot, ConsolePlatformStatus, ConsoleRuntimeState,
    ConsoleStatusSource, ConsoleValueState, path_storage,
};

use crate::config::AppConfig;

use super::{outbound::ReplyCapability, ping::GatewayRuntimeStatus};

#[derive(Clone)]
pub struct GatewayConsoleStatusSource {
    config: AppConfig,
    runtime: GatewayRuntimeStatus,
}

impl GatewayConsoleStatusSource {
    pub fn new(config: AppConfig, runtime: GatewayRuntimeStatus) -> Self {
        Self { config, runtime }
    }
}

impl ConsoleStatusSource for GatewayConsoleStatusSource {
    fn snapshot(&self) -> ConsoleExternalSnapshot {
        let runtime = self.runtime.snapshot();
        let qq_capability = ReplyCapability::qq_official_c2c(&self.config);
        let last_event_at = latest_time([
            runtime.last_c2c_received_at.as_deref(),
            runtime.last_heartbeat_ack_at.as_deref(),
            runtime.last_qq_send_success_at.as_deref(),
            runtime.last_respond_success_at.as_deref(),
        ]);
        let last_error_summary = latest_error(
            runtime.last_qq_send_failure_at.as_deref(),
            runtime.last_qq_send_failure_summary.as_deref(),
            runtime.last_respond_failure_at.as_deref(),
            runtime.last_respond_failure_summary.as_deref(),
        );
        // 现有快照只有历史 READY/RESUMED/心跳时刻，没有“当前仍连接”的真值；
        // 因此即使有历史事件也不能伪装成当前在线。
        let qq_state = ConsoleRuntimeState::Unknown;
        let qq = ConsolePlatformStatus {
            id: "qq_official".to_owned(),
            label: "QQ 官方 Gateway".to_owned(),
            configured: true,
            enabled: true,
            state: qq_state,
            last_event_at,
            last_error_summary,
            ready_at: runtime.last_ready_at,
            resumed_at: runtime.last_resumed_at,
            capabilities: ConsoleCapabilities {
                text: capability(qq_capability.render.supports_text),
                markdown: capability(qq_capability.render.supports_markdown),
                image: capability(qq_capability.render.supports_image),
                file: capability(qq_capability.render.supports_attachment),
                mixed_message: capability(qq_capability.supports_multi_part),
                streaming: capability(qq_capability.supports_streaming),
            },
        };

        let wechat_configured = self.config.wechat_service.token.is_some();
        let wechat_enabled = self.config.wechat_service.enabled;
        let wechat_capability =
            ReplyCapability::wechat_service_text_sync(self.config.wechat_service.reply_timeout);
        let unavailable = if wechat_configured {
            None
        } else {
            Some(ConsoleValueState::NotConfigured)
        };
        let wechat = ConsolePlatformStatus {
            id: "wechat_service".to_owned(),
            label: "微信服务号".to_owned(),
            configured: wechat_configured,
            enabled: wechat_enabled,
            state: if !wechat_configured {
                ConsoleRuntimeState::NotConfigured
            } else if wechat_enabled {
                // 当前回调入口没有连接态概念，也没有事件计数器，明确表达未知。
                ConsoleRuntimeState::Unknown
            } else {
                ConsoleRuntimeState::NotAvailable
            },
            last_event_at: None,
            last_error_summary: None,
            ready_at: None,
            resumed_at: None,
            capabilities: ConsoleCapabilities {
                text: unavailable
                    .unwrap_or_else(|| capability(wechat_capability.render.supports_text)),
                markdown: unavailable
                    .unwrap_or_else(|| capability(wechat_capability.render.supports_markdown)),
                image: unavailable
                    .unwrap_or_else(|| capability(wechat_capability.render.supports_image)),
                file: unavailable
                    .unwrap_or_else(|| capability(wechat_capability.render.supports_attachment)),
                mixed_message: unavailable
                    .unwrap_or_else(|| capability(wechat_capability.supports_multi_part)),
                streaming: unavailable
                    .unwrap_or_else(|| capability(wechat_capability.supports_streaming)),
            },
        };

        ConsoleExternalSnapshot {
            platforms: vec![qq, wechat],
            storage: vec![path_storage(
                "attachments",
                "入站附件目录",
                &self.config.media_dir,
                None,
            )],
        }
    }
}

fn capability(supported: bool) -> ConsoleValueState {
    if supported {
        ConsoleValueState::Supported
    } else {
        ConsoleValueState::Unsupported
    }
}

fn latest_time<const N: usize>(values: [Option<&str>; N]) -> Option<String> {
    values.into_iter().flatten().max().map(str::to_owned)
}

fn latest_error(
    send_at: Option<&str>,
    send_summary: Option<&str>,
    respond_at: Option<&str>,
    respond_summary: Option<&str>,
) -> Option<String> {
    match (send_at.zip(send_summary), respond_at.zip(respond_summary)) {
        (Some(send), Some(respond)) if respond.0 > send.0 => Some(respond.1.to_owned()),
        (Some(send), _) => Some(send.1.to_owned()),
        (_, Some(respond)) => Some(respond.1.to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn latest_error_uses_the_newer_safe_summary() {
        assert_eq!(
            latest_error(
                Some("unix:10"),
                Some("send"),
                Some("unix:11"),
                Some("respond")
            ),
            Some("respond".to_owned())
        );
    }

    #[test]
    fn disabled_platform_snapshot_is_safe_and_does_not_fail() {
        let config = AppConfig::from_map(&HashMap::from([
            ("QQ_BOT_APP_ID".to_owned(), "private-app-id".to_owned()),
            ("QQ_BOT_APP_SECRET".to_owned(), "private-secret".to_owned()),
        ]))
        .unwrap();
        let snapshot =
            GatewayConsoleStatusSource::new(config, GatewayRuntimeStatus::new()).snapshot();

        assert_eq!(snapshot.platforms[0].state, ConsoleRuntimeState::Unknown);
        assert_eq!(
            snapshot.platforms[1].state,
            ConsoleRuntimeState::NotConfigured
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("private-app-id"));
        assert!(!json.contains("private-secret"));
    }
}

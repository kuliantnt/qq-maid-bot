//! 8787 控制台使用的 Gateway 安全只读摘要。

use qq_maid_core::http::console::{
    ConsoleCapabilities, ConsoleCapabilityScope, ConsoleDirectionalCapabilities,
    ConsoleExternalSnapshot, ConsolePlatformStatus, ConsoleRuntimeState, ConsoleStatusSource,
    ConsoleValueState, path_storage,
};

use crate::config::{AppConfig, GroupMessageMode, QqOfficialBindingState};

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
        let qq_c2c_capability = ReplyCapability::qq_official_c2c(&self.config);
        let qq_group_capability = ReplyCapability::qq_official_group(&self.config);
        let qq_binding_state = self.config.qq_official_binding_state();
        let qq_configured = qq_binding_state != QqOfficialBindingState::Unbound;
        let qq_enabled = qq_binding_state == QqOfficialBindingState::Enabled;
        let qq_group_enabled =
            qq_enabled && self.config.group_message_mode != GroupMessageMode::Off;
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
        let qq_state = if !qq_configured {
            ConsoleRuntimeState::NotConfigured
        } else if !qq_enabled {
            ConsoleRuntimeState::NotAvailable
        } else if runtime.state_error.is_some() {
            ConsoleRuntimeState::Unknown
        } else if runtime.qq_connected {
            ConsoleRuntimeState::Online
        } else {
            ConsoleRuntimeState::Offline
        };
        let qq = ConsolePlatformStatus {
            id: "qq_official".to_owned(),
            label: "QQ 官方 Gateway".to_owned(),
            configured: qq_configured,
            enabled: qq_enabled,
            state: qq_state,
            last_event_at,
            last_error_summary,
            ready_at: runtime.last_ready_at,
            resumed_at: runtime.last_resumed_at,
            capability_scopes: vec![
                ConsoleCapabilityScope {
                    id: "c2c".to_owned(),
                    label: "私聊 / C2C".to_owned(),
                    enabled: qq_enabled,
                    capabilities: if !qq_configured {
                        unavailable_directional_capabilities(ConsoleValueState::NotConfigured)
                    } else if !qq_enabled {
                        unavailable_directional_capabilities(ConsoleValueState::Disabled)
                    } else {
                        ConsoleDirectionalCapabilities {
                            inbound: qq_inbound_capabilities(),
                            outbound: qq_outbound_capabilities(qq_c2c_capability),
                        }
                    },
                },
                ConsoleCapabilityScope {
                    id: "group".to_owned(),
                    label: "群聊 / Group".to_owned(),
                    enabled: qq_group_enabled,
                    capabilities: if !qq_configured {
                        unavailable_directional_capabilities(ConsoleValueState::NotConfigured)
                    } else if !qq_enabled || !qq_group_enabled {
                        unavailable_directional_capabilities(ConsoleValueState::Disabled)
                    } else {
                        ConsoleDirectionalCapabilities {
                            inbound: qq_inbound_capabilities(),
                            outbound: qq_outbound_capabilities(qq_group_capability),
                        }
                    },
                },
            ],
        };

        let wechat_configured = self.config.wechat_service.token.is_some();
        let wechat_enabled = self.config.wechat_service.enabled;
        let wechat_capability =
            ReplyCapability::wechat_service_text_sync(self.config.wechat_service.reply_timeout);
        let unavailable = if !wechat_configured {
            Some(ConsoleValueState::NotConfigured)
        } else if !wechat_enabled {
            Some(ConsoleValueState::Disabled)
        } else {
            None
        };
        let wechat = ConsolePlatformStatus {
            id: "wechat_service".to_owned(),
            label: "微信服务号".to_owned(),
            configured: wechat_configured,
            enabled: wechat_enabled,
            state: if !wechat_configured {
                ConsoleRuntimeState::NotConfigured
            } else if runtime.wechat_service_listening {
                ConsoleRuntimeState::Online
            } else if wechat_enabled {
                ConsoleRuntimeState::Offline
            } else {
                ConsoleRuntimeState::NotAvailable
            },
            last_event_at: None,
            last_error_summary: None,
            ready_at: None,
            resumed_at: None,
            capability_scopes: vec![ConsoleCapabilityScope {
                id: "service_account".to_owned(),
                label: "服务号回调".to_owned(),
                enabled: wechat_enabled,
                capabilities: unavailable
                    .map(unavailable_directional_capabilities)
                    .unwrap_or(ConsoleDirectionalCapabilities {
                        inbound: ConsoleCapabilities {
                            text: ConsoleValueState::Supported,
                            markdown: ConsoleValueState::Unsupported,
                            image: ConsoleValueState::Unsupported,
                            file: ConsoleValueState::Unsupported,
                            mixed_message: ConsoleValueState::Unsupported,
                            streaming: ConsoleValueState::Unsupported,
                        },
                        outbound: reply_capabilities(wechat_capability),
                    }),
            }],
        };

        let onebot_configured = self.config.onebot11.access_token.is_some();
        let onebot_enabled = self.config.onebot11.enabled;
        let onebot = ConsolePlatformStatus {
            id: "onebot11".to_owned(),
            label: "OneBot 11".to_owned(),
            configured: onebot_configured,
            enabled: onebot_enabled,
            state: if !onebot_configured {
                ConsoleRuntimeState::NotConfigured
            } else if !onebot_enabled {
                ConsoleRuntimeState::NotAvailable
            } else if runtime.onebot_connected {
                ConsoleRuntimeState::Online
            } else if runtime.onebot_listening {
                ConsoleRuntimeState::Available
            } else {
                ConsoleRuntimeState::Offline
            },
            last_event_at: runtime.last_onebot_heartbeat_at,
            last_error_summary: runtime.last_onebot_disconnect_summary,
            ready_at: None,
            resumed_at: None,
            capability_scopes: vec![ConsoleCapabilityScope {
                id: "reverse_websocket".to_owned(),
                label: "反向 WebSocket（协议底座）".to_owned(),
                enabled: onebot_enabled,
                capabilities: unavailable_directional_capabilities(if !onebot_configured {
                    ConsoleValueState::NotConfigured
                } else if !onebot_enabled {
                    ConsoleValueState::Disabled
                } else {
                    // #438 不接业务消息和发送；控制台不能把 transport 在线误报为文本能力可用。
                    ConsoleValueState::NotAvailable
                }),
            }],
        };

        ConsoleExternalSnapshot {
            platforms: vec![qq, wechat, onebot],
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

fn qq_inbound_capabilities() -> ConsoleCapabilities {
    // QQ C2C 与群聊 adapter 当前都支持文本、图片及图文混合解析；文件附件
    // 仍缺少完整处理链路，Markdown 和流式接收没有对应协议语义。
    ConsoleCapabilities {
        text: ConsoleValueState::Supported,
        markdown: ConsoleValueState::Unsupported,
        image: ConsoleValueState::Supported,
        file: ConsoleValueState::Unknown,
        mixed_message: ConsoleValueState::Supported,
        streaming: ConsoleValueState::NotAvailable,
    }
}

fn qq_outbound_capabilities(reply: ReplyCapability) -> ConsoleCapabilities {
    ConsoleCapabilities {
        text: capability(reply.render.supports_text),
        markdown: configurable_capability(true, reply.render.supports_markdown),
        image: configurable_capability(
            reply.image_delivery_implemented,
            reply.render.supports_image,
        ),
        file: capability(reply.render.supports_attachment),
        mixed_message: capability(reply.supports_multi_part),
        streaming: configurable_capability(
            reply.streaming_delivery_implemented,
            reply.supports_streaming,
        ),
    }
}

fn reply_capabilities(reply: ReplyCapability) -> ConsoleCapabilities {
    ConsoleCapabilities {
        text: capability(reply.render.supports_text),
        markdown: capability(reply.render.supports_markdown),
        image: capability(reply.render.supports_image),
        file: capability(reply.render.supports_attachment),
        mixed_message: capability(reply.supports_multi_part),
        streaming: capability(reply.supports_streaming),
    }
}

fn configurable_capability(implemented: bool, enabled: bool) -> ConsoleValueState {
    if !implemented {
        ConsoleValueState::Unsupported
    } else if enabled {
        ConsoleValueState::Supported
    } else {
        ConsoleValueState::Disabled
    }
}

fn unavailable_capabilities(state: ConsoleValueState) -> ConsoleCapabilities {
    ConsoleCapabilities {
        text: state,
        markdown: state,
        image: state,
        file: state,
        mixed_message: state,
        streaming: state,
    }
}

fn unavailable_directional_capabilities(
    state: ConsoleValueState,
) -> ConsoleDirectionalCapabilities {
    ConsoleDirectionalCapabilities {
        inbound: unavailable_capabilities(state),
        outbound: unavailable_capabilities(state),
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

        assert_eq!(snapshot.platforms[0].state, ConsoleRuntimeState::Offline);
        assert_eq!(
            snapshot.platforms[0].capability_scopes[0]
                .capabilities
                .inbound
                .image,
            ConsoleValueState::Supported
        );
        assert_eq!(
            snapshot.platforms[0].capability_scopes[0]
                .capabilities
                .outbound
                .image,
            ConsoleValueState::Disabled
        );
        assert_eq!(
            snapshot.platforms[1].state,
            ConsoleRuntimeState::NotConfigured
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("private-app-id"));
        assert!(!json.contains("private-secret"));
    }

    #[test]
    fn qq_unbound_and_disabled_are_not_reported_as_runtime_failures() {
        let unbound = AppConfig::from_map(&HashMap::from([
            ("WECHAT_SERVICE_ENABLED".to_owned(), "true".to_owned()),
            ("WECHAT_SERVICE_TOKEN".to_owned(), "wechat-token".to_owned()),
        ]))
        .unwrap();
        let snapshot =
            GatewayConsoleStatusSource::new(unbound, GatewayRuntimeStatus::new()).snapshot();
        let qq = platform(&snapshot, "qq_official");
        assert!(!qq.configured);
        assert!(!qq.enabled);
        assert_eq!(qq.state, ConsoleRuntimeState::NotConfigured);
        assert_eq!(
            scope(qq, "c2c").capabilities.outbound.text,
            ConsoleValueState::NotConfigured
        );

        let disabled = AppConfig::from_map(&HashMap::from([
            ("QQ_BOT_APP_ID".to_owned(), "private-app-id".to_owned()),
            ("QQ_BOT_APP_SECRET".to_owned(), "private-secret".to_owned()),
            ("QQ_BOT_ENABLED".to_owned(), "false".to_owned()),
        ]))
        .unwrap();
        let snapshot =
            GatewayConsoleStatusSource::new(disabled, GatewayRuntimeStatus::new()).snapshot();
        let qq = platform(&snapshot, "qq_official");
        assert!(qq.configured);
        assert!(!qq.enabled);
        assert_eq!(qq.state, ConsoleRuntimeState::NotAvailable);
        assert_eq!(
            scope(qq, "c2c").capabilities.outbound.text,
            ConsoleValueState::Disabled
        );
    }

    fn snapshot_with(entries: &[(&str, &str)]) -> ConsoleExternalSnapshot {
        let mut env = HashMap::from([
            ("QQ_BOT_APP_ID".to_owned(), "private-app-id".to_owned()),
            ("QQ_BOT_APP_SECRET".to_owned(), "private-secret".to_owned()),
        ]);
        env.extend(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        );
        let config = AppConfig::from_map(&env).unwrap();
        GatewayConsoleStatusSource::new(config, GatewayRuntimeStatus::new()).snapshot()
    }

    fn platform<'a>(snapshot: &'a ConsoleExternalSnapshot, id: &str) -> &'a ConsolePlatformStatus {
        snapshot
            .platforms
            .iter()
            .find(|platform| platform.id == id)
            .unwrap()
    }

    fn scope<'a>(platform: &'a ConsolePlatformStatus, id: &str) -> &'a ConsoleCapabilityScope {
        platform
            .capability_scopes
            .iter()
            .find(|scope| scope.id == id)
            .unwrap()
    }

    #[test]
    fn wechat_unconfigured_disabled_and_enabled_states_match_capabilities() {
        let unconfigured = snapshot_with(&[]);
        let wechat = platform(&unconfigured, "wechat_service");
        assert!(!wechat.configured);
        assert!(!wechat.enabled);
        assert!(!scope(wechat, "service_account").enabled);
        assert_eq!(wechat.state, ConsoleRuntimeState::NotConfigured);
        assert_eq!(
            scope(wechat, "service_account").capabilities.outbound.text,
            ConsoleValueState::NotConfigured
        );

        let configured_disabled = snapshot_with(&[("WECHAT_SERVICE_TOKEN", "token")]);
        let wechat = platform(&configured_disabled, "wechat_service");
        assert!(wechat.configured);
        assert!(!wechat.enabled);
        assert!(!scope(wechat, "service_account").enabled);
        assert_eq!(wechat.state, ConsoleRuntimeState::NotAvailable);
        assert_eq!(
            scope(wechat, "service_account").capabilities.inbound.text,
            ConsoleValueState::Disabled
        );
        assert_eq!(
            scope(wechat, "service_account").capabilities.outbound.text,
            ConsoleValueState::Disabled
        );

        let configured_enabled = snapshot_with(&[
            ("WECHAT_SERVICE_TOKEN", "token"),
            ("WECHAT_SERVICE_ENABLED", "true"),
        ]);
        let wechat = platform(&configured_enabled, "wechat_service");
        assert!(wechat.configured);
        assert!(wechat.enabled);
        assert!(scope(wechat, "service_account").enabled);
        assert_eq!(wechat.state, ConsoleRuntimeState::Offline);
        let capabilities = &scope(wechat, "service_account").capabilities;
        assert_eq!(capabilities.inbound.text, ConsoleValueState::Supported);
        assert_eq!(capabilities.outbound.text, ConsoleValueState::Supported);
        assert_eq!(
            capabilities.outbound.markdown,
            ConsoleValueState::Unsupported
        );
        assert_eq!(
            capabilities.inbound.streaming,
            ConsoleValueState::Unsupported
        );
    }

    #[test]
    fn onebot_foundation_reports_transport_state_without_business_capabilities() {
        let config = AppConfig::from_map(&HashMap::from([
            ("QQ_BOT_ENABLED".to_owned(), "false".to_owned()),
            ("ONEBOT11_ENABLED".to_owned(), "true".to_owned()),
            (
                "ONEBOT11_ACCESS_TOKEN".to_owned(),
                "private-onebot-token".to_owned(),
            ),
        ]))
        .unwrap();
        let runtime = GatewayRuntimeStatus::new();
        runtime.record_onebot_listening();
        runtime.record_onebot_connected("******123456".to_owned(), false);
        let snapshot = GatewayConsoleStatusSource::new(config, runtime).snapshot();
        let onebot = platform(&snapshot, "onebot11");

        assert!(onebot.configured);
        assert!(onebot.enabled);
        assert_eq!(onebot.state, ConsoleRuntimeState::Online);
        assert_eq!(
            scope(onebot, "reverse_websocket").capabilities.inbound.text,
            ConsoleValueState::NotAvailable
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        assert!(!json.contains("private-onebot-token"));
        assert!(!json.contains("123456123456"));
    }

    #[test]
    fn qq_c2c_and_group_scopes_follow_reply_capabilities() {
        let snapshot = snapshot_with(&[
            ("QQ_MAID_ENABLE_MARKDOWN", "true"),
            ("QQ_MAID_ENABLE_IMAGE", "true"),
            ("QQ_MAID_C2C_FINAL_REPLY_STREAM_ENABLED", "true"),
            ("QQ_MAID_GROUP_MESSAGE_MODE", "mention"),
        ]);
        let qq = platform(&snapshot, "qq_official");
        let c2c = scope(qq, "c2c");
        let group = scope(qq, "group");

        assert!(c2c.enabled);
        assert!(group.enabled);
        assert_eq!(
            c2c.capabilities.outbound.image,
            ConsoleValueState::Supported
        );
        assert_eq!(
            group.capabilities.outbound.image,
            ConsoleValueState::Unsupported
        );
        assert_eq!(
            c2c.capabilities.outbound.markdown,
            ConsoleValueState::Supported
        );
        assert_eq!(
            group.capabilities.outbound.markdown,
            ConsoleValueState::Supported
        );
        assert_eq!(
            c2c.capabilities.outbound.mixed_message,
            ConsoleValueState::Supported
        );
        assert_eq!(
            group.capabilities.outbound.mixed_message,
            ConsoleValueState::Supported
        );
        assert_eq!(
            c2c.capabilities.outbound.streaming,
            ConsoleValueState::Supported
        );
        assert_eq!(
            group.capabilities.outbound.streaming,
            ConsoleValueState::Unsupported
        );
    }

    #[test]
    fn disabled_qq_group_scope_reports_disabled_capabilities() {
        let snapshot = snapshot_with(&[("QQ_MAID_GROUP_MESSAGE_MODE", "off")]);
        let group = scope(platform(&snapshot, "qq_official"), "group");

        assert!(!group.enabled);
        assert_eq!(group.capabilities.inbound.text, ConsoleValueState::Disabled);
        assert_eq!(
            group.capabilities.outbound.streaming,
            ConsoleValueState::Disabled
        );
    }
}

//! Gateway 本地命令的统一分发边界。
//!
//! 平台 adapter 只提交已经解析出的文本与实际可用消息上下文；命令识别、执行和
//! 通用输出格式都收敛在这里。命中后由平台复用自己的发送链路，不再进入 Core respond。

use crate::{
    auth::AccessTokenManager,
    config::AppConfig,
    gateway::{outbound::ReplyCapability, ping, ping::GatewayRuntimeStatus},
    markdown::MarkdownPayload,
    render::OutboundMessage,
    respond::RespondClient,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GatewayCommandConversation {
    Private,
    Group,
    ServiceAccount,
}

impl GatewayCommandConversation {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Private => "私聊",
            Self::Group => "群聊",
            Self::ServiceAccount => "服务号会话",
        }
    }

    pub(crate) fn scope_kind(self) -> &'static str {
        match self {
            Self::Private | Self::ServiceAccount => "private",
            Self::Group => "group",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatewayCommandContext {
    pub(crate) platform_name: &'static str,
    pub(crate) platform_code: &'static str,
    pub(crate) event_name: &'static str,
    pub(crate) conversation: GatewayCommandConversation,
    pub(crate) user_id: Option<String>,
    pub(crate) group_id: Option<String>,
    pub(crate) message_id: Option<String>,
    pub(crate) timestamp: Option<String>,
    pub(crate) attachment_count: usize,
}

impl GatewayCommandContext {
    pub(crate) fn scope_key(&self) -> Option<String> {
        let target = match self.conversation {
            GatewayCommandConversation::Group => self.group_id.as_deref(),
            GatewayCommandConversation::Private | GatewayCommandConversation::ServiceAccount => {
                self.user_id.as_deref()
            }
        }?;
        Some(format!("{}:{target}", self.conversation.scope_kind()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GatewayCommandOutput {
    markdown: String,
}

impl GatewayCommandOutput {
    fn markdown(markdown: String) -> Self {
        Self { markdown }
    }

    pub(crate) fn render(&self, capability: &ReplyCapability) -> OutboundMessage {
        if capability.render.supports_markdown {
            return OutboundMessage::Markdown {
                markdown: MarkdownPayload::new(self.markdown.clone()),
                fallback_text: self.markdown.clone(),
            };
        }
        OutboundMessage::Text {
            text: self.markdown.clone(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct GatewayCommandService {
    config: AppConfig,
    runtime: GatewayRuntimeStatus,
    respond: RespondClient,
    qq_auth: Option<AccessTokenManager>,
}

impl GatewayCommandService {
    pub(crate) fn new(
        config: AppConfig,
        runtime: GatewayRuntimeStatus,
        respond: RespondClient,
        qq_auth: Option<AccessTokenManager>,
    ) -> Self {
        Self {
            config,
            runtime,
            respond,
            qq_auth,
        }
    }

    pub(crate) fn from_config(
        config: AppConfig,
        runtime: GatewayRuntimeStatus,
        respond: RespondClient,
    ) -> Self {
        let qq_auth = config
            .enabled_qq_official_credentials()
            .map(|(app_id, app_secret)| {
                AccessTokenManager::new(
                    reqwest::Client::new(),
                    app_id,
                    app_secret,
                    config.token_refresh_margin,
                )
            });
        Self::new(config, runtime, respond, qq_auth)
    }

    /// 返回 `Some` 表示本地命令已完全接管本轮消息，平台必须直接回包并停止后续 Core 流程。
    pub(crate) async fn try_handle(
        &self,
        text: &str,
        context: &GatewayCommandContext,
    ) -> Option<GatewayCommandOutput> {
        if !ping::is_ping_command(text) {
            return None;
        }
        let check_failure = if ping::is_ping_check_command(text) {
            self.respond
                .check_upstream()
                .await
                .err()
                .map(|error| format!("主动检查失败：{}", error.qq_visible_kind()))
        } else {
            None
        };
        let token_snapshot = match self.qq_auth.as_ref() {
            Some(auth) => auth.snapshot().await,
            None => ping::empty_token_snapshot(self.config.token_refresh_margin),
        };
        let reply = ping::build_ping_reply(
            text,
            context,
            &self.config,
            &self.runtime,
            &token_snapshot,
            &self.respond.health_snapshot(),
            check_failure.as_deref(),
        );
        Some(GatewayCommandOutput::markdown(reply))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use async_trait::async_trait;
    use qq_maid_core::service::{
        CoreError, CoreHealthSnapshot, CoreInboundClassification, CoreRequest, CoreRespondOutput,
        CoreService, UpstreamStatusSnapshot,
    };

    use super::*;

    #[derive(Default)]
    struct CountingCore {
        respond_calls: AtomicUsize,
        upstream_calls: AtomicUsize,
    }

    #[async_trait]
    impl CoreService for CountingCore {
        async fn respond(&self, _request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
            self.respond_calls.fetch_add(1, Ordering::SeqCst);
            unreachable!("plain /ping must not call Core respond")
        }

        async fn classify_inbound(
            &self,
            _request: CoreRequest,
        ) -> Result<CoreInboundClassification, CoreError> {
            unreachable!("Gateway commands must not call Core classification")
        }

        async fn upstream_check(&self) -> Result<(), CoreError> {
            self.upstream_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn health_snapshot(&self) -> CoreHealthSnapshot {
            CoreHealthSnapshot {
                ok: true,
                provider: "test".to_owned(),
                model: "test".to_owned(),
                stream: false,
                upstream: UpstreamStatusSnapshot::default(),
            }
        }
    }

    fn qq_context() -> GatewayCommandContext {
        GatewayCommandContext {
            platform_name: "QQ 官方机器人",
            platform_code: "qq_official_gateway_rs",
            event_name: "C2C 消息",
            conversation: GatewayCommandConversation::Private,
            user_id: Some("user-1".to_owned()),
            group_id: None,
            message_id: Some("message-1".to_owned()),
            timestamp: None,
            attachment_count: 0,
        }
    }

    #[test]
    fn context_builds_platform_neutral_scope() {
        let context = GatewayCommandContext {
            platform_name: "OneBot 11",
            platform_code: "onebot11",
            event_name: "private_message",
            conversation: GatewayCommandConversation::Private,
            user_id: Some("user-1".to_owned()),
            group_id: None,
            message_id: Some("message-1".to_owned()),
            timestamp: None,
            attachment_count: 0,
        };

        assert_eq!(context.scope_key().as_deref(), Some("private:user-1"));
    }

    #[test]
    fn command_output_keeps_markdown_and_downgrades_for_text_platforms() {
        let output = GatewayCommandOutput::markdown("# 状态\n\n| 模块 | 状态 |".to_owned());
        let mut config = AppConfig::from_map(&std::collections::HashMap::new()).unwrap();
        config.enable_markdown = true;
        let markdown = output.render(&ReplyCapability::qq_official_c2c(&config));
        assert!(matches!(markdown, OutboundMessage::Markdown { .. }));

        let text = output.render(&ReplyCapability::onebot11_text());
        assert_eq!(
            text,
            OutboundMessage::Text {
                text: "# 状态\n\n| 模块 | 状态 |".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn plain_ping_is_handled_without_core_or_upstream_call() {
        let core = Arc::new(CountingCore::default());
        let config = AppConfig::from_map(&std::collections::HashMap::new()).unwrap();
        let commands = GatewayCommandService::new(
            config,
            GatewayRuntimeStatus::new(),
            RespondClient::new(core.clone()),
            None,
        );

        assert!(commands.try_handle("/ping", &qq_context()).await.is_some());
        assert_eq!(core.respond_calls.load(Ordering::SeqCst), 0);
        assert_eq!(core.upstream_calls.load(Ordering::SeqCst), 0);
        assert!(
            commands
                .try_handle("/pingxxx", &qq_context())
                .await
                .is_none()
        );
    }
}

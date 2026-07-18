//! gateway 出站发送封装。
//!
//! 这里集中维护“真实 QQ 发送 -> runtime 状态记录”的约束，
//! 避免不同调用点各自实现后出现重复记录或遗漏记录。

use std::time::Duration;

use crate::{
    api::{
        C2cReplyTarget, GroupOutboundSender, GroupReplyTarget, OutboundSender, QqApiClient,
        SendFuture, SendResult,
    },
    config::AppConfig,
    markdown::MarkdownPayload,
    media::ImagePayload,
};

use super::{ping::GatewayRuntimeStatus, platform::Platform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryMode {
    SynchronousReply,
    AsynchronousReply,
    Streaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnsupportedCapabilityFallback {
    UseText,
    UsePlaceholderText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LongRunningReplyStrategy {
    KeepAsyncDelivery,
    RequireAsyncFollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReplyTarget {
    Private {
        platform: Platform,
        target_id: String,
        source_message_id: Option<String>,
    },
    Group {
        platform: Platform,
        target_id: String,
        source_message_id: Option<String>,
    },
    #[allow(dead_code)]
    ServiceAccount {
        platform: Platform,
        target_id: String,
        source_message_id: Option<String>,
    },
}

impl ReplyTarget {
    pub(crate) fn qq_c2c(target_id: impl Into<String>, source_message_id: Option<String>) -> Self {
        Self::Private {
            platform: Platform::QqOfficial,
            target_id: target_id.into(),
            source_message_id,
        }
    }

    pub(crate) fn qq_group(
        target_id: impl Into<String>,
        source_message_id: Option<String>,
    ) -> Self {
        Self::Group {
            platform: Platform::QqOfficial,
            target_id: target_id.into(),
            source_message_id,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn onebot_private(
        target_id: impl Into<String>,
        source_message_id: Option<String>,
    ) -> Self {
        Self::Private {
            platform: Platform::OneBot11,
            target_id: target_id.into(),
            source_message_id,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn onebot_group(
        target_id: impl Into<String>,
        source_message_id: Option<String>,
    ) -> Self {
        Self::Group {
            platform: Platform::OneBot11,
            target_id: target_id.into(),
            source_message_id,
        }
    }

    pub(crate) fn to_qq_c2c_target(&self) -> Option<C2cReplyTarget> {
        match self {
            Self::Private {
                platform: Platform::QqOfficial,
                target_id,
                source_message_id,
            } => Some(C2cReplyTarget {
                user_openid: target_id.clone(),
                msg_id: source_message_id.clone(),
            }),
            _ => None,
        }
    }

    pub(crate) fn to_qq_group_target(&self) -> Option<GroupReplyTarget> {
        match self {
            Self::Group {
                platform: Platform::QqOfficial,
                target_id,
                source_message_id,
            } => Some(GroupReplyTarget {
                group_openid: target_id.clone(),
                msg_id: source_message_id.clone(),
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RenderProfile {
    pub(crate) supports_text: bool,
    pub(crate) supports_markdown: bool,
    pub(crate) supports_image: bool,
    pub(crate) supports_attachment: bool,
    pub(crate) unsupported_fallback: UnsupportedCapabilityFallback,
}

impl RenderProfile {
    pub(crate) fn qq_official(config: &AppConfig, supports_image: bool) -> Self {
        Self {
            supports_text: true,
            supports_markdown: config.enable_markdown,
            supports_image: supports_image && config.enable_image,
            supports_attachment: false,
            unsupported_fallback: UnsupportedCapabilityFallback::UseText,
        }
    }

    pub(crate) fn text_only_sync() -> Self {
        Self {
            supports_text: true,
            supports_markdown: false,
            supports_image: false,
            supports_attachment: false,
            unsupported_fallback: UnsupportedCapabilityFallback::UsePlaceholderText,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplyLimits {
    pub(crate) single_message_chars: Option<usize>,
    pub(crate) reply_timeout: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplyCapability {
    pub(crate) platform: Platform,
    pub(crate) render: RenderProfile,
    pub(crate) supports_quote_original: bool,
    pub(crate) supports_at_mention: bool,
    pub(crate) supports_multi_part: bool,
    /// 区分 sender 未实现图片通道与图片通道已实现但被运行配置关闭。
    pub(crate) image_delivery_implemented: bool,
    pub(crate) supports_synchronous_reply: bool,
    pub(crate) supports_asynchronous_reply: bool,
    /// 区分场景不支持流式发送与流式发送已实现但被运行配置关闭。
    pub(crate) streaming_delivery_implemented: bool,
    pub(crate) supports_streaming: bool,
    pub(crate) limits: ReplyLimits,
    pub(crate) long_running_strategy: LongRunningReplyStrategy,
}

impl ReplyCapability {
    pub(crate) fn qq_official_c2c(config: &AppConfig) -> Self {
        Self {
            platform: Platform::QqOfficial,
            render: RenderProfile::qq_official(config, true),
            supports_quote_original: true,
            supports_at_mention: false,
            supports_multi_part: true,
            image_delivery_implemented: true,
            supports_synchronous_reply: false,
            supports_asynchronous_reply: true,
            streaming_delivery_implemented: true,
            supports_streaming: config.c2c_final_reply_stream_enabled,
            limits: ReplyLimits {
                single_message_chars: Some(
                    config
                        .markdown_chunk_soft_limit
                        .max(config.text_chunk_soft_limit),
                ),
                reply_timeout: None,
            },
            long_running_strategy: LongRunningReplyStrategy::KeepAsyncDelivery,
        }
    }

    pub(crate) fn qq_official_group(config: &AppConfig) -> Self {
        Self {
            platform: Platform::QqOfficial,
            render: RenderProfile::qq_official(config, false),
            supports_quote_original: true,
            supports_at_mention: true,
            supports_multi_part: true,
            image_delivery_implemented: false,
            supports_synchronous_reply: false,
            supports_asynchronous_reply: true,
            streaming_delivery_implemented: false,
            supports_streaming: false,
            limits: ReplyLimits {
                single_message_chars: Some(
                    config
                        .markdown_chunk_soft_limit
                        .max(config.text_chunk_soft_limit),
                ),
                reply_timeout: None,
            },
            long_running_strategy: LongRunningReplyStrategy::KeepAsyncDelivery,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn onebot11_text() -> Self {
        Self {
            platform: Platform::OneBot11,
            render: RenderProfile {
                supports_text: true,
                supports_markdown: false,
                supports_image: false,
                supports_attachment: false,
                unsupported_fallback: UnsupportedCapabilityFallback::UseText,
            },
            supports_quote_original: false,
            supports_at_mention: false,
            supports_multi_part: false,
            image_delivery_implemented: false,
            supports_synchronous_reply: false,
            supports_asynchronous_reply: true,
            streaming_delivery_implemented: false,
            supports_streaming: false,
            limits: ReplyLimits {
                single_message_chars: None,
                reply_timeout: None,
            },
            long_running_strategy: LongRunningReplyStrategy::KeepAsyncDelivery,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn wechat_service_text_sync(reply_timeout: Duration) -> Self {
        Self {
            platform: Platform::WechatService,
            render: RenderProfile::text_only_sync(),
            supports_quote_original: false,
            supports_at_mention: false,
            supports_multi_part: false,
            image_delivery_implemented: false,
            supports_synchronous_reply: true,
            supports_asynchronous_reply: true,
            streaming_delivery_implemented: false,
            supports_streaming: false,
            limits: ReplyLimits {
                single_message_chars: None,
                reply_timeout: Some(reply_timeout),
            },
            long_running_strategy: LongRunningReplyStrategy::RequireAsyncFollowUp,
        }
    }

    pub(crate) fn supports_delivery_mode(self, mode: DeliveryMode) -> bool {
        match mode {
            DeliveryMode::SynchronousReply => self.supports_synchronous_reply,
            DeliveryMode::AsynchronousReply => self.supports_asynchronous_reply,
            DeliveryMode::Streaming => self.supports_streaming,
        }
    }
}

pub(crate) async fn send_c2c_text_with_status(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    user_openid: &str,
    msg_id: Option<&str>,
    text: &str,
) -> SendResult {
    let result = api.send_c2c_text(user_openid, msg_id, text).await;
    record_qq_send_result(runtime, &result);
    result
}

pub(crate) async fn send_group_text_with_status(
    api: &QqApiClient,
    runtime: &GatewayRuntimeStatus,
    group_openid: &str,
    msg_id: Option<&str>,
    text: &str,
) -> SendResult {
    let result = api.send_group_text(group_openid, msg_id, text).await;
    record_qq_send_result(runtime, &result);
    result
}

pub(crate) fn record_qq_send_result(runtime: &GatewayRuntimeStatus, result: &SendResult) {
    match result {
        Ok(_) => runtime.record_qq_send_success(),
        Err(err) => runtime.record_qq_send_failure(err.log_summary()),
    }
}

pub(crate) struct RuntimeRecordingSender<'a> {
    pub(crate) inner: &'a QqApiClient,
    pub(crate) runtime: &'a GatewayRuntimeStatus,
}

pub(crate) struct RuntimeRecordingGroupSender<'a> {
    pub(crate) inner: &'a QqApiClient,
    pub(crate) runtime: &'a GatewayRuntimeStatus,
}

impl OutboundSender for RuntimeRecordingSender<'_> {
    fn send_text<'a>(&'a self, target: &'a C2cReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_text(&target.user_openid, target.msg_id.as_deref(), text)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }

    fn send_markdown<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_markdown(&target.user_openid, target.msg_id.as_deref(), markdown)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }

    fn send_image<'a>(
        &'a self,
        target: &'a C2cReplyTarget,
        image: &'a ImagePayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_c2c_image(&target.user_openid, target.msg_id.as_deref(), image)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }
}

impl GroupOutboundSender for RuntimeRecordingGroupSender<'_> {
    fn send_text<'a>(&'a self, target: &'a GroupReplyTarget, text: &'a str) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_group_text(&target.group_openid, target.msg_id.as_deref(), text)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }

    fn send_markdown<'a>(
        &'a self,
        target: &'a GroupReplyTarget,
        markdown: &'a MarkdownPayload,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            let result = self
                .inner
                .send_group_markdown(&target.group_openid, target.msg_id.as_deref(), markdown)
                .await;
            record_qq_send_result(self.runtime, &result);
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiError;
    use crate::gateway::test_support::qq_official_test_config;

    fn test_config() -> AppConfig {
        let mut config = qq_official_test_config();
        config.enable_group_messages = true;
        config.c2c_final_reply_stream_enabled = true;
        config
    }

    #[test]
    fn record_qq_send_result_updates_runtime_status() {
        let runtime = GatewayRuntimeStatus::new();
        let success: SendResult = Ok(crate::api::SendMessageIds::none());

        record_qq_send_result(&runtime, &success);
        let snapshot = runtime.snapshot();
        assert!(snapshot.last_qq_send_success_at.is_some());
        assert_eq!(snapshot.last_qq_send_failure_at, None);

        let failure: SendResult = Err(ApiError::Unsupported("text"));
        record_qq_send_result(&runtime, &failure);
        let snapshot = runtime.snapshot();

        assert!(snapshot.last_qq_send_failure_at.is_some());
        assert_eq!(
            snapshot.last_qq_send_failure_summary.as_deref(),
            Some("text sending is unsupported")
        );
    }

    #[test]
    fn qq_official_c2c_capability_expresses_text_markdown_stream_and_multipart() {
        let capability = ReplyCapability::qq_official_c2c(&test_config());

        assert_eq!(capability.platform, Platform::QqOfficial);
        assert!(capability.render.supports_text);
        assert!(capability.render.supports_markdown);
        assert!(capability.supports_multi_part);
        assert!(capability.supports_quote_original);
        assert!(!capability.supports_at_mention);
        assert!(capability.supports_delivery_mode(DeliveryMode::AsynchronousReply));
        assert!(capability.supports_delivery_mode(DeliveryMode::Streaming));
    }

    #[test]
    fn reply_target_adapts_qq_private_and_group_targets() {
        let private = ReplyTarget::qq_c2c("user-1", Some("msg-1".to_owned()))
            .to_qq_c2c_target()
            .unwrap();
        assert_eq!(private.user_openid, "user-1");
        assert_eq!(private.msg_id.as_deref(), Some("msg-1"));

        let group = ReplyTarget::qq_group("group-1", Some("group-msg-1".to_owned()))
            .to_qq_group_target()
            .unwrap();
        assert_eq!(group.group_openid, "group-1");
        assert_eq!(group.msg_id.as_deref(), Some("group-msg-1"));
    }

    #[test]
    fn qq_official_markdown_capability_follows_runtime_config() {
        let mut config = test_config();
        config.enable_markdown = false;
        let capability = ReplyCapability::qq_official_c2c(&config);

        assert!(!capability.render.supports_markdown);
        assert_eq!(
            capability.render.unsupported_fallback,
            UnsupportedCapabilityFallback::UseText
        );
    }

    #[test]
    fn wechat_service_capability_is_text_sync_without_streaming() {
        let capability = ReplyCapability::wechat_service_text_sync(Duration::from_secs(5));

        assert_eq!(capability.platform, Platform::WechatService);
        assert!(capability.render.supports_text);
        assert!(!capability.render.supports_markdown);
        assert!(capability.supports_delivery_mode(DeliveryMode::SynchronousReply));
        assert!(capability.supports_delivery_mode(DeliveryMode::AsynchronousReply));
        assert!(!capability.supports_delivery_mode(DeliveryMode::Streaming));
        assert_eq!(
            capability.long_running_strategy,
            LongRunningReplyStrategy::RequireAsyncFollowUp
        );
        assert_eq!(
            capability.limits.reply_timeout,
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn onebot_capability_is_text_only_async_and_non_streaming() {
        let capability = ReplyCapability::onebot11_text();

        assert_eq!(capability.platform, Platform::OneBot11);
        assert!(capability.render.supports_text);
        assert!(!capability.render.supports_markdown);
        assert!(!capability.render.supports_image);
        assert_eq!(
            capability.render.unsupported_fallback,
            UnsupportedCapabilityFallback::UseText
        );
        assert!(capability.supports_delivery_mode(DeliveryMode::AsynchronousReply));
        assert!(!capability.supports_delivery_mode(DeliveryMode::SynchronousReply));
        assert!(!capability.supports_delivery_mode(DeliveryMode::Streaming));
    }
}

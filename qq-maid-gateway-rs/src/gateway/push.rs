//! Gateway 进程内主动推送实现。
//!
//! Core 只通过 `PushSink` 交付推送意图；本模块按 platform/account 精确选择 sender。
//! QQ 官方继续负责 Markdown fallback 和群消息缓存；OneBot 只发送纯文本 segment。

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use qq_maid_core::runtime::push::{
    ONEBOT11_PLATFORM, PushError, PushIntent, PushResult, PushSink, PushTargetType,
    QQ_OFFICIAL_PLATFORM,
};
use tokio::{sync::Notify, time::Instant};
use tracing::{info, warn};

use crate::{
    api::{
        QqApiClient, SendMessageIds, SendResult, build_c2c_text_payload, build_group_text_payload,
    },
    gateway::{
        BotOutboundCache,
        logging::mask_identifier,
        onebot11::{OneBotSendError, OneBotSendResult, OneBotSender},
        ping::GatewayRuntimeStatus,
        platform::ConversationTarget,
        ref_index::SharedRefIndex,
    },
    markdown::MarkdownPayload,
};

#[async_trait]
trait PushQqSender: Send + Sync {
    async fn send_c2c_text(&self, target_id: &str, text: &str) -> SendResult;
    async fn send_c2c_markdown(&self, target_id: &str, markdown: &MarkdownPayload) -> SendResult;
    async fn send_group_text(&self, target_id: &str, text: &str) -> SendResult;
    async fn send_group_markdown(&self, target_id: &str, markdown: &MarkdownPayload) -> SendResult;
}

#[async_trait]
impl PushQqSender for QqApiClient {
    async fn send_c2c_text(&self, target_id: &str, text: &str) -> SendResult {
        QqApiClient::send_c2c_text(self, target_id, None, text).await
    }

    async fn send_c2c_markdown(&self, target_id: &str, markdown: &MarkdownPayload) -> SendResult {
        QqApiClient::send_c2c_markdown(self, target_id, None, markdown).await
    }

    async fn send_group_text(&self, target_id: &str, text: &str) -> SendResult {
        QqApiClient::send_group_text(self, target_id, None, text).await
    }

    async fn send_group_markdown(&self, target_id: &str, markdown: &MarkdownPayload) -> SendResult {
        QqApiClient::send_group_markdown(self, target_id, None, markdown).await
    }
}

#[derive(Clone)]
pub struct GatewayPushSink {
    inner: Arc<Mutex<GatewayPushState>>,
    ready: Arc<Notify>,
}

#[derive(Clone)]
struct GatewayPushState {
    qq_official: PushChannelState<GatewayPushRuntime>,
    onebot11: PushChannelState<OneBotPushRuntime>,
}

#[derive(Clone)]
enum PushChannelState<T> {
    Pending,
    Bound(T),
    Unavailable(&'static str),
}

#[derive(Clone)]
enum RoutedPushRuntime {
    QqOfficial(GatewayPushRuntime),
    OneBot11(OneBotPushRuntime),
}

enum RouteSnapshot {
    Pending,
    Bound(RoutedPushRuntime),
    Unavailable(&'static str),
}

#[derive(Clone)]
struct GatewayPushRuntime {
    api: QqApiClient,
    qq_official_account_id: String,
    runtime: GatewayRuntimeStatus,
    group_outbound_cache: Arc<Mutex<BotOutboundCache>>,
    ref_index: SharedRefIndex,
}

#[async_trait]
trait PushOneBotSender: Send + Sync {
    fn connected_account_id(&self) -> Option<String>;
    async fn send_private_text(
        &self,
        target_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError>;
    async fn send_group_text(
        &self,
        target_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError>;
}

#[async_trait]
impl PushOneBotSender for OneBotSender {
    fn connected_account_id(&self) -> Option<String> {
        OneBotSender::connected_account_id(self)
    }

    async fn send_private_text(
        &self,
        target_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        OneBotSender::send_private_text(self, target_id, text).await
    }

    async fn send_group_text(
        &self,
        target_id: &str,
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        OneBotSender::send_group_text(self, target_id, text).await
    }
}

#[derive(Clone)]
struct OneBotPushRuntime {
    sender: Arc<dyn PushOneBotSender>,
    ref_index: SharedRefIndex,
}

#[derive(Debug)]
struct PushSendOutcome {
    ids: SendMessageIds,
    delivered_text: String,
}

impl GatewayPushSink {
    pub fn unbound() -> Self {
        Self {
            inner: Arc::new(Mutex::new(GatewayPushState {
                qq_official: PushChannelState::Pending,
                onebot11: PushChannelState::Pending,
            })),
            ready: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn bind(
        &self,
        api: QqApiClient,
        qq_official_account_id: impl Into<String>,
        runtime: GatewayRuntimeStatus,
        group_outbound_cache: Arc<Mutex<BotOutboundCache>>,
        ref_index: SharedRefIndex,
    ) {
        // Core scheduler 可能在 Gateway 首次连接 QQ 前启动，因此 sink 需要先存在；
        // 真正发送前必须已绑定运行期上下文，否则返回可观测错误而不是静默丢消息。
        self.inner.lock().unwrap().qq_official = PushChannelState::Bound(GatewayPushRuntime {
            api,
            qq_official_account_id: qq_official_account_id.into(),
            runtime,
            group_outbound_cache,
            ref_index,
        });
        self.ready.notify_waiters();
    }

    pub(crate) fn mark_qq_official_unavailable(&self, summary: &'static str) {
        self.inner.lock().unwrap().qq_official = PushChannelState::Unavailable(summary);
        self.ready.notify_waiters();
    }

    pub(crate) fn bind_onebot11(&self, sender: OneBotSender, ref_index: SharedRefIndex) {
        self.bind_onebot_sender(Arc::new(sender), ref_index);
    }

    fn bind_onebot_sender(&self, sender: Arc<dyn PushOneBotSender>, ref_index: SharedRefIndex) {
        self.inner.lock().unwrap().onebot11 =
            PushChannelState::Bound(OneBotPushRuntime { sender, ref_index });
        self.ready.notify_waiters();
    }

    pub(crate) fn mark_onebot11_unavailable(&self, summary: &'static str) {
        self.inner.lock().unwrap().onebot11 = PushChannelState::Unavailable(summary);
        self.ready.notify_waiters();
    }

    async fn runtime(&self, platform: &str) -> Result<RoutedPushRuntime, PushError> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            // 先创建 waiter 再读取状态，避免 bind 恰好发生在状态读取和等待之间时漏通知。
            let notified = self.ready.notified();
            match self.route_snapshot(platform)? {
                RouteSnapshot::Bound(runtime) => return Ok(runtime),
                RouteSnapshot::Unavailable(summary) => {
                    return Err(PushError::Failed {
                        summary: summary.to_owned(),
                    });
                }
                RouteSnapshot::Pending => {}
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(PushError::Failed {
                    summary: format!("gateway push sink for `{platform}` is not ready"),
                });
            }
        }
    }

    fn route_snapshot(&self, platform: &str) -> Result<RouteSnapshot, PushError> {
        let state = self.inner.lock().unwrap();
        match platform {
            QQ_OFFICIAL_PLATFORM => Ok(match &state.qq_official {
                PushChannelState::Pending => RouteSnapshot::Pending,
                PushChannelState::Bound(runtime) => {
                    RouteSnapshot::Bound(RoutedPushRuntime::QqOfficial(runtime.clone()))
                }
                PushChannelState::Unavailable(summary) => RouteSnapshot::Unavailable(summary),
            }),
            ONEBOT11_PLATFORM => Ok(match &state.onebot11 {
                PushChannelState::Pending => RouteSnapshot::Pending,
                PushChannelState::Bound(runtime) => {
                    RouteSnapshot::Bound(RoutedPushRuntime::OneBot11(runtime.clone()))
                }
                PushChannelState::Unavailable(summary) => RouteSnapshot::Unavailable(summary),
            }),
            other => Err(PushError::Failed {
                summary: format!("push platform `{other}` is not supported by gateway"),
            }),
        }
    }
}

#[async_trait]
impl PushSink for GatewayPushSink {
    async fn push(&self, intent: PushIntent) -> Result<PushResult, PushError> {
        let platform = intent.target.platform.trim();
        match self.runtime(platform).await? {
            RoutedPushRuntime::QqOfficial(runtime) => runtime.push(intent).await,
            RoutedPushRuntime::OneBot11(runtime) => runtime.push(intent).await,
        }
    }
}

impl OneBotPushRuntime {
    async fn push(&self, intent: PushIntent) -> Result<PushResult, PushError> {
        let target_id = intent.target.target_id.trim();
        let text = intent.text.trim();
        if target_id.is_empty() || text.is_empty() {
            return Err(PushError::Failed {
                summary: "target_id and text are required".to_owned(),
            });
        }
        let target_account = intent
            .target
            .account_id
            .as_deref()
            .map(str::trim)
            .filter(|account| !account.is_empty())
            .ok_or_else(|| PushError::Failed {
                summary: "onebot11 push target account_id is required".to_owned(),
            })?;
        let connected_account =
            self.sender
                .connected_account_id()
                .ok_or_else(|| PushError::Failed {
                    summary: "OneBot 11 account is offline".to_owned(),
                })?;
        if target_account != connected_account {
            return Err(PushError::Failed {
                summary: "push target account does not match connected OneBot 11 account"
                    .to_owned(),
            });
        }

        // OneBot 一期只有 text segment；Markdown、图片等结构化意图统一使用上游已生成的
        // 纯文本 fallback，不能把 QQ Markdown payload 或 CQ 码带入 sender。
        let fallback_text = intent
            .fallback_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(text);
        let delivered_text = if matches!(intent.message_type.trim(), "" | "text") {
            text
        } else {
            fallback_text
        };
        let result = match intent.target.target_type {
            PushTargetType::Private => {
                self.sender
                    .send_private_text(target_id, delivered_text)
                    .await
            }
            PushTargetType::Group => self.sender.send_group_text(target_id, delivered_text).await,
        }
        .map_err(|error| PushError::Failed {
            // sender 的错误摘要不会包含消息正文、token 或完整 response envelope。
            summary: error.to_string(),
        })?;
        let conversation = match intent.target.target_type {
            PushTargetType::Private => ConversationTarget::Private {
                target_id: intent.target.target_id.clone(),
            },
            PushTargetType::Group => ConversationTarget::Group {
                target_id: intent.target.target_id.clone(),
            },
        };
        self.ref_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert_bot_outbound(
                crate::gateway::platform::Platform::OneBot11,
                Some(target_account),
                &conversation,
                Some(result.message_id.clone()),
                delivered_text,
                intent.visible_entity_snapshot.clone(),
            );
        Ok(PushResult {
            message_id: Some(result.message_id),
        })
    }
}

impl GatewayPushRuntime {
    async fn push(&self, intent: PushIntent) -> Result<PushResult, PushError> {
        let target_id = intent.target.target_id.trim();
        let text = intent.text.trim();
        if target_id.is_empty() || text.is_empty() {
            return Err(PushError::Failed {
                summary: "target_id and text are required".to_owned(),
            });
        }
        validate_qq_official_target(&intent, &self.qq_official_account_id)?;

        let fallback_text = intent
            .fallback_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(text);
        let message_type = intent.message_type.trim();
        let result = match intent.target.target_type {
            PushTargetType::Private => {
                send_private_push(&self.api, target_id, message_type, text, fallback_text).await
            }
            PushTargetType::Group => {
                send_group_push(&self.api, target_id, message_type, text, fallback_text).await
            }
        };
        match &result {
            Ok(_) => self.runtime.record_qq_send_success(),
            Err(err) => self.runtime.record_qq_send_failure(err.log_summary()),
        }
        match result {
            Ok(outcome) => Ok(self.record_successful_push(&intent, target_id, outcome)),
            Err(err) => {
                warn!(
                    platform = %intent.target.platform,
                    target_type = %intent.target.target_type.as_str(),
                    target = %mask_identifier(target_id),
                    error = %err.log_summary(),
                    "gateway push failed"
                );
                Err(PushError::Failed {
                    summary: err.log_summary(),
                })
            }
        }
    }

    fn record_successful_push(
        &self,
        intent: &PushIntent,
        target_id: &str,
        outcome: PushSendOutcome,
    ) -> PushResult {
        if intent.target.target_type == PushTargetType::Group {
            let mut cache = self.group_outbound_cache.lock().unwrap();
            cache.insert(outcome.ids.message_id.clone());
            cache.insert_ref_index_id(outcome.ids.ref_index_id.clone());
        }
        self.record_push_ref_index(intent, &outcome.ids, &outcome.delivered_text);
        info!(
            platform = %intent.target.platform,
            target_type = %intent.target.target_type.as_str(),
            target = %mask_identifier(target_id),
            "gateway push sent"
        );
        PushResult {
            message_id: outcome.ids.message_id,
        }
    }

    fn record_push_ref_index(
        &self,
        intent: &PushIntent,
        sent_ids: &SendMessageIds,
        delivered_text: &str,
    ) {
        let Some(ref_index_id) = sent_ids.ref_index_id.as_deref() else {
            return;
        };
        let conversation = match intent.target.target_type {
            PushTargetType::Private => ConversationTarget::Private {
                target_id: intent.target.target_id.clone(),
            },
            PushTargetType::Group => ConversationTarget::Group {
                target_id: intent.target.target_id.clone(),
            },
        };
        let mut ref_index = match self.ref_index.lock() {
            Ok(ref_index) => ref_index,
            Err(_) => {
                warn!(
                    target_type = %intent.target.target_type.as_str(),
                    target = %mask_identifier(&intent.target.target_id),
                    ref_index_id = %mask_identifier(ref_index_id),
                    "push ref_index write skipped because index lock is poisoned"
                );
                return;
            }
        };
        ref_index.insert_bot_outbound(
            crate::gateway::platform::Platform::QqOfficial,
            Some(&self.qq_official_account_id),
            &conversation,
            Some(ref_index_id.to_owned()),
            delivered_text,
            intent.visible_entity_snapshot.clone(),
        );
    }
}

fn validate_qq_official_target(
    intent: &PushIntent,
    qq_official_account_id: &str,
) -> Result<(), PushError> {
    let platform = intent.target.platform.trim();
    if platform != QQ_OFFICIAL_PLATFORM {
        let summary = if platform == "wechat_service" {
            "wechat_service proactive customer-service push is not available in this gateway sink"
                .to_owned()
        } else {
            format!("push platform `{platform}` is not supported by qq official gateway sink")
        };
        return Err(PushError::Failed { summary });
    }

    if let Some(account_id) = intent.target.account_id.as_deref().map(str::trim)
        && !account_id.is_empty()
        && account_id != qq_official_account_id.trim()
    {
        return Err(PushError::Failed {
            summary: "push target account does not match bound qq official account".to_owned(),
        });
    }
    Ok(())
}

async fn send_private_push<S: PushQqSender + ?Sized>(
    sender: &S,
    target_id: &str,
    message_type: &str,
    text: &str,
    fallback_text: &str,
) -> Result<PushSendOutcome, crate::api::ApiError> {
    match message_type {
        "markdown" => {
            let markdown = MarkdownPayload::new(text.to_owned());
            match sender.send_c2c_markdown(target_id, &markdown).await {
                Ok(ids) => Ok(PushSendOutcome {
                    ids,
                    delivered_text: text.to_owned(),
                }),
                Err(err) => {
                    warn!(
                        target = %mask_identifier(target_id),
                        error = %err.log_summary(),
                        "markdown push failed; falling back to text"
                    );
                    sender
                        .send_c2c_text(target_id, fallback_text)
                        .await
                        .map(|ids| PushSendOutcome {
                            ids,
                            delivered_text: fallback_text.to_owned(),
                        })
                }
            }
        }
        "text" | "" => {
            // 主动推送没有原始 QQ msg_id，因此只发送 content/msg_type/msg_seq。
            let _shape = build_c2c_text_payload(text, None, 1);
            sender
                .send_c2c_text(target_id, text)
                .await
                .map(|ids| PushSendOutcome {
                    ids,
                    delivered_text: text.to_owned(),
                })
        }
        _ => Err(crate::api::ApiError::Unsupported("message_type")),
    }
}

async fn send_group_push<S: PushQqSender + ?Sized>(
    sender: &S,
    target_id: &str,
    message_type: &str,
    text: &str,
    fallback_text: &str,
) -> Result<PushSendOutcome, crate::api::ApiError> {
    match message_type {
        "markdown" => {
            let markdown = MarkdownPayload::new(text.to_owned());
            match sender.send_group_markdown(target_id, &markdown).await {
                Ok(ids) => Ok(PushSendOutcome {
                    ids,
                    delivered_text: text.to_owned(),
                }),
                Err(err) => {
                    warn!(
                        target = %mask_identifier(target_id),
                        error = %err.log_summary(),
                        "group markdown push failed; falling back to text"
                    );
                    sender
                        .send_group_text(target_id, fallback_text)
                        .await
                        .map(|ids| PushSendOutcome {
                            ids,
                            delivered_text: fallback_text.to_owned(),
                        })
                }
            }
        }
        "text" | "" => {
            // QQ 群 openid 主动消息使用 /v2/groups/{group_openid}/messages。
            let _shape = build_group_text_payload(text, None, 1);
            sender
                .send_group_text(target_id, text)
                .await
                .map(|ids| PushSendOutcome {
                    ids,
                    delivered_text: text.to_owned(),
                })
        }
        _ => Err(crate::api::ApiError::Unsupported("message_type")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qq_maid_core::runtime::push::{PushTarget, PushTargetType};

    #[derive(Default)]
    struct MockPushSender {
        calls: Mutex<Vec<String>>,
        fail_markdown: bool,
        fail_text: bool,
        message_id: Option<String>,
        ref_index_id: Option<String>,
    }

    impl MockPushSender {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    struct MockOneBotSender {
        account_id: Option<String>,
        calls: Mutex<Vec<String>>,
        fail: bool,
    }

    impl MockOneBotSender {
        fn connected(account_id: &str) -> Self {
            Self {
                account_id: Some(account_id.to_owned()),
                calls: Mutex::new(Vec::new()),
                fail: false,
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PushOneBotSender for MockOneBotSender {
        fn connected_account_id(&self) -> Option<String> {
            self.account_id.clone()
        }

        async fn send_private_text(
            &self,
            target_id: &str,
            text: &str,
        ) -> Result<OneBotSendResult, OneBotSendError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("private:{target_id}:{text}"));
            if self.fail {
                Err(OneBotSendError::Transport(
                    crate::gateway::onebot11::OneBotCallError::ConnectionClosed,
                ))
            } else {
                Ok(OneBotSendResult {
                    message_id: "ob-private-1".to_owned(),
                })
            }
        }

        async fn send_group_text(
            &self,
            target_id: &str,
            text: &str,
        ) -> Result<OneBotSendResult, OneBotSendError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("group:{target_id}:{text}"));
            if self.fail {
                Err(OneBotSendError::Transport(
                    crate::gateway::onebot11::OneBotCallError::ConnectionClosed,
                ))
            } else {
                Ok(OneBotSendResult {
                    message_id: "ob-group-1".to_owned(),
                })
            }
        }
    }

    #[async_trait]
    impl PushQqSender for MockPushSender {
        async fn send_c2c_text(&self, target_id: &str, text: &str) -> SendResult {
            self.calls
                .lock()
                .unwrap()
                .push(format!("c2c-text:{target_id}:{text}"));
            if self.fail_text {
                Err(crate::api::ApiError::Unsupported("text"))
            } else {
                Ok(SendMessageIds {
                    message_id: self.message_id.clone(),
                    ref_index_id: self.ref_index_id.clone(),
                })
            }
        }

        async fn send_c2c_markdown(
            &self,
            target_id: &str,
            markdown: &MarkdownPayload,
        ) -> SendResult {
            self.calls
                .lock()
                .unwrap()
                .push(format!("c2c-markdown:{target_id}:{}", markdown.content));
            if self.fail_markdown {
                Err(crate::api::ApiError::Unsupported("markdown"))
            } else {
                Ok(SendMessageIds {
                    message_id: self.message_id.clone(),
                    ref_index_id: self.ref_index_id.clone(),
                })
            }
        }

        async fn send_group_text(&self, target_id: &str, text: &str) -> SendResult {
            self.calls
                .lock()
                .unwrap()
                .push(format!("group-text:{target_id}:{text}"));
            if self.fail_text {
                Err(crate::api::ApiError::Unsupported("text"))
            } else {
                Ok(SendMessageIds {
                    message_id: self.message_id.clone(),
                    ref_index_id: self.ref_index_id.clone(),
                })
            }
        }

        async fn send_group_markdown(
            &self,
            target_id: &str,
            markdown: &MarkdownPayload,
        ) -> SendResult {
            self.calls
                .lock()
                .unwrap()
                .push(format!("group-markdown:{target_id}:{}", markdown.content));
            if self.fail_markdown {
                Err(crate::api::ApiError::Unsupported("markdown"))
            } else {
                Ok(SendMessageIds {
                    message_id: self.message_id.clone(),
                    ref_index_id: self.ref_index_id.clone(),
                })
            }
        }
    }

    fn quoted_group_context(
        ref_index: &SharedRefIndex,
        group_id: &str,
        ref_id: &str,
    ) -> qq_maid_common::input_part::QuotedMessageContext {
        let mut quoted = crate::gateway::platform::InboundMessage {
            platform: crate::gateway::platform::Platform::QqOfficial,
            account_id: Some("app".to_owned()),
            conversation: ConversationTarget::Group {
                target_id: group_id.to_owned(),
            },
            actor: crate::gateway::platform::Actor {
                sender_id: Some("member-1".to_owned()),
                union_id: None,
                display_name: None,
                group_member_role: None,
                is_bot: false,
                source: qq_maid_common::identity_context::IdentitySource::Event,
            },
            visible_entity_snapshot: None,
            message_id: "gm-quote".to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: "继续".to_owned(),
            input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("继续")],
            attachments: Vec::new(),
            quoted: Some(qq_maid_common::input_part::QuotedMessageContext {
                ref_msg_idx: Some(ref_id.to_owned()),
                ..Default::default()
            }),
            mentions: Vec::new(),
            mentioned_bot: false,
        };
        ref_index.lock().unwrap().enrich_inbound(&mut quoted);
        quoted.quoted.unwrap()
    }

    fn quoted_onebot_context(
        ref_index: &SharedRefIndex,
        account_id: &str,
        conversation: ConversationTarget,
        ref_id: &str,
    ) -> qq_maid_common::input_part::QuotedMessageContext {
        let mut quoted = crate::gateway::platform::InboundMessage {
            platform: crate::gateway::platform::Platform::OneBot11,
            account_id: Some(account_id.to_owned()),
            conversation,
            actor: crate::gateway::platform::Actor {
                sender_id: Some("member-1".to_owned()),
                union_id: None,
                display_name: None,
                group_member_role: None,
                is_bot: false,
                source: qq_maid_common::identity_context::IdentitySource::Event,
            },
            visible_entity_snapshot: None,
            message_id: "onebot-quote".to_owned(),
            current_msg_idx: None,
            timestamp: None,
            text: "继续".to_owned(),
            input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("继续")],
            attachments: Vec::new(),
            quoted: Some(qq_maid_common::input_part::QuotedMessageContext {
                reference_id: Some(ref_id.to_owned()),
                ..Default::default()
            }),
            mentions: Vec::new(),
            mentioned_bot: false,
        };
        ref_index.lock().unwrap().enrich_inbound(&mut quoted);
        quoted.quoted.unwrap()
    }

    #[tokio::test]
    async fn unavailable_qq_channel_returns_immediate_explicit_error() {
        let sink = GatewayPushSink::unbound();
        sink.mark_qq_official_unavailable("QQ official channel is not bound");
        let intent = PushIntent {
            target: PushTarget::qq_official(PushTargetType::Private, "user-1"),
            text: "hello".to_owned(),
            fallback_text: None,
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        };

        let err = sink.push(intent).await.unwrap_err();

        assert!(err.to_string().contains("QQ official channel is not bound"));
    }

    #[tokio::test]
    async fn onebot_push_routes_independently_when_qq_is_unavailable() {
        let sink = GatewayPushSink::unbound();
        sink.mark_qq_official_unavailable("QQ official channel is not bound");
        let sender = Arc::new(MockOneBotSender::connected("bot-1"));
        sink.bind_onebot_sender(sender.clone(), crate::gateway::ref_index::ref_index());
        let intent = PushIntent {
            target: PushTarget::onebot11("bot-1", PushTargetType::Private, "user-1"),
            text: "# Markdown".to_owned(),
            fallback_text: Some("纯文本".to_owned()),
            message_type: "markdown".to_owned(),
            visible_entity_snapshot: None,
        };

        let result = sink.push(intent).await.unwrap();

        assert_eq!(result.message_id.as_deref(), Some("ob-private-1"));
        assert_eq!(sender.calls(), vec!["private:user-1:纯文本"]);
    }

    #[tokio::test]
    async fn onebot_group_push_routes_to_group_action_sender() {
        let sink = GatewayPushSink::unbound();
        let sender = Arc::new(MockOneBotSender::connected("bot-1"));
        sink.bind_onebot_sender(sender.clone(), crate::gateway::ref_index::ref_index());

        let result = sink
            .push(PushIntent {
                target: PushTarget::onebot11("bot-1", PushTargetType::Group, "group-1"),
                text: "group text".to_owned(),
                fallback_text: None,
                message_type: "text".to_owned(),
                visible_entity_snapshot: None,
            })
            .await
            .unwrap();

        assert_eq!(result.message_id.as_deref(), Some("ob-group-1"));
        assert_eq!(sender.calls(), vec!["group:group-1:group text"]);
    }

    #[tokio::test]
    async fn onebot_push_records_returned_message_id_in_ref_index() {
        let sink = GatewayPushSink::unbound();
        let sender = Arc::new(MockOneBotSender::connected("bot-1"));
        let ref_index = crate::gateway::ref_index::ref_index();
        sink.bind_onebot_sender(sender, ref_index.clone());

        sink.push(PushIntent {
            target: PushTarget::onebot11("bot-1", PushTargetType::Private, "user-1"),
            text: "提醒正文".to_owned(),
            fallback_text: None,
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        })
        .await
        .unwrap();

        let quoted = quoted_onebot_context(
            &ref_index,
            "bot-1",
            ConversationTarget::Private {
                target_id: "user-1".to_owned(),
            },
            "ob-private-1",
        );
        assert!(quoted.lookup_found);
        assert_eq!(quoted.text_summary.as_deref(), Some("提醒正文"));
        assert_eq!(quoted.from_bot, Some(true));
    }

    #[tokio::test]
    async fn onebot_push_rejects_offline_missing_or_wrong_account_without_sending() {
        let missing_account_sender = Arc::new(MockOneBotSender::connected("bot-1"));
        let runtime = OneBotPushRuntime {
            sender: missing_account_sender.clone(),
            ref_index: crate::gateway::ref_index::ref_index(),
        };
        let base = PushIntent {
            target: PushTarget::new(ONEBOT11_PLATFORM, None, PushTargetType::Group, "group-1"),
            text: "hello".to_owned(),
            fallback_text: None,
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        };
        let missing = runtime.push(base.clone()).await.unwrap_err();
        assert!(missing.to_string().contains("account_id is required"));

        let wrong = runtime
            .push(PushIntent {
                target: PushTarget::new(
                    ONEBOT11_PLATFORM,
                    Some("bot-2".to_owned()),
                    PushTargetType::Group,
                    "group-1",
                ),
                ..base.clone()
            })
            .await
            .unwrap_err();
        assert!(wrong.to_string().contains("does not match"));
        assert!(missing_account_sender.calls().is_empty());

        let offline = OneBotPushRuntime {
            sender: Arc::new(MockOneBotSender {
                account_id: None,
                calls: Mutex::new(Vec::new()),
                fail: false,
            }),
            ref_index: crate::gateway::ref_index::ref_index(),
        }
        .push(PushIntent {
            target: PushTarget::new(
                ONEBOT11_PLATFORM,
                Some("bot-1".to_owned()),
                PushTargetType::Group,
                "group-1",
            ),
            ..base
        })
        .await
        .unwrap_err();
        assert!(offline.to_string().contains("offline"));
    }

    #[tokio::test]
    async fn qq_target_never_falls_through_to_bound_onebot_sender() {
        let sink = GatewayPushSink::unbound();
        sink.mark_qq_official_unavailable("QQ official channel is not bound");
        let sender = Arc::new(MockOneBotSender::connected("bot-1"));
        sink.bind_onebot_sender(sender.clone(), crate::gateway::ref_index::ref_index());

        let err = sink
            .push(PushIntent {
                target: PushTarget::qq_official(PushTargetType::Private, "user-1"),
                text: "hello".to_owned(),
                fallback_text: None,
                message_type: "text".to_owned(),
                visible_entity_snapshot: None,
            })
            .await
            .unwrap_err();

        assert!(err.to_string().contains("QQ official channel is not bound"));
        assert!(sender.calls().is_empty());
    }

    #[tokio::test]
    async fn onebot_transport_failure_is_propagated_for_outbox_retry() {
        let sink = GatewayPushSink::unbound();
        sink.bind_onebot_sender(
            Arc::new(MockOneBotSender {
                account_id: Some("bot-1".to_owned()),
                calls: Mutex::new(Vec::new()),
                fail: true,
            }),
            crate::gateway::ref_index::ref_index(),
        );

        let err = sink
            .push(PushIntent {
                target: PushTarget::onebot11("bot-1", PushTargetType::Private, "user-1"),
                text: "hello".to_owned(),
                fallback_text: None,
                message_type: "text".to_owned(),
                visible_entity_snapshot: None,
            })
            .await
            .unwrap_err();

        assert!(err.to_string().contains("outbound queue is closed"));
    }

    #[tokio::test]
    async fn private_markdown_push_falls_back_to_text() {
        let sender = MockPushSender {
            fail_markdown: true,
            ..MockPushSender::default()
        };

        let outcome = send_private_push(&sender, "u1", "markdown", "# title", "title")
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec!["c2c-markdown:u1:# title", "c2c-text:u1:title"]
        );
        assert_eq!(outcome.delivered_text, "title");
    }

    #[tokio::test]
    async fn group_markdown_push_falls_back_to_text() {
        let sender = MockPushSender {
            fail_markdown: true,
            ..MockPushSender::default()
        };

        let outcome = send_group_push(&sender, "g1", "markdown", "# title", "title")
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec!["group-markdown:g1:# title", "group-text:g1:title"]
        );
        assert_eq!(outcome.delivered_text, "title");
    }

    #[tokio::test]
    async fn push_runtime_records_group_message_id_in_bot_outbound_cache() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let runtime = GatewayPushRuntime {
            api: panic_api_client(),
            qq_official_account_id: "app".to_owned(),
            runtime: GatewayRuntimeStatus::default(),
            group_outbound_cache: cache.clone(),
            ref_index: crate::gateway::ref_index::ref_index(),
        };
        let sender = MockPushSender {
            message_id: Some("bot-msg-1".to_owned()),
            ..MockPushSender::default()
        };

        let result = send_group_push(&sender, "g1", "text", "hello", "hello")
            .await
            .unwrap();
        // `GatewayPushRuntime::push` 的 QQ 发送成功路径会把群消息 ID 写入缓存；
        // 这里直接复用同一个缓存写入分支，证明主动推送仍能触发“回复机器人”识别。
        if let Some(message_id) = result.ids.message_id {
            runtime
                .group_outbound_cache
                .lock()
                .unwrap()
                .insert(Some(message_id));
        }

        assert!(
            cache.lock().unwrap().contains("bot-msg-1"),
            "group push message_id should be cached for reply detection"
        );
    }

    #[tokio::test]
    async fn group_push_cache_uses_message_id_and_ref_index_uses_refidx() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let ref_index = crate::gateway::ref_index::ref_index();
        let runtime = GatewayPushRuntime {
            api: panic_api_client(),
            qq_official_account_id: "app".to_owned(),
            runtime: GatewayRuntimeStatus::default(),
            group_outbound_cache: cache.clone(),
            ref_index: ref_index.clone(),
        };
        let intent = PushIntent {
            target: PushTarget::qq_official(PushTargetType::Group, "g1"),
            text: "RSS 推送正文".to_owned(),
            fallback_text: Some("RSS 推送正文".to_owned()),
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        };
        let sent_ids = SendMessageIds {
            message_id: Some("qq_msg_1".to_owned()),
            ref_index_id: Some("REFIDX_1".to_owned()),
        };

        let push_result = runtime.record_successful_push(
            &intent,
            "g1",
            PushSendOutcome {
                ids: sent_ids,
                delivered_text: "RSS 推送正文".to_owned(),
            },
        );

        assert_eq!(push_result.message_id.as_deref(), Some("qq_msg_1"));
        assert!(cache.lock().unwrap().contains("qq_msg_1"));
        assert!(!cache.lock().unwrap().contains("REFIDX_1"));

        let quoted = quoted_group_context(&ref_index, "g1", "REFIDX_1");
        assert!(quoted.lookup_found);
        assert_eq!(quoted.text_summary.as_deref(), Some("RSS 推送正文"));
        assert_eq!(quoted.from_bot, Some(true));
    }

    #[tokio::test]
    async fn group_markdown_push_success_ref_index_uses_delivered_markdown_text() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let ref_index = crate::gateway::ref_index::ref_index();
        let runtime = GatewayPushRuntime {
            api: panic_api_client(),
            qq_official_account_id: "app".to_owned(),
            runtime: GatewayRuntimeStatus::default(),
            group_outbound_cache: cache,
            ref_index: ref_index.clone(),
        };
        let sender = MockPushSender {
            message_id: Some("qq_md_msg".to_owned()),
            ref_index_id: Some("REFIDX_md".to_owned()),
            ..MockPushSender::default()
        };
        let intent = PushIntent {
            target: PushTarget::qq_official(PushTargetType::Group, "g1"),
            text: "# Markdown 标题".to_owned(),
            fallback_text: Some("Markdown 标题".to_owned()),
            message_type: "markdown".to_owned(),
            visible_entity_snapshot: None,
        };

        let outcome = send_group_push(
            &sender,
            "g1",
            "markdown",
            "# Markdown 标题",
            "Markdown 标题",
        )
        .await
        .unwrap();
        let push_result = runtime.record_successful_push(&intent, "g1", outcome);

        assert_eq!(sender.calls(), vec!["group-markdown:g1:# Markdown 标题"]);
        assert_eq!(push_result.message_id.as_deref(), Some("qq_md_msg"));
        let quoted = quoted_group_context(&ref_index, "g1", "REFIDX_md");
        assert!(quoted.lookup_found);
        assert_eq!(quoted.text_summary.as_deref(), Some("# Markdown 标题"));
    }

    #[tokio::test]
    async fn group_markdown_push_fallback_ref_index_uses_fallback_text() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let ref_index = crate::gateway::ref_index::ref_index();
        let runtime = GatewayPushRuntime {
            api: panic_api_client(),
            qq_official_account_id: "app".to_owned(),
            runtime: GatewayRuntimeStatus::default(),
            group_outbound_cache: cache,
            ref_index: ref_index.clone(),
        };
        let sender = MockPushSender {
            fail_markdown: true,
            message_id: Some("qq_fallback_msg".to_owned()),
            ref_index_id: Some("REFIDX_fallback".to_owned()),
            ..MockPushSender::default()
        };
        let intent = PushIntent {
            target: PushTarget::qq_official(PushTargetType::Group, "g1"),
            text: "# 失败的 Markdown".to_owned(),
            fallback_text: Some("降级文本".to_owned()),
            message_type: "markdown".to_owned(),
            visible_entity_snapshot: None,
        };

        let outcome = send_group_push(&sender, "g1", "markdown", "# 失败的 Markdown", "降级文本")
            .await
            .unwrap();
        let push_result = runtime.record_successful_push(&intent, "g1", outcome);

        assert_eq!(
            sender.calls(),
            vec![
                "group-markdown:g1:# 失败的 Markdown",
                "group-text:g1:降级文本"
            ]
        );
        assert_eq!(push_result.message_id.as_deref(), Some("qq_fallback_msg"));
        let quoted = quoted_group_context(&ref_index, "g1", "REFIDX_fallback");
        assert!(quoted.lookup_found);
        assert_eq!(quoted.text_summary.as_deref(), Some("降级文本"));
    }

    #[test]
    fn push_segment_outcomes_record_each_delivered_text_by_refidx() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let ref_index = crate::gateway::ref_index::ref_index();
        let runtime = GatewayPushRuntime {
            api: panic_api_client(),
            qq_official_account_id: "app".to_owned(),
            runtime: GatewayRuntimeStatus::default(),
            group_outbound_cache: cache.clone(),
            ref_index: ref_index.clone(),
        };
        let intent = PushIntent {
            target: PushTarget::qq_official(PushTargetType::Group, "g1"),
            text: "完整推送".to_owned(),
            fallback_text: Some("完整推送".to_owned()),
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        };

        let first = runtime.record_successful_push(
            &intent,
            "g1",
            PushSendOutcome {
                ids: SendMessageIds {
                    message_id: Some("qq_seg_1".to_owned()),
                    ref_index_id: Some("REFIDX_seg_1".to_owned()),
                },
                delivered_text: "第一段".to_owned(),
            },
        );
        let second = runtime.record_successful_push(
            &intent,
            "g1",
            PushSendOutcome {
                ids: SendMessageIds {
                    message_id: Some("qq_seg_2".to_owned()),
                    ref_index_id: Some("REFIDX_seg_2".to_owned()),
                },
                delivered_text: "第二段".to_owned(),
            },
        );

        assert_eq!(first.message_id.as_deref(), Some("qq_seg_1"));
        assert_eq!(second.message_id.as_deref(), Some("qq_seg_2"));
        assert!(cache.lock().unwrap().contains("qq_seg_1"));
        assert!(cache.lock().unwrap().contains("qq_seg_2"));
        assert!(!cache.lock().unwrap().contains("REFIDX_seg_1"));
        assert!(!cache.lock().unwrap().contains("REFIDX_seg_2"));
        assert_eq!(
            quoted_group_context(&ref_index, "g1", "REFIDX_seg_1")
                .text_summary
                .as_deref(),
            Some("第一段")
        );
        assert_eq!(
            quoted_group_context(&ref_index, "g1", "REFIDX_seg_2")
                .text_summary
                .as_deref(),
            Some("第二段")
        );
    }

    #[tokio::test]
    async fn todo_push_refidx_without_message_id_does_not_enter_group_cache() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let ref_index = crate::gateway::ref_index::ref_index();
        let runtime = GatewayPushRuntime {
            api: panic_api_client(),
            qq_official_account_id: "app".to_owned(),
            runtime: GatewayRuntimeStatus::default(),
            group_outbound_cache: cache.clone(),
            ref_index: ref_index.clone(),
        };
        let intent = PushIntent {
            target: PushTarget::qq_official(PushTargetType::Group, "g1"),
            text: "Todo 提醒正文".to_owned(),
            fallback_text: Some("Todo 提醒正文".to_owned()),
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        };
        let sent_ids = SendMessageIds {
            message_id: None,
            ref_index_id: Some("REFIDX_todo_only".to_owned()),
        };

        let push_result = runtime.record_successful_push(
            &intent,
            "g1",
            PushSendOutcome {
                ids: sent_ids,
                delivered_text: "Todo 提醒正文".to_owned(),
            },
        );

        assert_eq!(push_result.message_id, None);
        assert!(!cache.lock().unwrap().contains("REFIDX_todo_only"));
        let quoted = quoted_group_context(&ref_index, "g1", "REFIDX_todo_only");
        assert!(quoted.lookup_found);
        assert_eq!(quoted.text_summary.as_deref(), Some("Todo 提醒正文"));
    }

    #[tokio::test]
    async fn push_with_message_id_only_does_not_forge_ref_index_entry() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let ref_index = crate::gateway::ref_index::ref_index();
        let runtime = GatewayPushRuntime {
            api: panic_api_client(),
            qq_official_account_id: "app".to_owned(),
            runtime: GatewayRuntimeStatus::default(),
            group_outbound_cache: cache,
            ref_index: ref_index.clone(),
        };
        let intent = PushIntent {
            target: PushTarget::qq_official(PushTargetType::Group, "g1"),
            text: "只有 message_id 的推送".to_owned(),
            fallback_text: Some("只有 message_id 的推送".to_owned()),
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        };

        let push_result = runtime.record_successful_push(
            &intent,
            "g1",
            PushSendOutcome {
                ids: SendMessageIds {
                    message_id: Some("qq_msg_only".to_owned()),
                    ref_index_id: None,
                },
                delivered_text: "只有 message_id 的推送".to_owned(),
            },
        );
        assert_eq!(push_result.message_id.as_deref(), Some("qq_msg_only"));

        let quoted = quoted_group_context(&ref_index, "g1", "qq_msg_only");
        assert!(!quoted.lookup_found);
    }

    #[test]
    fn push_ref_index_write_failure_is_best_effort() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let ref_index = crate::gateway::ref_index::ref_index();
        let poisoned = ref_index.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("poison ref_index for test");
        });
        let runtime = GatewayPushRuntime {
            api: panic_api_client(),
            qq_official_account_id: "app".to_owned(),
            runtime: GatewayRuntimeStatus::default(),
            group_outbound_cache: cache,
            ref_index,
        };
        let intent = PushIntent {
            target: PushTarget::qq_official(PushTargetType::Group, "g1"),
            text: "推送正文".to_owned(),
            fallback_text: Some("推送正文".to_owned()),
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        };

        let push_result = runtime.record_successful_push(
            &intent,
            "g1",
            PushSendOutcome {
                ids: SendMessageIds {
                    message_id: Some("qq_msg_1".to_owned()),
                    ref_index_id: Some("REFIDX_poison".to_owned()),
                },
                delivered_text: "推送正文".to_owned(),
            },
        );
        assert_eq!(push_result.message_id.as_deref(), Some("qq_msg_1"));
    }

    #[tokio::test]
    async fn push_sink_error_is_propagated() {
        let sender = MockPushSender {
            fail_text: true,
            ..MockPushSender::default()
        };

        let err = send_private_push(&sender, "u1", "text", "hello", "hello")
            .await
            .unwrap_err();

        assert!(err.log_summary().contains("text sending is unsupported"));
    }

    #[test]
    fn push_intent_expresses_private_and_group_targets_without_http_metadata() {
        let private = PushIntent {
            target: PushTarget::qq_official(PushTargetType::Private, "u1"),
            text: "hello".to_owned(),
            fallback_text: Some("hello".to_owned()),
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        };
        let group = PushIntent {
            target: PushTarget::qq_official(PushTargetType::Group, "g1"),
            ..private.clone()
        };

        assert_eq!(private.target.platform, "qq_official");
        assert_eq!(private.target.target_type, PushTargetType::Private);
        assert_eq!(group.target.target_type, PushTargetType::Group);
        assert_eq!(private.message_type, "text");
    }

    #[test]
    fn qq_gateway_rejects_non_qq_push_target_before_sending() {
        let intent = PushIntent {
            target: PushTarget::new(
                "wechat_service",
                Some("gh_service".to_owned()),
                PushTargetType::Private,
                "user-openid",
            ),
            text: "hello".to_owned(),
            fallback_text: Some("hello".to_owned()),
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        };

        let err = validate_qq_official_target(&intent, "app").unwrap_err();

        assert!(err.to_string().contains("wechat_service proactive"));
    }

    #[test]
    fn qq_gateway_rejects_mismatched_qq_account() {
        let intent = PushIntent {
            target: PushTarget::new(
                "qq_official",
                Some("other-app".to_owned()),
                PushTargetType::Private,
                "u1",
            ),
            text: "hello".to_owned(),
            fallback_text: Some("hello".to_owned()),
            message_type: "text".to_owned(),
            visible_entity_snapshot: None,
        };

        let err = validate_qq_official_target(&intent, "app").unwrap_err();

        assert!(err.to_string().contains("target account"));
    }

    fn panic_api_client() -> QqApiClient {
        crate::api::QqApiClient::new(
            reqwest::Client::new(),
            "http://127.0.0.1",
            crate::auth::AccessTokenManager::new(
                reqwest::Client::new(),
                "app",
                "secret",
                Duration::from_secs(60),
            ),
        )
    }
}

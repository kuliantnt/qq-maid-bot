//! Gateway 进程内主动推送实现与平台路由。
//!
//! Core 只通过 `PushSink` 交付推送意图；本模块按 platform/account 精确选择 sender。
//! QQ 官方继续负责 Markdown fallback 和群消息缓存；OneBot 使用原生消息 segment。

mod mention;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use qq_maid_core::runtime::push::{
    ONEBOT11_PLATFORM, PushError, PushIntent, PushResult, PushSink, PushTargetType,
    QQ_OFFICIAL_PLATFORM, normalize_push_mentions,
};
use tokio::{sync::Notify, time::Instant};
use tracing::{info, warn};

use mention::{
    PreparedQqBot2Content, mention_display_names, partition_onebot_mentions,
    prepare_qq_bot2_content, prepend_mention_notice,
};

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
    async fn send_group_text_with_mentions(
        &self,
        target_id: &str,
        mention_user_ids: &[String],
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

    async fn send_group_text_with_mentions(
        &self,
        target_id: &str,
        mention_user_ids: &[String],
        text: &str,
    ) -> Result<OneBotSendResult, OneBotSendError> {
        OneBotSender::send_group_text_with_mentions(self, target_id, mention_user_ids, text).await
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
        let base_delivered_text = if matches!(intent.message_type.trim(), "" | "text") {
            text
        } else {
            fallback_text
        };
        let mentions = normalize_push_mentions(intent.mentions.clone());
        let (valid_mentions, invalid_mentions) = partition_onebot_mentions(&mentions);
        let degraded_names = mention_display_names(&invalid_mentions);
        let delivered_text = if intent.target.target_type == PushTargetType::Group {
            if !invalid_mentions.is_empty() {
                warn!(
                    platform = ONEBOT11_PLATFORM,
                    invalid_mention_count = invalid_mentions.len(),
                    "push mentions partially downgraded because OneBot member IDs are invalid"
                );
            }
            prepend_mention_notice(base_delivered_text, &degraded_names, false)
        } else {
            base_delivered_text.to_owned()
        };
        let result = match intent.target.target_type {
            PushTargetType::Private => {
                if !mentions.is_empty() {
                    warn!(
                        platform = ONEBOT11_PLATFORM,
                        mention_count = mentions.len(),
                        "push mentions ignored because private messages do not support group member mention"
                    );
                }
                self.sender
                    .send_private_text(target_id, &delivered_text)
                    .await
            }
            PushTargetType::Group if valid_mentions.is_empty() => {
                self.sender.send_group_text(target_id, &delivered_text).await
            }
            PushTargetType::Group => {
                self.sender
                    .send_group_text_with_mentions(target_id, &valid_mentions, &delivered_text)
                    .await
            }
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
                &delivered_text,
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
        let mentions = normalize_push_mentions(intent.mentions.clone());
        let prepared = prepare_qq_bot2_content(
            intent.target.target_type,
            &mentions,
            text,
            fallback_text,
            intent.message_type.trim(),
        );
        let message_type = intent.message_type.trim();
        let result = match intent.target.target_type {
            PushTargetType::Private => {
                send_private_push(
                    &self.api,
                    target_id,
                    message_type,
                    &prepared.content,
                    &prepared.fallback_content,
                )
                .await
            }
            PushTargetType::Group => {
                send_group_push(&self.api, target_id, message_type, &prepared).await
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
    prepared: &PreparedQqBot2Content,
) -> Result<PushSendOutcome, crate::api::ApiError> {
    match message_type {
        "markdown" => {
            let markdown = MarkdownPayload::new(prepared.content.clone());
            match sender.send_group_markdown(target_id, &markdown).await {
                Ok(ids) => Ok(PushSendOutcome {
                    ids,
                    delivered_text: prepared.ref_index_content.clone(),
                }),
                Err(err) => {
                    warn!(
                        target = %mask_identifier(target_id),
                        error = %err.log_summary(),
                        "group markdown push failed; falling back to text"
                    );
                    sender
                        .send_group_text(target_id, &prepared.fallback_content)
                        .await
                        .map(|ids| PushSendOutcome {
                            ids,
                            delivered_text: prepared.fallback_ref_index_content.clone(),
                        })
                }
            }
        }
        "text" | "" => {
            // QQ 群 openid 主动消息使用 /v2/groups/{group_openid}/messages。
            let _shape = build_group_text_payload(&prepared.content, None, 1);
            sender
                .send_group_text(target_id, &prepared.content)
                .await
                .map(|ids| PushSendOutcome {
                    ids,
                    delivered_text: prepared.ref_index_content.clone(),
                })
        }
        _ => Err(crate::api::ApiError::Unsupported("message_type")),
    }
}

#[cfg(test)]
mod tests;

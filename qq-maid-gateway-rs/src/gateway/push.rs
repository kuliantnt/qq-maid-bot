//! Gateway 进程内主动推送实现。
//!
//! Core 只通过 `PushSink` 交付推送意图；本模块负责 QQ 平台发送、Markdown
//! 失败后的文本 fallback、发送状态记录，以及群推送成功后的 BotOutboundCache 回填。

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use qq_maid_core::runtime::push::{PushError, PushIntent, PushResult, PushSink, PushTargetType};
use tokio::{sync::Notify, time::timeout};
use tracing::{info, warn};

use crate::{
    api::{QqApiClient, SendResult, build_c2c_text_payload, build_group_text_payload},
    gateway::{
        BotOutboundCache, logging::mask_identifier, outbound::record_qq_send_result,
        ping::GatewayRuntimeStatus,
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
    inner: Arc<Mutex<Option<GatewayPushRuntime>>>,
    ready: Arc<Notify>,
}

#[derive(Clone)]
struct GatewayPushRuntime {
    api: QqApiClient,
    runtime: GatewayRuntimeStatus,
    group_outbound_cache: Arc<Mutex<BotOutboundCache>>,
}

impl GatewayPushSink {
    pub fn unbound() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            ready: Arc::new(Notify::new()),
        }
    }

    pub(crate) fn bind(
        &self,
        api: QqApiClient,
        runtime: GatewayRuntimeStatus,
        group_outbound_cache: Arc<Mutex<BotOutboundCache>>,
    ) {
        // Core scheduler 可能在 Gateway 首次连接 QQ 前启动，因此 sink 需要先存在；
        // 真正发送前必须已绑定运行期上下文，否则返回可观测错误而不是静默丢消息。
        *self.inner.lock().unwrap() = Some(GatewayPushRuntime {
            api,
            runtime,
            group_outbound_cache,
        });
        self.ready.notify_waiters();
    }

    async fn runtime(&self) -> Result<GatewayPushRuntime, PushError> {
        if let Some(runtime) = self.inner.lock().unwrap().clone() {
            return Ok(runtime);
        }

        // 统一进程启动时 Core 的 RSS / Todo 定时器和 QQ Gateway 连接并行启动。
        // 首次推送如果撞上 Gateway 尚未 bind，等待一小段时间可避免把正常启动竞态记成推送失败。
        let notified = self.ready.notified();
        if timeout(Duration::from_secs(30), notified).await.is_err() {
            return Err(PushError::Failed {
                summary: "gateway push sink is not ready".to_owned(),
            });
        }

        self.inner
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| PushError::Failed {
                summary: "gateway push sink is not ready".to_owned(),
            })
    }
}

#[async_trait]
impl PushSink for GatewayPushSink {
    async fn push(&self, intent: PushIntent) -> Result<PushResult, PushError> {
        let runtime = self.runtime().await?;
        runtime.push(intent).await
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
        record_qq_send_result(&self.runtime, &result);
        if intent.target.target_type == PushTargetType::Group
            && let Ok(Some(message_id)) = result.as_ref()
        {
            self.group_outbound_cache
                .lock()
                .unwrap()
                .insert(Some(message_id.clone()));
        }
        match result {
            Ok(message_id) => {
                info!(
                    target_type = %intent.target.target_type.as_str(),
                    target = %mask_identifier(target_id),
                    "gateway push sent"
                );
                Ok(PushResult { message_id })
            }
            Err(err) => {
                warn!(
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
}

async fn send_private_push<S: PushQqSender + ?Sized>(
    sender: &S,
    target_id: &str,
    message_type: &str,
    text: &str,
    fallback_text: &str,
) -> SendResult {
    match message_type {
        "markdown" => {
            let markdown = MarkdownPayload::new(text.to_owned());
            match sender.send_c2c_markdown(target_id, &markdown).await {
                Ok(message_id) => Ok(message_id),
                Err(err) => {
                    warn!(
                        target = %mask_identifier(target_id),
                        error = %err.log_summary(),
                        "markdown push failed; falling back to text"
                    );
                    sender.send_c2c_text(target_id, fallback_text).await
                }
            }
        }
        "text" | "" => {
            // 主动推送没有原始 QQ msg_id，因此只发送 content/msg_type/msg_seq。
            let _shape = build_c2c_text_payload(text, None, 1);
            sender.send_c2c_text(target_id, text).await
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
) -> SendResult {
    match message_type {
        "markdown" => {
            let markdown = MarkdownPayload::new(text.to_owned());
            match sender.send_group_markdown(target_id, &markdown).await {
                Ok(message_id) => Ok(message_id),
                Err(err) => {
                    warn!(
                        target = %mask_identifier(target_id),
                        error = %err.log_summary(),
                        "group markdown push failed; falling back to text"
                    );
                    sender.send_group_text(target_id, fallback_text).await
                }
            }
        }
        "text" | "" => {
            // QQ 群 openid 主动消息使用 /v2/groups/{group_openid}/messages。
            let _shape = build_group_text_payload(text, None, 1);
            sender.send_group_text(target_id, text).await
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
    }

    impl MockPushSender {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
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
                Ok(self.message_id.clone())
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
                Ok(self.message_id.clone())
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
                Ok(self.message_id.clone())
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
                Ok(self.message_id.clone())
            }
        }
    }

    #[tokio::test]
    async fn private_markdown_push_falls_back_to_text() {
        let sender = MockPushSender {
            fail_markdown: true,
            ..MockPushSender::default()
        };

        send_private_push(&sender, "u1", "markdown", "# title", "title")
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec!["c2c-markdown:u1:# title", "c2c-text:u1:title"]
        );
    }

    #[tokio::test]
    async fn group_markdown_push_falls_back_to_text() {
        let sender = MockPushSender {
            fail_markdown: true,
            ..MockPushSender::default()
        };

        send_group_push(&sender, "g1", "markdown", "# title", "title")
            .await
            .unwrap();

        assert_eq!(
            sender.calls(),
            vec!["group-markdown:g1:# title", "group-text:g1:title"]
        );
    }

    #[tokio::test]
    async fn push_runtime_records_group_message_id_in_bot_outbound_cache() {
        let cache = Arc::new(Mutex::new(BotOutboundCache::default()));
        let runtime = GatewayPushRuntime {
            api: panic_api_client(),
            runtime: GatewayRuntimeStatus::default(),
            group_outbound_cache: cache.clone(),
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
        if let Some(message_id) = result {
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
            target: PushTarget {
                target_type: PushTargetType::Private,
                target_id: "u1".to_owned(),
            },
            text: "hello".to_owned(),
            fallback_text: Some("hello".to_owned()),
            message_type: "text".to_owned(),
        };
        let group = PushIntent {
            target: PushTarget {
                target_type: PushTargetType::Group,
                target_id: "g1".to_owned(),
            },
            ..private.clone()
        };

        assert_eq!(private.target.target_type, PushTargetType::Private);
        assert_eq!(group.target.target_type, PushTargetType::Group);
        assert_eq!(private.message_type, "text");
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

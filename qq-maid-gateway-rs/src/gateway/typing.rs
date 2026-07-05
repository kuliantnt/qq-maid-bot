//! QQ C2C 原生“正在输入”状态的请求级生命周期。
//!
//! 这里只发送平台状态，不生成聊天文本，也不参与 Core 的工具成功判定。

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use tokio::{task::JoinHandle, time::sleep};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{
    event::C2cMessage,
    logging::{mask_identifier, mask_openid},
};
use crate::{
    api::{QqApiClient, SendResult},
    config::AgentTypingConfig,
};

pub(super) type TypingSendFuture<'a> = Pin<Box<dyn Future<Output = SendResult> + Send + 'a>>;

pub(super) trait C2cTypingSender: Send + Sync {
    fn send_typing<'a>(
        &'a self,
        user_openid: &'a str,
        msg_id: Option<&'a str>,
    ) -> TypingSendFuture<'a>;
}

impl C2cTypingSender for QqApiClient {
    fn send_typing<'a>(
        &'a self,
        user_openid: &'a str,
        msg_id: Option<&'a str>,
    ) -> TypingSendFuture<'a> {
        Box::pin(async move { self.send_c2c_typing(user_openid, msg_id).await })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TypingStopReason {
    FirstFrame,
    FinalReply,
    RequestFailed,
    Timeout,
    Cancelled,
}

impl TypingStopReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::FirstFrame => "first_frame",
            Self::FinalReply => "final_reply",
            Self::RequestFailed => "request_failed",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug)]
pub(super) struct C2cTypingStatusGuard {
    token: CancellationToken,
    sent: Arc<AtomicBool>,
    stopped_flag: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
    masked_user: String,
    source_message_id: String,
    delay_ms: u128,
    started_at: Instant,
    #[cfg(test)]
    stop_reason: Arc<std::sync::Mutex<Option<TypingStopReason>>>,
}

impl C2cTypingStatusGuard {
    pub(super) fn schedule(
        config: &AgentTypingConfig,
        api: QqApiClient,
        message: &C2cMessage,
        transport: &'static str,
    ) -> Option<Self> {
        Self::schedule_with_sender(config, Arc::new(api), message, transport)
    }

    pub(super) fn schedule_with_sender(
        config: &AgentTypingConfig,
        sender: Arc<dyn C2cTypingSender>,
        message: &C2cMessage,
        transport: &'static str,
    ) -> Option<Self> {
        if !config.enabled {
            return None;
        }
        let token = CancellationToken::new();
        let task_token = token.clone();
        let sent = Arc::new(AtomicBool::new(false));
        let task_sent = sent.clone();
        let stopped_flag = Arc::new(AtomicBool::new(false));
        let task_stopped_flag = stopped_flag.clone();
        let delay = config.delay;
        let delay_ms = delay.as_millis();
        let user_openid = message.user_openid.clone();
        let msg_id = message.message_id.clone();
        let masked_user = mask_openid(&user_openid);
        let source_message_id = mask_identifier(&msg_id);
        let task_masked_user = masked_user.clone();
        let task_source_message_id = source_message_id.clone();
        let started_at = Instant::now();

        debug!(
            user = %masked_user,
            source_message_id = %source_message_id,
            transport,
            delay_ms,
            "typing_status_scheduled"
        );

        let task = tokio::spawn(async move {
            let delay_finished = tokio::select! {
                _ = task_token.cancelled() => {
                    debug!(
                        user = %task_masked_user,
                        source_message_id = %task_source_message_id,
                        transport,
                        delay_ms,
                        elapsed_ms = started_at.elapsed().as_millis(),
                        "typing_status_cancelled"
                    );
                    false
                }
                _ = sleep(delay) => {
                    true
                }
            };
            if !delay_finished || task_stopped_flag.load(Ordering::SeqCst) {
                return;
            }

            let result = tokio::select! {
                _ = task_token.cancelled() => {
                    debug!(
                        user = %task_masked_user,
                        source_message_id = %task_source_message_id,
                        transport,
                        delay_ms,
                        elapsed_ms = started_at.elapsed().as_millis(),
                        "typing_status_cancelled"
                    );
                    return;
                }
                result = sender.send_typing(&user_openid, Some(&msg_id)) => result,
            };
            if task_stopped_flag.load(Ordering::SeqCst) {
                return;
            }
            match result {
                Ok(_) => {
                    task_sent.store(true, Ordering::SeqCst);
                    debug!(
                        user = %task_masked_user,
                        source_message_id = %task_source_message_id,
                        transport,
                        delay_ms,
                        elapsed_ms = started_at.elapsed().as_millis(),
                        "typing_status_sent"
                    );
                }
                Err(error) => {
                    warn!(
                        user = %task_masked_user,
                        source_message_id = %task_source_message_id,
                        transport,
                        delay_ms,
                        elapsed_ms = started_at.elapsed().as_millis(),
                        error = %error.log_summary(),
                        "typing_status_send_failed"
                    );
                }
            }
        });

        Some(Self {
            token,
            sent,
            stopped_flag,
            task: Some(task),
            masked_user,
            source_message_id,
            delay_ms,
            started_at,
            #[cfg(test)]
            stop_reason: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    pub(super) fn stop(&mut self, reason: TypingStopReason) {
        if self.stopped_flag.swap(true, Ordering::SeqCst) {
            return;
        }
        self.token.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        #[cfg(test)]
        {
            *self.stop_reason.lock().unwrap() = Some(reason);
        }
        debug!(
            user = %self.masked_user,
            source_message_id = %self.source_message_id,
            delay_ms = self.delay_ms,
            elapsed_ms = self.started_at.elapsed().as_millis(),
            sent = self.sent.load(Ordering::SeqCst),
            stop_reason = reason.as_str(),
            "typing_status_stopped"
        );
    }

    #[cfg(test)]
    fn sent_for_test(&self) -> bool {
        self.sent.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn stop_reason_for_test(&self) -> Option<TypingStopReason> {
        *self.stop_reason.lock().unwrap()
    }

    #[cfg(test)]
    pub(super) fn stop_reason_probe_for_test(
        &self,
    ) -> Arc<std::sync::Mutex<Option<TypingStopReason>>> {
        self.stop_reason.clone()
    }
}

impl Drop for C2cTypingStatusGuard {
    fn drop(&mut self) {
        self.stopped_flag.store(true, Ordering::SeqCst);
        self.token.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiError;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        sync::{Notify, oneshot},
        time::{Duration, advance, pause},
    };

    #[derive(Default)]
    struct FakeTypingSender {
        calls: AtomicUsize,
        started: Notify,
        finish_tx: Mutex<Option<oneshot::Sender<SendResult>>>,
        finish_rx: Mutex<Option<oneshot::Receiver<SendResult>>>,
        requests: Mutex<Vec<(String, Option<String>)>>,
    }

    impl FakeTypingSender {
        fn ready(result: SendResult) -> Arc<Self> {
            let (tx, rx) = oneshot::channel();
            tx.send(result).ok();
            Arc::new(Self {
                finish_tx: Mutex::new(None),
                finish_rx: Mutex::new(Some(rx)),
                ..Self::default()
            })
        }

        fn blocked() -> Arc<Self> {
            let (tx, rx) = oneshot::channel();
            Arc::new(Self {
                finish_tx: Mutex::new(Some(tx)),
                finish_rx: Mutex::new(Some(rx)),
                ..Self::default()
            })
        }

        fn complete(&self, result: SendResult) {
            if let Some(tx) = self.finish_tx.lock().unwrap().take() {
                tx.send(result).ok();
            }
        }

        async fn wait_started(&self) {
            self.started.notified().await;
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn requests(&self) -> Vec<(String, Option<String>)> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl C2cTypingSender for FakeTypingSender {
        fn send_typing<'a>(
            &'a self,
            user_openid: &'a str,
            msg_id: Option<&'a str>,
        ) -> TypingSendFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.requests
                    .lock()
                    .unwrap()
                    .push((user_openid.to_owned(), msg_id.map(str::to_owned)));
                self.started.notify_one();
                let rx = self.finish_rx.lock().unwrap().take();
                match rx {
                    Some(rx) => rx
                        .await
                        .unwrap_or_else(|_| Err(ApiError::Unsupported("typing"))),
                    None => Ok(None),
                }
            })
        }
    }

    fn typing_config(delay: Duration) -> AgentTypingConfig {
        AgentTypingConfig {
            enabled: true,
            delay,
        }
    }

    fn c2c_message(id: &str, user: &str) -> C2cMessage {
        C2cMessage {
            message_id: id.to_owned(),
            current_msg_idx: None,
            event_id: Some(format!("event-{id}")),
            source_message_ids: vec![id.to_owned()],
            source_event_ids: vec![format!("event-{id}")],
            user_openid: user.to_owned(),
            content: "晚上好".to_owned(),
            reply: None,
            timestamp: None,
            first_message_timestamp: None,
            last_message_timestamp: None,
            input_parts: vec![qq_maid_common::input_part::MessageInputPart::text("晚上好")],
            attachments: Vec::new(),
        }
    }

    async fn yield_tasks() {
        for _ in 0..5 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn stop_before_delay_threshold_does_not_send_typing() {
        pause();
        let sender = FakeTypingSender::ready(Ok(None));
        let mut guard = C2cTypingStatusGuard::schedule_with_sender(
            &typing_config(Duration::from_millis(100)),
            sender.clone(),
            &c2c_message("msg-1", "user-1"),
            "test",
        )
        .unwrap();

        yield_tasks().await;
        advance(Duration::from_millis(99)).await;
        guard.stop(TypingStopReason::FinalReply);
        advance(Duration::from_millis(1)).await;
        yield_tasks().await;

        assert_eq!(sender.calls(), 0);
        assert!(!guard.sent_for_test());
    }

    #[tokio::test]
    async fn sends_typing_once_after_delay_threshold() {
        let sender = FakeTypingSender::ready(Ok(Some("typing-id".to_owned())));
        let guard = C2cTypingStatusGuard::schedule_with_sender(
            &typing_config(Duration::from_millis(1)),
            sender.clone(),
            &c2c_message("msg-1", "user-1"),
            "test",
        )
        .unwrap();

        sender.wait_started().await;
        yield_tasks().await;

        assert_eq!(sender.calls(), 1);
        assert!(guard.sent_for_test());
    }

    #[tokio::test]
    async fn stop_at_delay_boundary_does_not_record_late_typing_sent() {
        pause();
        let sender = FakeTypingSender::blocked();
        let mut guard = C2cTypingStatusGuard::schedule_with_sender(
            &typing_config(Duration::from_millis(100)),
            sender.clone(),
            &c2c_message("msg-1", "user-1"),
            "test",
        )
        .unwrap();

        yield_tasks().await;
        advance(Duration::from_millis(100)).await;
        guard.stop(TypingStopReason::FinalReply);
        sender.complete(Ok(Some("typing-id".to_owned())));
        yield_tasks().await;

        assert!(!guard.sent_for_test());
    }

    #[tokio::test]
    async fn stop_while_send_is_blocked_aborts_local_task() {
        pause();
        let sender = FakeTypingSender::blocked();
        let mut guard = C2cTypingStatusGuard::schedule_with_sender(
            &typing_config(Duration::from_millis(10)),
            sender.clone(),
            &c2c_message("msg-1", "user-1"),
            "test",
        )
        .unwrap();

        yield_tasks().await;
        advance(Duration::from_millis(10)).await;
        sender.wait_started().await;
        guard.stop(TypingStopReason::FirstFrame);
        sender.complete(Ok(Some("typing-id".to_owned())));
        yield_tasks().await;

        assert_eq!(sender.calls(), 1);
        assert!(!guard.sent_for_test());
    }

    #[tokio::test]
    async fn typing_send_failure_is_best_effort_and_not_retried() {
        let sender = FakeTypingSender::ready(Err(ApiError::Unsupported("typing")));
        let guard = C2cTypingStatusGuard::schedule_with_sender(
            &typing_config(Duration::from_millis(1)),
            sender.clone(),
            &c2c_message("msg-1", "user-1"),
            "test",
        )
        .unwrap();

        sender.wait_started().await;
        yield_tasks().await;

        assert_eq!(sender.calls(), 1);
        assert!(!guard.sent_for_test());
    }

    #[tokio::test]
    async fn repeated_stop_is_idempotent() {
        pause();
        let sender = FakeTypingSender::blocked();
        let mut guard = C2cTypingStatusGuard::schedule_with_sender(
            &typing_config(Duration::from_millis(100)),
            sender.clone(),
            &c2c_message("msg-1", "user-1"),
            "test",
        )
        .unwrap();

        yield_tasks().await;
        guard.stop(TypingStopReason::FinalReply);
        guard.stop(TypingStopReason::RequestFailed);
        advance(Duration::from_millis(100)).await;
        yield_tasks().await;

        assert_eq!(sender.calls(), 0);
        assert_eq!(
            guard.stop_reason_for_test(),
            Some(TypingStopReason::FinalReply)
        );
    }

    #[tokio::test]
    async fn different_source_message_ids_are_isolated() {
        let sender = FakeTypingSender::ready(Ok(None));
        let mut first = C2cTypingStatusGuard::schedule_with_sender(
            &typing_config(Duration::from_secs(60)),
            sender.clone(),
            &c2c_message("msg-1", "user-1"),
            "test",
        )
        .unwrap();
        first.stop(TypingStopReason::FinalReply);
        let second = C2cTypingStatusGuard::schedule_with_sender(
            &typing_config(Duration::from_millis(1)),
            sender.clone(),
            &c2c_message("msg-2", "user-1"),
            "test",
        )
        .unwrap();

        sender.wait_started().await;
        yield_tasks().await;

        assert_eq!(sender.calls(), 1);
        assert_eq!(
            sender.requests(),
            vec![("user-1".to_owned(), Some("msg-2".to_owned()))]
        );
        assert!(!first.sent_for_test());
        assert!(second.sent_for_test());
    }

    #[tokio::test]
    async fn drop_before_delay_cleans_task_without_late_typing() {
        pause();
        let sender = FakeTypingSender::ready(Ok(None));
        let guard = C2cTypingStatusGuard::schedule_with_sender(
            &typing_config(Duration::from_millis(100)),
            sender.clone(),
            &c2c_message("msg-1", "user-1"),
            "test",
        )
        .unwrap();

        yield_tasks().await;
        drop(guard);
        advance(Duration::from_millis(100)).await;
        yield_tasks().await;

        assert_eq!(sender.calls(), 0);
    }
}

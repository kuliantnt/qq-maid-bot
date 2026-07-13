//! 活动连接、echo 关联和后续 API sender 共用的 transport 上下文。

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde_json::Value;
use thiserror::Error;
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::protocol::{ActionRequest, ActionResponse, Echo, OneBotId};

#[derive(Debug, Error)]
pub enum OneBotCallError {
    #[error("OneBot client is not connected")]
    NotConnected,
    #[error("OneBot connection outbound queue is closed")]
    ConnectionClosed,
    #[error("OneBot API request timed out")]
    Timeout,
    #[error("failed to encode OneBot API request: {0}")]
    Encode(#[from] serde_json::Error),
}

#[derive(Clone)]
pub struct OneBotConnectionContext {
    state: Arc<Mutex<ConnectionState>>,
    pending: Arc<Mutex<HashMap<String, PendingRequest>>>,
    next_echo: Arc<AtomicU64>,
    request_timeout: Duration,
}

struct PendingRequest {
    generation: u64,
    response: oneshot::Sender<ActionResponse>,
}

#[derive(Default)]
struct ConnectionState {
    expected_self_id: Option<OneBotId>,
    generation: u64,
    active: Option<ActiveConnection>,
}

struct ActiveConnection {
    generation: u64,
    outbound: mpsc::Sender<String>,
    replaced: CancellationToken,
}

pub(super) struct Registration {
    pub(super) generation: u64,
    pub(super) replaced_existing: bool,
}

#[derive(Debug)]
pub(super) enum RegistrationError {
    AccountMismatch { expected: OneBotId },
    StateUnavailable,
}

impl OneBotConnectionContext {
    pub fn new(request_timeout: Duration) -> Self {
        Self {
            state: Arc::new(Mutex::new(ConnectionState::default())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_echo: Arc::new(AtomicU64::new(1)),
            request_timeout,
        }
    }

    /// 通过当前活动 WebSocket 调用 OneBot action。具体 sender 后续只需封装 action/params，
    /// 不应创建第二条 transport 或自行维护 echo 表。
    pub async fn call(
        &self,
        action: impl Into<String>,
        params: Value,
    ) -> Result<ActionResponse, OneBotCallError> {
        // request_timeout 是调用方看到的完整 API 调用预算，必须同时覆盖发送队列等待
        // 与响应等待，避免 outbound 拥塞时永远卡在进入 response timeout 之前。
        let deadline = Instant::now() + self.request_timeout;
        let echo = format!(
            "qq-maid-onebot-{}",
            self.next_echo.fetch_add(1, Ordering::Relaxed)
        );
        let request = ActionRequest {
            action: action.into(),
            params,
            echo: Echo(Value::String(echo.clone())),
        };
        let payload = serde_json::to_string(&request)?;
        let (generation, outbound) = self
            .state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .active
                    .as_ref()
                    .map(|active| (active.generation, active.outbound.clone()))
            })
            .ok_or(OneBotCallError::NotConnected)?;
        let (response_tx, response_rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| OneBotCallError::ConnectionClosed)?
            .insert(
                echo.clone(),
                PendingRequest {
                    generation,
                    response: response_tx,
                },
            );
        let still_current = self.state.lock().ok().is_some_and(|state| {
            state
                .active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
        });
        if !still_current {
            self.remove_pending(&echo, generation);
            return Err(OneBotCallError::ConnectionClosed);
        }
        let result = tokio::time::timeout_at(deadline, async {
            outbound
                .send(payload)
                .await
                .map_err(|_| OneBotCallError::ConnectionClosed)?;
            response_rx
                .await
                .map_err(|_| OneBotCallError::ConnectionClosed)
        })
        .await;
        match result {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) => {
                self.remove_pending(&echo, generation);
                Err(error)
            }
            Err(_) => {
                self.remove_pending(&echo, generation);
                Err(OneBotCallError::Timeout)
            }
        }
    }

    pub fn connected_self_id(&self) -> Option<OneBotId> {
        self.state.lock().ok().and_then(|state| {
            state
                .active
                .as_ref()
                .and(state.expected_self_id.as_ref())
                .cloned()
        })
    }

    pub(super) fn register(
        &self,
        self_id: OneBotId,
        outbound: mpsc::Sender<String>,
        replaced: CancellationToken,
    ) -> Result<Registration, RegistrationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| RegistrationError::StateUnavailable)?;
        if let Some(expected) = state.expected_self_id.as_ref()
            && expected != &self_id
        {
            return Err(RegistrationError::AccountMismatch {
                expected: expected.clone(),
            });
        }
        if state.expected_self_id.is_none() {
            state.expected_self_id = Some(self_id);
        }
        let replaced_generation = state.active.take().map(|active| {
            let generation = active.generation;
            active.replaced.cancel();
            generation
        });
        let replaced_existing = replaced_generation.is_some();
        state.generation = state.generation.wrapping_add(1).max(1);
        let generation = state.generation;
        state.active = Some(ActiveConnection {
            generation,
            outbound,
            replaced,
        });
        if let Some(replaced_generation) = replaced_generation {
            // 保持 state 锁直到旧 generation 的 pending 清理完成：新调用此时还无法取得
            // 新 generation，因而不会在替换清理与新 active 发布之间被误取消。
            self.cancel_pending_generation(replaced_generation);
        }
        Ok(Registration {
            generation,
            replaced_existing,
        })
    }

    pub(super) fn unregister(&self, generation: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let owns_active = state
            .active
            .as_ref()
            .is_some_and(|active| active.generation == generation);
        if owns_active {
            state.active = None;
            self.cancel_pending_generation(generation);
        }
        owns_active
    }

    pub(super) fn dispatch_response(&self, generation: u64, response: ActionResponse) {
        let Some(Echo(Value::String(echo))) = response.echo.as_ref() else {
            debug!("ignoring OneBot API response without string echo");
            return;
        };
        let Ok(state) = self.state.lock() else {
            return;
        };
        let is_current = state
            .active
            .as_ref()
            .is_some_and(|active| active.generation == generation);
        if !is_current {
            debug!(
                generation,
                "ignoring OneBot API response from stale connection"
            );
            return;
        }
        let sender = self.pending.lock().ok().and_then(|mut pending| {
            let belongs_to_generation = pending
                .get(echo)
                .is_some_and(|request| request.generation == generation);
            belongs_to_generation
                .then(|| pending.remove(echo))
                .flatten()
                .map(|request| request.response)
        });
        drop(state);
        if let Some(sender) = sender {
            let _ = sender.send(response);
        }
    }

    fn remove_pending(&self, echo: &str, generation: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            let belongs_to_generation = pending
                .get(echo)
                .is_some_and(|request| request.generation == generation);
            if belongs_to_generation {
                pending.remove(echo);
            }
        }
    }

    fn cancel_pending_generation(&self, generation: u64) {
        if let Ok(mut pending) = self.pending.lock() {
            // 旧连接上的 action 不可能由新连接可靠完成；只释放旧 generation 的等待方，
            // 不能影响替换后已经创建的新请求。
            pending.retain(|_, request| request.generation != generation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_cleanup_only_cancels_old_generation_pending() {
        let context = OneBotConnectionContext::new(Duration::from_secs(1));
        let (old_tx, mut old_rx) = oneshot::channel();
        let (current_tx, mut current_rx) = oneshot::channel();
        context.pending.lock().unwrap().insert(
            "old".to_owned(),
            PendingRequest {
                generation: 1,
                response: old_tx,
            },
        );
        context.pending.lock().unwrap().insert(
            "current".to_owned(),
            PendingRequest {
                generation: 2,
                response: current_tx,
            },
        );

        context.cancel_pending_generation(1);

        assert!(matches!(
            old_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
        assert!(matches!(
            current_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let pending = context.pending.lock().unwrap();
        assert!(!pending.contains_key("old"));
        assert!(pending.contains_key("current"));
    }
}

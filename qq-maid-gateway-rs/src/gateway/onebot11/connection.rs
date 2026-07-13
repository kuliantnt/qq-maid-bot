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
use tokio::sync::{mpsc, oneshot};
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
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<ActionResponse>>>>,
    next_echo: Arc<AtomicU64>,
    request_timeout: Duration,
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
            .insert(echo.clone(), response_tx);
        let still_current = self.state.lock().ok().is_some_and(|state| {
            state
                .active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
        });
        if !still_current {
            self.remove_pending(&echo);
            return Err(OneBotCallError::ConnectionClosed);
        }
        if outbound.send(payload).await.is_err() {
            self.remove_pending(&echo);
            return Err(OneBotCallError::ConnectionClosed);
        }
        match tokio::time::timeout(self.request_timeout, response_rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(OneBotCallError::ConnectionClosed),
            Err(_) => {
                self.remove_pending(&echo);
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
        let replaced_existing = state.active.is_some();
        if let Some(active) = state.active.take() {
            active.replaced.cancel();
        }
        state.generation = state.generation.wrapping_add(1).max(1);
        let generation = state.generation;
        state.active = Some(ActiveConnection {
            generation,
            outbound,
            replaced,
        });
        drop(state);
        if replaced_existing && let Ok(mut pending) = self.pending.lock() {
            // 旧连接上的 action 不可能由新连接可靠完成；立刻释放等待方，避免全部拖到超时。
            pending.clear();
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
            if let Ok(mut pending) = self.pending.lock() {
                pending.clear();
            }
        }
        owns_active
    }

    pub(super) fn dispatch_response(&self, response: ActionResponse) {
        let Some(Echo(Value::String(echo))) = response.echo.as_ref() else {
            debug!("ignoring OneBot API response without string echo");
            return;
        };
        if let Some(sender) = self
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(echo))
        {
            let _ = sender.send(response);
        }
    }

    fn remove_pending(&self, echo: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(echo);
        }
    }
}

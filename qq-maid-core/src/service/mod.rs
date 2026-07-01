//! Core 进程内服务契约。
//!
//! Gateway 只依赖本模块暴露的强类型边界，不直接访问 Core 内部 store、HTTP
//! route 或 provider 细节。scope_key 统一由 Core 根据会话目标派生，避免跨层出现
//! 两套会话归属事实。

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use tokio::{sync::mpsc, time::timeout};
use tracing::{error, warn};

use crate::{
    config::AppConfig,
    error::{ErrorInfo, LlmError},
    http::routes::AppState,
    provider::types::{ChatMessage, ChatRequest, ChatRole},
    runtime::respond::{
        RespondExecutors, RespondPlan, RespondRequest, RespondResponse, RespondServiceOptions,
        RespondStores, RustRespondService,
    },
    util::metrics::MetricsRecorder,
};
use qq_maid_common::{
    redaction::redact_sensitive_text, text::truncate_chars_with_ellipsis_trimmed,
};

pub use qq_maid_llm::provider::status::{UpstreamState, UpstreamStatusSnapshot};

#[async_trait]
pub trait CoreService: Send + Sync {
    async fn respond(&self, request: CoreRequest) -> Result<CoreRespondOutput, CoreError>;

    async fn classify_inbound(
        &self,
        request: CoreRequest,
    ) -> Result<CoreInboundClassification, CoreError>;

    async fn upstream_check(&self) -> Result<(), CoreError>;

    fn health_snapshot(&self) -> CoreHealthSnapshot;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreRequest {
    pub text: String,
    pub platform: Platform,
    pub actor: CoreActor,
    pub conversation: CoreConversation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreInboundClassification {
    pub kind: CoreInboundKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreInboundKind {
    NormalChat,
    Immediate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    QqOfficial,
    OneBot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreActor {
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreConversation {
    Private { peer_id: String },
    Group { group_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreResponse {
    pub text: Option<String>,
    pub markdown: Option<String>,
    pub handled: Option<bool>,
    pub session_id: Option<String>,
    pub command: Option<String>,
    pub diagnostics: Option<serde_json::Value>,
}

#[derive(Debug)]
pub enum CoreRespondOutput {
    Complete(CoreResponse),
    Stream(CoreResponseStream),
}

#[derive(Debug)]
pub struct CoreResponseStream {
    receiver: mpsc::Receiver<CoreResponseEvent>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreResponseEvent {
    /// 用户可见的最终文本增量。
    ///
    /// Tool Loop 路径只会在工具循环完成、业务校验通过并生成最终回复后发送该事件；
    /// 工具参数、工具结果原文和模型中间候选文本不得通过此事件外发。
    TextDelta(String),
    Completed(CoreResponse),
    Failed(CoreRespondFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreRespondFailure {
    pub kind: CoreFailureKind,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreFailureKind {
    SearchTimeout,
    SearchFailed,
    LlmTimeout,
    LlmFailed,
    Cancelled,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreHealthSnapshot {
    pub ok: bool,
    pub provider: String,
    pub model: String,
    pub stream: bool,
    pub upstream: UpstreamStatusSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}@{stage}: {message}")]
pub struct CoreError {
    pub code: String,
    pub stage: String,
    pub message: String,
}

#[derive(Clone)]
pub struct CoreHandle {
    state: Arc<AppState>,
}

impl CoreHandle {
    pub fn new(state: AppState) -> Self {
        Self {
            state: Arc::new(state),
        }
    }

    fn respond_service(&self) -> RustRespondService {
        let state = self.state.as_ref();
        RustRespondService::new(
            state.provider.clone(),
            RespondExecutors {
                query_executor: state.query_executor.clone(),
                weather_executor: state.weather_executor.clone(),
                train_executor: state.train_executor.clone(),
            },
            RespondStores {
                memory_store: state.memory_store.clone(),
                session_store: state.session_store.clone(),
                todo_store: state.todo_store.clone(),
                rss_store: state.rss_store.clone(),
            },
            state.rss_fetcher.clone(),
            state.knowledge_index.clone(),
            state.prompt_config.clone(),
            respond_options(&state.config),
        )
    }
}

#[async_trait]
impl CoreService for CoreHandle {
    async fn respond(&self, request: CoreRequest) -> Result<CoreRespondOutput, CoreError> {
        let req: RespondRequest = request.into();
        let service = self.respond_service();
        let recorder = MetricsRecorder::start();
        let scope_key = req.scope_key.clone();
        let state = self.state.as_ref();
        let respond_plan = service.plan_core_respond(&req).map_err(CoreError::from)?;
        if matches!(
            respond_plan,
            RespondPlan::StreamingChat | RespondPlan::CompleteToolLoop
        ) {
            let provider_stream_enabled = state.provider.stream_enabled();
            let result = timeout(
                Duration::from_secs(state.config.request_timeout_seconds),
                async {
                    Ok::<_, LlmError>(start_core_response_stream(
                        service,
                        req,
                        respond_plan,
                        provider_stream_enabled,
                        Duration::from_secs(state.config.request_timeout_seconds),
                    ))
                },
            )
            .await;
            return match result {
                Ok(Ok(stream)) => Ok(CoreRespondOutput::Stream(stream)),
                Ok(Err(err)) => {
                    warn_core_error(&scope_key, &err);
                    Err(err.into())
                }
                Err(_) => {
                    let err = LlmError::timeout("stream_init");
                    error_core_error(&scope_key, &err);
                    let _metrics = recorder.fail(
                        state.provider.name(),
                        state.provider.model(),
                        state.provider.stream_enabled(),
                    );
                    Err(err.into())
                }
            };
        }
        let result = timeout(
            Duration::from_secs(state.config.request_timeout_seconds),
            service.respond_with_plan(req, respond_plan),
        )
        .await;

        match result {
            Ok(Ok(response)) if response.ok => Ok(CoreRespondOutput::Complete(response.into())),
            Ok(Ok(response)) => {
                let err = response.error.map(CoreError::from).unwrap_or_else(|| {
                    CoreError::new("internal_error", "respond", "处理失败，请稍后再试")
                });
                warn!(
                    scope_key,
                    error_code = err.code,
                    error_stage = err.stage,
                    "core respond returned business error"
                );
                Err(err)
            }
            Ok(Err(err)) => {
                warn_core_error(&scope_key, &err);
                Err(err.into())
            }
            Err(_) => {
                let err = LlmError::timeout("request");
                error_core_error(&scope_key, &err);
                let _metrics = recorder.fail(
                    state.provider.name(),
                    state.provider.model(),
                    state.provider.stream_enabled(),
                );
                Err(err.into())
            }
        }
    }

    async fn classify_inbound(
        &self,
        request: CoreRequest,
    ) -> Result<CoreInboundClassification, CoreError> {
        let req: RespondRequest = request.into();
        let service = self.respond_service();
        service.classify_inbound(req).map_err(CoreError::from)
    }

    async fn upstream_check(&self) -> Result<(), CoreError> {
        let state = self.state.as_ref();
        let request = ChatRequest {
            session_id: "diagnostic:upstream_check".to_owned(),
            model: None,
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "这是连通性检查。请只回复 OK。".to_owned(),
            }],
            context_budget: None,
            metadata: HashMap::from([("purpose".to_owned(), "upstream_check".to_owned())]),
        };

        match timeout(
            Duration::from_secs(state.config.request_timeout_seconds),
            state.provider.chat(request),
        )
        .await
        {
            Ok(Ok(outcome)) if !outcome.reply.trim().is_empty() => Ok(()),
            Ok(Ok(_)) => {
                let error = LlmError::provider("upstream returned empty response", "diagnostic");
                // 空正文不能证明响应解析可用，必须显式覆盖为失败状态。
                state.upstream_status.record_failure(&error);
                Err(CoreError::new(
                    "provider_error",
                    "diagnostic",
                    "上游返回空响应",
                ))
            }
            Ok(Err(error)) => Err(error.into()),
            Err(_) => {
                let error = LlmError::timeout("upstream_check");
                // timeout 会取消被观测 provider 的 future，因此在入口补记失败状态。
                state.upstream_status.record_failure(&error);
                Err(error.into())
            }
        }
    }

    fn health_snapshot(&self) -> CoreHealthSnapshot {
        let state = self.state.as_ref();
        CoreHealthSnapshot {
            ok: true,
            provider: state.provider.name().to_owned(),
            model: state.provider.model().to_owned(),
            stream: state.provider.stream_enabled(),
            upstream: state.upstream_status.snapshot(),
        }
    }
}

impl CoreResponseStream {
    pub async fn recv(&mut self) -> Option<CoreResponseEvent> {
        self.receiver.recv().await
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl Drop for CoreResponseStream {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn start_core_response_stream(
    service: RustRespondService,
    req: RespondRequest,
    plan: RespondPlan,
    provider_stream_enabled: bool,
    request_timeout: Duration,
) -> CoreResponseStream {
    let (tx, receiver) = mpsc::channel(16);
    let cancelled = Arc::new(AtomicBool::new(false));
    let producer_cancelled = cancelled.clone();
    let scope_key = req.scope_key.clone();
    tokio::spawn(async move {
        if producer_cancelled.load(Ordering::SeqCst) {
            let _ = tx
                .send(CoreResponseEvent::Failed(CoreRespondFailure::cancelled()))
                .await;
            return;
        }
        let respond_future = run_streaming_respond(
            &service,
            req,
            plan,
            tx.clone(),
            producer_cancelled.clone(),
            provider_stream_enabled,
        );
        let result = if matches!(plan, RespondPlan::CompleteToolLoop) {
            match timeout(request_timeout, respond_future).await {
                Ok(result) => result,
                Err(_) => Err(LlmError::timeout("request")),
            }
        } else {
            respond_future.await
        };
        if producer_cancelled.load(Ordering::SeqCst) {
            return;
        }
        let event = match result {
            Ok(response) if response.ok => CoreResponseEvent::Completed(response.into()),
            Ok(response) => {
                let err = response.error.map(CoreError::from).unwrap_or_else(|| {
                    CoreError::new("internal_error", "respond", "处理失败，请稍后再试")
                });
                tracing::warn!(
                    scope_key,
                    error_code = err.code,
                    error_stage = err.stage,
                    "streaming core respond returned business error"
                );
                CoreResponseEvent::Failed(CoreRespondFailure::from_core_error(&err))
            }
            Err(err) => {
                warn_core_error(&scope_key, &err);
                CoreResponseEvent::Failed(CoreRespondFailure::from_llm_error(&err))
            }
        };
        if !producer_cancelled.load(Ordering::SeqCst) {
            let _ = tx.send(event).await;
        }
    });
    CoreResponseStream {
        receiver,
        cancelled,
    }
}

async fn run_streaming_respond(
    service: &RustRespondService,
    req: RespondRequest,
    plan: RespondPlan,
    tx: mpsc::Sender<CoreResponseEvent>,
    cancelled: Arc<AtomicBool>,
    provider_stream_enabled: bool,
) -> Result<RespondResponse, LlmError> {
    if matches!(plan, RespondPlan::CompleteToolLoop) || !provider_stream_enabled {
        let response = service.respond_with_plan(req, plan).await?;
        send_final_response_delta(&tx, &cancelled, &response).await?;
        return Ok(response);
    }
    service
        .respond_stream(req, |delta| {
            let tx = tx.clone();
            let cancelled = cancelled.clone();
            Box::pin(async move { send_core_delta(&tx, &cancelled, delta).await })
        })
        .await
}

async fn send_final_response_delta(
    tx: &mpsc::Sender<CoreResponseEvent>,
    cancelled: &Arc<AtomicBool>,
    response: &RespondResponse,
) -> Result<(), LlmError> {
    // 非 SSE 或 Tool Loop 路径只在完整响应完成后合成用户可见增量。
    // 对 Tool Loop 来说，这也是最终可信回复的边界：此时工具副作用、Todo 成功校验、
    // session/history 和 diagnostics 均已由 RespondService 完成提交。
    if response.ok
        && let Some(delta) = response
            .markdown
            .as_deref()
            .or(response.text.as_deref())
            .map(str::to_owned)
            .filter(|value| !value.trim().is_empty())
    {
        send_core_delta(tx, cancelled, delta).await?;
    }
    Ok(())
}

async fn send_core_delta(
    tx: &mpsc::Sender<CoreResponseEvent>,
    cancelled: &Arc<AtomicBool>,
    delta: String,
) -> Result<(), LlmError> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(LlmError::new("cancelled", "stream cancelled", "stream"));
    }
    tx.send(CoreResponseEvent::TextDelta(delta))
        .await
        .map_err(|_| LlmError::new("cancelled", "stream receiver dropped", "stream"))
}

impl CoreRequest {
    pub fn scope_key(&self) -> String {
        match &self.conversation {
            CoreConversation::Private { peer_id } => format!("private:{peer_id}"),
            CoreConversation::Group { group_id } => format!("group:{group_id}"),
        }
    }
}

impl From<CoreRequest> for RespondRequest {
    fn from(value: CoreRequest) -> Self {
        let scope_key = value.scope_key();
        let (group_id, channel_id, event_type) = match &value.conversation {
            CoreConversation::Private { .. } => (None, None, "c2c_message"),
            CoreConversation::Group { group_id } => (Some(group_id.clone()), None, "group_message"),
        };
        Self {
            content: value.text,
            scope_key,
            user_id: value.actor.user_id,
            group_id,
            guild_id: None,
            channel_id,
            platform: value.platform.as_str().to_owned(),
            event_type: event_type.to_owned(),
            ..Default::default()
        }
    }
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QqOfficial => "qq_official",
            Self::OneBot => "onebot",
        }
    }
}

impl From<RespondResponse> for CoreResponse {
    fn from(value: RespondResponse) -> Self {
        Self {
            text: value.text,
            markdown: value.markdown,
            handled: value.handled,
            session_id: value.session_id,
            command: value.command,
            diagnostics: value.diagnostics,
        }
    }
}

impl CoreError {
    pub fn new(
        code: impl Into<String>,
        stage: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            stage: stage.into(),
            message: message.into(),
        }
    }

    pub fn as_info(&self) -> ErrorInfo {
        ErrorInfo {
            code: self.code.clone(),
            message: self.message.clone(),
            stage: self.stage.clone(),
        }
    }
}

impl From<LlmError> for CoreError {
    fn from(value: LlmError) -> Self {
        Self {
            code: value.code,
            stage: value.stage,
            message: value.message,
        }
    }
}

impl From<ErrorInfo> for CoreError {
    fn from(value: ErrorInfo) -> Self {
        Self {
            code: value.code,
            stage: value.stage,
            message: value.message,
        }
    }
}

impl CoreRespondFailure {
    fn cancelled() -> Self {
        Self {
            kind: CoreFailureKind::Cancelled,
            message: "请求已取消".to_owned(),
            retryable: true,
        }
    }

    fn from_llm_error(error: &LlmError) -> Self {
        let core_error = CoreError::from(error.clone());
        Self::from_core_error(&core_error)
    }

    fn from_core_error(error: &CoreError) -> Self {
        let kind = match (error.code.as_str(), error.stage.as_str()) {
            ("timeout", "query" | "search" | "web_search") => CoreFailureKind::SearchTimeout,
            ("timeout", _) => CoreFailureKind::LlmTimeout,
            (_, "query" | "search" | "web_search") => CoreFailureKind::SearchFailed,
            ("provider_error" | "http_error" | "upstream_unavailable" | "rate_limited", _) => {
                CoreFailureKind::LlmFailed
            }
            _ => CoreFailureKind::Internal,
        };
        Self {
            kind,
            message: user_visible_failure_message(kind),
            retryable: matches!(
                kind,
                CoreFailureKind::SearchTimeout
                    | CoreFailureKind::SearchFailed
                    | CoreFailureKind::LlmTimeout
                    | CoreFailureKind::LlmFailed
                    | CoreFailureKind::Cancelled
            ),
        }
    }
}

fn user_visible_failure_message(kind: CoreFailureKind) -> String {
    match kind {
        CoreFailureKind::SearchTimeout => "联网查询超时了，请稍后再试。",
        CoreFailureKind::SearchFailed => "联网查询暂时不可用，请稍后再试。",
        CoreFailureKind::LlmTimeout => "LLM 服务处理超时，请稍后再试。",
        CoreFailureKind::LlmFailed => "上游服务暂时不可用，请稍后再试。",
        CoreFailureKind::Cancelled => "请求已取消。",
        CoreFailureKind::Internal => "处理失败，请稍后再试。",
    }
    .to_owned()
}

fn respond_options(config: &AppConfig) -> RespondServiceOptions {
    RespondServiceOptions {
        title_model: config.title_model.clone(),
        todo_model: config.todo_model.clone(),
        memory_model: config.memory_model.clone(),
        compact_model: config.compact_model.clone(),
        translation_model: config.translation_model.clone(),
        rss_summary_max_chars: config.rss_summary_max_chars as usize,
        rss_seen_retention: config.rss_seen_retention as usize,
        tool_calling_enabled: config.tool_calling_enabled,
        tool_calling_max_rounds: config.tool_calling_max_rounds as usize,
        context_budget: config.context_budget,
        tool_result_max_chars: config.tool_result_max_chars,
    }
}

fn warn_core_error(scope_key: &str, err: &LlmError) {
    warn!(
        scope_key,
        error_code = err.code,
        error_stage = err.stage,
        error_message = %safe_error_message(err),
        "core respond request failed"
    );
}

fn error_core_error(scope_key: &str, err: &LlmError) {
    error!(
        scope_key,
        error_code = err.code,
        error_stage = err.stage,
        error_message = %safe_error_message(err),
        "core respond request timed out"
    );
}

fn safe_error_message(err: &LlmError) -> String {
    // 只把脱敏后的短错误摘要写入日志，避免 HTTP 上游正文携带 token、URL query 或过长 payload。
    truncate_chars_with_ellipsis_trimmed(&redact_sensitive_text(&err.message), 500)
}

#[cfg(test)]
mod tests;

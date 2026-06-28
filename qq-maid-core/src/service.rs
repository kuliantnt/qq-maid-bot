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
        RespondExecutors, RespondRequest, RespondResponse, RespondServiceOptions, RespondStores,
        RustRespondService,
    },
    util::metrics::MetricsRecorder,
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
        let should_stream = should_stream_respond(&req);
        let service = self.respond_service();
        let recorder = MetricsRecorder::start();
        let scope_key = req.scope_key.clone();
        let state = self.state.as_ref();
        if should_stream {
            let provider_stream_enabled = state.provider.stream_enabled();
            let result = timeout(
                Duration::from_secs(state.config.request_timeout_seconds),
                async {
                    Ok::<_, LlmError>(start_core_response_stream(
                        service,
                        req,
                        provider_stream_enabled,
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
            service.respond(req),
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
    provider_stream_enabled: bool,
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
        let result = run_streaming_respond(
            &service,
            req,
            tx.clone(),
            producer_cancelled.clone(),
            provider_stream_enabled,
        )
        .await;
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
    tx: mpsc::Sender<CoreResponseEvent>,
    cancelled: Arc<AtomicBool>,
    provider_stream_enabled: bool,
) -> Result<RespondResponse, LlmError> {
    if !provider_stream_enabled {
        let response = service.respond(req).await?;
        // provider 流式关闭时仍维持 Core/Gateway 的进程内流契约；
        // 这里合成单个 delta，但上游请求保持原有非 SSE 行为。
        if response.ok
            && let Some(delta) = response
                .markdown
                .as_deref()
                .or(response.text.as_deref())
                .map(str::to_owned)
                .filter(|value| !value.trim().is_empty())
        {
            send_core_delta(&tx, &cancelled, delta).await?;
        }
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

fn should_stream_respond(req: &RespondRequest) -> bool {
    let text = req.effective_user_text();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if is_web_search_command(trimmed) {
        return true;
    }
    // 只有普通聊天默认流式化；短命令继续走 Complete，保留原有总超时语义。
    !trimmed.starts_with('/') && !trimmed.starts_with('／')
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

fn is_web_search_command(text: &str) -> bool {
    let normalized = text.strip_prefix('／').unwrap_or(text);
    normalized.starts_with("/查")
        || normalized.starts_with("/查询")
        || normalized.starts_with("/search ")
        || normalized == "/search"
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
    }
}

fn warn_core_error(scope_key: &str, err: &LlmError) {
    warn!(
        scope_key,
        error_code = err.code,
        error_stage = err.stage,
        "core respond request failed"
    );
}

fn error_core_error(scope_key: &str, err: &LlmError) {
    error!(
        scope_key,
        error_code = err.code,
        error_stage = err.stage,
        "core respond request timed out"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use crate::{
        config::{
            DEFAULT_BIGMODEL_BASE_URL, DEFAULT_DEEPSEEK_BASE_URL, DEFAULT_RSS_SUMMARY_MAX_CHARS,
            DailyReminderTime, OpenAiApiMode, ProviderMode,
        },
        provider::{
            ChatOutcome, LlmProvider, LlmStream, LlmStreamEvent,
            status::{UpstreamStatus, observe_provider},
            types::{ModelRoute, TokenUsage},
        },
        runtime::{
            knowledge::KnowledgeIndex,
            prompt::PromptConfig,
            query::{QueryExecutor, QueryOutcome, QueryRequest},
            rss::{RssFetchConfig, RssFetcher, RssStore},
            session::SessionStore,
            todo::TodoStore,
            train::{TrainExecutor, TrainSchedule, TrainScheduleRequest},
            weather::{WeatherExecutor, WeatherOutcome, WeatherRequest},
        },
        storage::{APP_MIGRATIONS, database::SqliteDatabase, knowledge::KnowledgeStore},
        util::metrics::LlmMetrics,
    };

    #[test]
    fn private_conversation_derives_private_scope() {
        let req = CoreRequest {
            text: "hello".to_owned(),
            platform: Platform::QqOfficial,
            actor: CoreActor {
                user_id: Some("u1".to_owned()),
            },
            conversation: CoreConversation::Private {
                peer_id: "u1".to_owned(),
            },
        };

        let respond: RespondRequest = req.into();

        assert_eq!(respond.scope_key, "private:u1");
        assert_eq!(respond.platform, "qq_official");
        assert_eq!(respond.user_id.as_deref(), Some("u1"));
        assert_eq!(respond.group_id, None);
    }

    #[test]
    fn group_conversation_derives_group_scope_without_member_split() {
        let req = CoreRequest {
            text: "/todo".to_owned(),
            platform: Platform::QqOfficial,
            actor: CoreActor { user_id: None },
            conversation: CoreConversation::Group {
                group_id: "g1".to_owned(),
            },
        };

        let respond: RespondRequest = req.into();

        assert_eq!(respond.scope_key, "group:g1");
        assert_eq!(respond.platform, "qq_official");
        assert_eq!(respond.user_id, None);
        assert_eq!(respond.group_id.as_deref(), Some("g1"));
    }

    #[test]
    fn core_response_keeps_public_fields_from_respond_response() {
        let response = CoreResponse::from(RespondResponse {
            ok: true,
            text: Some("text".to_owned()),
            markdown: Some("**text**".to_owned()),
            handled: Some(true),
            session_id: Some("session-1".to_owned()),
            command: Some("chat".to_owned()),
            diagnostics: Some(serde_json::json!({"k":"v"})),
            metrics: LlmMetrics {
                provider: "test".to_owned(),
                model: "test".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: None,
            error: None,
        });

        assert_eq!(response.text.as_deref(), Some("text"));
        assert_eq!(response.markdown.as_deref(), Some("**text**"));
        assert_eq!(response.handled, Some(true));
        assert_eq!(response.session_id.as_deref(), Some("session-1"));
        assert_eq!(response.command.as_deref(), Some("chat"));
        assert_eq!(response.diagnostics.unwrap()["k"], "v");
    }

    #[tokio::test]
    async fn upstream_check_calls_provider_without_creating_session() {
        let provider = TestProvider::replying("OK");
        let state = test_state(provider.clone(), 5);
        let session_store = state.session_store.clone();
        let service = CoreHandle::new(state);

        service.upstream_check().await.unwrap();

        let requests = provider.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].session_id, "diagnostic:upstream_check");
        assert_eq!(
            requests[0].metadata.get("purpose").map(String::as_str),
            Some("upstream_check")
        );
        // `/ping check` 只验证 provider 连通性，不能创建业务会话或写聊天历史。
        let sessions = session_store
            .list_for_scope("diagnostic:upstream_check", None)
            .unwrap();
        assert!(sessions.is_empty());
    }

    #[tokio::test]
    async fn provider_error_is_returned_as_stream_failure() {
        let state = test_state(
            TestProvider::failing(LlmError::provider("boom", "provider")),
            5,
        );
        let service = CoreHandle::new(state);

        let failure = collect_stream_failure(service.respond(private_request("hello")).await).await;

        assert_eq!(failure.kind, CoreFailureKind::LlmFailed);
        assert!(failure.retryable);
    }

    #[tokio::test]
    async fn stream_response_is_not_cut_by_request_total_timeout() {
        let state = test_state(TestProvider::delayed("late", Duration::from_millis(80)), 0);
        let service = CoreHandle::new(state);

        let response =
            collect_stream_completed(service.respond(private_request("hello")).await).await;

        assert_eq!(response.text.as_deref(), Some("late"));
    }

    #[tokio::test]
    async fn chat_stream_forwards_text_delta_and_completed_from_same_stream() {
        let provider = TestProvider::streaming(vec![
            Ok(LlmStreamEvent::TextDelta("你".to_owned())),
            Ok(LlmStreamEvent::TextDelta("好".to_owned())),
            Ok(LlmStreamEvent::Completed {
                usage: None,
                finish_reason: None,
                fallback_used: false,
            }),
        ]);
        let state = test_state(provider.clone(), 5);
        let session_store = state.session_store.clone();
        let service = CoreHandle::new(state);
        let CoreRespondOutput::Stream(mut stream) =
            service.respond(private_request("hello")).await.unwrap()
        else {
            panic!("expected stream output");
        };

        assert_eq!(
            stream.recv().await,
            Some(CoreResponseEvent::TextDelta("你".to_owned()))
        );
        assert_eq!(
            stream.recv().await,
            Some(CoreResponseEvent::TextDelta("好".to_owned()))
        );
        let Some(CoreResponseEvent::Completed(response)) = stream.recv().await else {
            panic!("expected completed response");
        };

        assert_eq!(response.text.as_deref(), Some("你好"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let sessions = session_store.list_for_scope("private:u1", None).unwrap();
        assert_eq!(sessions[0].history.last().unwrap().content, "你好");
    }

    #[tokio::test]
    async fn stream_disabled_chat_is_wrapped_as_process_stream() {
        let provider = TestProvider::replying("非流完整回复");
        let state = test_state(provider.clone(), 5);
        let service = CoreHandle::new(state);
        let CoreRespondOutput::Stream(mut stream) =
            service.respond(private_request("hello")).await.unwrap()
        else {
            panic!("expected stream output");
        };

        assert_eq!(
            stream.recv().await,
            Some(CoreResponseEvent::TextDelta("非流完整回复".to_owned()))
        );
        let Some(CoreResponseEvent::Completed(response)) = stream.recv().await else {
            panic!("expected completed response");
        };

        assert_eq!(response.text.as_deref(), Some("非流完整回复"));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            provider.requests()[0].metadata.get("purpose").unwrap(),
            "chat"
        );
    }

    async fn collect_stream_failure(
        output: Result<CoreRespondOutput, CoreError>,
    ) -> CoreRespondFailure {
        let CoreRespondOutput::Stream(mut stream) = output.unwrap() else {
            panic!("expected stream output");
        };
        while let Some(event) = stream.recv().await {
            if let CoreResponseEvent::Failed(failure) = event {
                return failure;
            }
        }
        panic!("stream ended without failure");
    }

    async fn collect_stream_completed(
        output: Result<CoreRespondOutput, CoreError>,
    ) -> CoreResponse {
        let CoreRespondOutput::Stream(mut stream) = output.unwrap() else {
            panic!("expected stream output");
        };
        while let Some(event) = stream.recv().await {
            if let CoreResponseEvent::Completed(response) = event {
                return response;
            }
        }
        panic!("stream ended without completed response");
    }

    #[derive(Clone)]
    enum ProviderBehavior {
        Reply(String),
        Stream(Vec<Result<LlmStreamEvent, LlmError>>),
        Error(LlmError),
        Delayed { reply: String, delay: Duration },
    }

    #[derive(Clone)]
    struct TestProvider {
        behavior: ProviderBehavior,
        requests: Arc<Mutex<Vec<ChatRequest>>>,
        calls: Arc<AtomicUsize>,
        stream_enabled: bool,
    }

    impl TestProvider {
        fn replying(reply: &str) -> Self {
            Self::new(ProviderBehavior::Reply(reply.to_owned()))
        }

        fn failing(error: LlmError) -> Self {
            Self::new(ProviderBehavior::Error(error))
        }

        fn streaming(events: Vec<Result<LlmStreamEvent, LlmError>>) -> Self {
            Self::new(ProviderBehavior::Stream(events)).with_stream_enabled(true)
        }

        fn delayed(reply: &str, delay: Duration) -> Self {
            Self::new(ProviderBehavior::Delayed {
                reply: reply.to_owned(),
                delay,
            })
        }

        fn new(behavior: ProviderBehavior) -> Self {
            Self {
                behavior,
                requests: Arc::new(Mutex::new(Vec::new())),
                calls: Arc::new(AtomicUsize::new(0)),
                stream_enabled: false,
            }
        }

        fn with_stream_enabled(mut self, enabled: bool) -> Self {
            self.stream_enabled = enabled;
            self
        }

        fn requests(&self) -> Vec<ChatRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for TestProvider {
        async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(req);
            match &self.behavior {
                ProviderBehavior::Reply(reply) => Ok(chat_outcome(reply)),
                ProviderBehavior::Stream(events) => {
                    let reply = events
                        .iter()
                        .filter_map(|event| match event {
                            Ok(LlmStreamEvent::TextDelta(delta)) => Some(delta.as_str()),
                            _ => None,
                        })
                        .collect::<String>();
                    Ok(chat_outcome(&reply))
                }
                ProviderBehavior::Error(error) => Err(error.clone()),
                ProviderBehavior::Delayed { reply, delay } => {
                    tokio::time::sleep(*delay).await;
                    Ok(chat_outcome(reply))
                }
            }
        }

        async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.requests.lock().unwrap().push(req);
            match &self.behavior {
                ProviderBehavior::Reply(reply) => Ok(Box::pin(futures::stream::iter(vec![
                    Ok(LlmStreamEvent::TextDelta(reply.clone())),
                    Ok(LlmStreamEvent::Completed {
                        usage: None,
                        finish_reason: None,
                        fallback_used: false,
                    }),
                ]))),
                ProviderBehavior::Stream(events) => {
                    Ok(Box::pin(futures::stream::iter(events.to_vec())))
                }
                ProviderBehavior::Error(error) => Err(error.clone()),
                ProviderBehavior::Delayed { reply, delay } => {
                    let reply = reply.clone();
                    let delay = *delay;
                    Ok(Box::pin(futures::stream::unfold(
                        (0_u8, reply, delay),
                        |(state, reply, delay)| async move {
                            if state == 0 {
                                tokio::time::sleep(delay).await;
                                return Some((
                                    Ok(LlmStreamEvent::TextDelta(reply)),
                                    (1, String::new(), delay),
                                ));
                            }
                            if state == 1 {
                                return Some((
                                    Ok(LlmStreamEvent::Completed {
                                        usage: None,
                                        finish_reason: None,
                                        fallback_used: false,
                                    }),
                                    (2, String::new(), delay),
                                ));
                            }
                            None
                        },
                    )))
                }
            }
        }

        fn name(&self) -> &'static str {
            "test-provider"
        }

        fn model(&self) -> &str {
            "test-model"
        }

        fn stream_enabled(&self) -> bool {
            self.stream_enabled
        }
    }

    struct EmptyQueryExecutor;

    #[async_trait::async_trait]
    impl QueryExecutor for EmptyQueryExecutor {
        async fn query(&self, _req: QueryRequest) -> Result<QueryOutcome, LlmError> {
            Err(LlmError::provider("query unused", "query"))
        }

        fn provider_name(&self) -> &'static str {
            "empty-query"
        }
    }

    struct EmptyWeatherExecutor;

    #[async_trait::async_trait]
    impl WeatherExecutor for EmptyWeatherExecutor {
        async fn weather(&self, _req: WeatherRequest) -> Result<WeatherOutcome, LlmError> {
            Err(LlmError::provider("weather unused", "weather"))
        }

        fn provider_name(&self) -> &'static str {
            "empty-weather"
        }
    }

    struct EmptyTrainExecutor;

    #[async_trait::async_trait]
    impl TrainExecutor for EmptyTrainExecutor {
        async fn query_train_schedule(
            &self,
            _req: TrainScheduleRequest,
        ) -> Result<TrainSchedule, LlmError> {
            Err(LlmError::provider("train unused", "train"))
        }

        fn provider_name(&self) -> &'static str {
            "empty-train"
        }
    }

    fn private_request(text: &str) -> CoreRequest {
        CoreRequest {
            text: text.to_owned(),
            platform: Platform::QqOfficial,
            actor: CoreActor {
                user_id: Some("u1".to_owned()),
            },
            conversation: CoreConversation::Private {
                peer_id: "u1".to_owned(),
            },
        }
    }

    fn chat_outcome(reply: &str) -> ChatOutcome {
        ChatOutcome {
            reply: reply.to_owned(),
            metrics: LlmMetrics {
                provider: "test-provider".to_owned(),
                model: "test-model".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: Some(TokenUsage {
                input_tokens: None,
                cached_input_tokens: None,
                output_tokens: None,
                total_tokens: None,
            }),
            fallback_used: false,
        }
    }

    fn test_state(
        provider: TestProvider,
        request_timeout_seconds: u64,
    ) -> crate::http::routes::AppState {
        let base_dir = std::env::temp_dir().join(format!(
            "qq-maid-core-service-test-{}",
            uuid::Uuid::new_v4()
        ));
        let prompt_dir = base_dir.join("prompts");
        fs::create_dir_all(&prompt_dir).unwrap();
        for file_name in crate::runtime::prompt::PROMPT_FILES {
            fs::write(prompt_dir.join(file_name), format!("{file_name} content")).unwrap();
        }
        let member_id_mapping_file = base_dir.join("member.json");
        fs::write(&member_id_mapping_file, "{}").unwrap();
        let app_db_file = base_dir.join("app.db");
        let database = SqliteDatabase::open(&app_db_file, APP_MIGRATIONS).unwrap();
        let knowledge_dir = base_dir.join("knowledge");
        let knowledge_index =
            KnowledgeIndex::new(KnowledgeStore::new(database.clone()), &knowledge_dir);
        knowledge_index.sync().unwrap();
        let upstream_status = UpstreamStatus::default();

        crate::http::routes::AppState {
            config: AppConfig {
                provider: ProviderMode::OpenAi,
                model: "test-model".to_owned(),
                model_route: ModelRoute::parse_config("test-model", "LLM_MODEL").unwrap(),
                title_model: None,
                todo_model: None,
                memory_model: None,
                compact_model: None,
                translation_model: None,
                openai_search_model: "test-search".to_owned(),
                openai_api_key: Some("test".to_owned()),
                openai_base_url: None,
                openai_api_mode: OpenAiApiMode::Auto,
                deepseek_api_key: None,
                deepseek_base_url: DEFAULT_DEEPSEEK_BASE_URL.to_owned(),
                deepseek_model: "deepseek-chat".to_owned(),
                bigmodel_api_key: None,
                bigmodel_base_url: DEFAULT_BIGMODEL_BASE_URL.to_owned(),
                bigmodel_model: "glm-5.2".to_owned(),
                stream: false,
                request_timeout_seconds,
                ttft_warn_seconds: 30,
                max_output_tokens: 1200,
                max_concurrent_responses: 4,
                server_host: "127.0.0.1".to_owned(),
                server_port: 8787,
                app_db_file: app_db_file.to_string_lossy().into_owned(),
                rss_enabled: false,
                rss_poll_interval_seconds: 300,
                rss_http_timeout_seconds: 15,
                rss_max_body_bytes: 2 * 1024 * 1024,
                rss_max_push_per_feed: 3,
                rss_summary_max_chars: DEFAULT_RSS_SUMMARY_MAX_CHARS,
                rss_seen_retention: 500,
                rss_push_max_failures: 3,
                rss_push_message_type: "markdown".to_owned(),
                todo_daily_reminder_enabled: false,
                todo_daily_reminder_time: DailyReminderTime { hour: 9, minute: 0 },
                rss_allow_private_urls: true,
                prompt_dir: prompt_dir.to_string_lossy().into_owned(),
                prompt_dir_uses_builtin_defaults: false,
                knowledge_dir: knowledge_dir.to_string_lossy().into_owned(),
                member_id_mapping_file: member_id_mapping_file.to_string_lossy().into_owned(),
                qweather_api_key: "test".to_owned(),
                qweather_api_host: "https://api.qweather.com".to_owned(),
                qweather_geo_host: "https://geoapi.qweather.com".to_owned(),
                web_console_enabled: false,
                web_console_allowed_origins: Vec::new(),
            },
            provider: observe_provider(Arc::new(provider), upstream_status.clone()),
            upstream_status,
            query_executor: Arc::new(EmptyQueryExecutor),
            weather_executor: Arc::new(EmptyWeatherExecutor),
            train_executor: Arc::new(EmptyTrainExecutor),
            memory_store: crate::runtime::memory::MemoryStore::new(database.clone()),
            session_store: SessionStore::new(database.clone()),
            todo_store: TodoStore::new(database.clone()),
            rss_store: RssStore::new(database),
            rss_fetcher: RssFetcher::new(RssFetchConfig {
                allow_private_networks: true,
                ..RssFetchConfig::default()
            })
            .unwrap(),
            knowledge_index,
            prompt_config: PromptConfig::new(prompt_dir, member_id_mapping_file),
        }
    }
}

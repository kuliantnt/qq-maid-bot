//! LLM 上游调用状态观测。
//!
//! 状态只保存在当前进程内，用于 `/healthz` 和 gateway `/ping` 诊断；
//! 不写入业务数据库，也不保留请求正文、响应正文或上游原始错误。

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use qq_maid_common::time_context::now_unix_seconds_marker;
use serde::Serialize;

use crate::error::LlmError;
use crate::metrics::MetricsRecorder;

use super::{
    ChatOutcome, DynLlmProvider, LlmProvider, LlmStream, LlmStreamEvent, ToolCallingProtocol,
    ToolChatRequest, types::ChatRequest,
};

/// 最近一次真实 provider 调用状态。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamState {
    /// 当前进程启动后还没有完成过上游调用。
    Unverified,
    /// 最近一次上游调用成功。
    Available,
    /// 最近一次上游调用失败。
    Error,
}

/// 暴露给健康检查的脱敏快照。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UpstreamStatusSnapshot {
    pub state: UpstreamState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_checked_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_success_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub fallback_used: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_summary: Option<String>,
}

impl Default for UpstreamStatusSnapshot {
    fn default() -> Self {
        Self {
            state: UpstreamState::Unverified,
            last_checked_at: None,
            last_success_at: None,
            provider: None,
            model: None,
            fallback_used: false,
            error_summary: None,
        }
    }
}

/// 可在线程间共享的上游状态记录器。
#[derive(Debug, Clone, Default)]
pub struct UpstreamStatus {
    state: Arc<RwLock<UpstreamStatusSnapshot>>,
}

impl UpstreamStatus {
    pub fn snapshot(&self) -> UpstreamStatusSnapshot {
        self.state
            .read()
            .map(|state| state.clone())
            .unwrap_or_else(|_| UpstreamStatusSnapshot {
                state: UpstreamState::Error,
                error_summary: Some("状态读取失败".to_owned()),
                ..UpstreamStatusSnapshot::default()
            })
    }

    fn record_success(&self, outcome: &ChatOutcome) {
        let now = now_unix_seconds_marker();
        if let Ok(mut state) = self.state.write() {
            *state = UpstreamStatusSnapshot {
                state: UpstreamState::Available,
                last_checked_at: Some(now.clone()),
                last_success_at: Some(now),
                provider: Some(outcome.metrics.provider.clone()),
                model: Some(outcome.metrics.model.clone()),
                fallback_used: outcome.fallback_used,
                error_summary: None,
            };
        }
    }

    /// 超时可能取消 provider future，需由 HTTP 诊断入口显式补记。
    pub fn record_failure(&self, error: &LlmError) {
        let now = now_unix_seconds_marker();
        if let Ok(mut state) = self.state.write() {
            let last_success_at = state.last_success_at.clone();
            *state = UpstreamStatusSnapshot {
                state: UpstreamState::Error,
                last_checked_at: Some(now),
                last_success_at,
                provider: None,
                model: None,
                fallback_used: false,
                error_summary: Some(safe_error_summary(error)),
            };
        }
    }

    fn begin_attempt(&self) -> UpstreamAttemptGuard {
        UpstreamAttemptGuard {
            status: self.clone(),
            completed: false,
        }
    }
}

/// provider future 被外层 timeout 取消时不会返回 `Err`，Drop guard 负责补记失败。
struct UpstreamAttemptGuard {
    status: UpstreamStatus,
    completed: bool,
}

impl UpstreamAttemptGuard {
    fn complete(mut self) {
        self.completed = true;
    }
}

impl Drop for UpstreamAttemptGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.status
                .record_failure(&LlmError::timeout("provider_cancelled"));
        }
    }
}

/// 给现有 provider 增加统一观测，不侵入 chat、翻译、标题等各业务 flow。
pub fn observe_provider(provider: DynLlmProvider, status: UpstreamStatus) -> DynLlmProvider {
    Arc::new(ObservedProvider { provider, status })
}

struct ObservedProvider {
    provider: DynLlmProvider,
    status: UpstreamStatus,
}

#[async_trait]
impl LlmProvider for ObservedProvider {
    async fn chat(&self, req: ChatRequest) -> Result<ChatOutcome, LlmError> {
        // 自动标题失败会被业务层有意吞掉，不能覆盖刚完成的主聊天健康状态；
        // 用户显式 `/rename` 不带此标记，仍作为真实调用参与观测。
        if req.metadata.get("health_observation").map(String::as_str) == Some("ignore") {
            return self.provider.chat(req).await;
        }
        let attempt = self.status.begin_attempt();
        let result = self.provider.chat(req).await;
        match &result {
            Ok(outcome) => self.status.record_success(outcome),
            Err(error) => self.status.record_failure(error),
        }
        attempt.complete();
        result
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<LlmStream, LlmError> {
        // 自动标题等旁路任务不应覆盖主聊天健康状态；stream_chat 也保持同一约束。
        if req.metadata.get("health_observation").map(String::as_str) == Some("ignore") {
            return self.provider.stream_chat(req).await;
        }
        let provider_name = self.provider.name().to_owned();
        let model = self.provider.model().to_owned();
        let status = self.status.clone();
        let attempt = status.begin_attempt();
        let stream = match self.provider.stream_chat(req).await {
            Ok(stream) => stream,
            Err(err) => {
                status.record_failure(&err);
                attempt.complete();
                return Err(err);
            }
        };
        Ok(Box::pin(stream::unfold(
            ObservedStreamState {
                inner: stream,
                status,
                provider_name,
                model,
                attempt: Some(attempt),
                reply: String::new(),
                usage: None,
                fallback_used: false,
                recorder: MetricsRecorder::start(),
                completed: false,
                done: false,
            },
            |mut state| async move {
                let event = next_observed_stream_event(&mut state).await;
                event.map(|event| (event, state))
            },
        )))
    }

    async fn chat_with_tools(&self, req: ToolChatRequest) -> Result<ChatOutcome, LlmError> {
        if req
            .chat
            .metadata
            .get("health_observation")
            .map(String::as_str)
            == Some("ignore")
        {
            return self.provider.chat_with_tools(req).await;
        }
        let attempt = self.status.begin_attempt();
        let result = self.provider.chat_with_tools(req).await;
        match &result {
            Ok(outcome) => self.status.record_success(outcome),
            Err(error) => self.status.record_failure(error),
        }
        attempt.complete();
        result
    }

    fn tool_calling_protocol(&self, model: Option<&str>) -> Option<ToolCallingProtocol> {
        self.provider.tool_calling_protocol(model)
    }

    fn name(&self) -> &'static str {
        self.provider.name()
    }

    fn model(&self) -> &str {
        self.provider.model()
    }

    fn stream_enabled(&self) -> bool {
        self.provider.stream_enabled()
    }
}

struct ObservedStreamState {
    inner: LlmStream,
    status: UpstreamStatus,
    provider_name: String,
    model: String,
    attempt: Option<UpstreamAttemptGuard>,
    reply: String,
    usage: Option<super::types::TokenUsage>,
    fallback_used: bool,
    recorder: MetricsRecorder,
    completed: bool,
    done: bool,
}

async fn next_observed_stream_event(
    state: &mut ObservedStreamState,
) -> Option<Result<LlmStreamEvent, LlmError>> {
    if state.done {
        return None;
    }
    match state.inner.next().await {
        Some(Ok(LlmStreamEvent::TextDelta(delta))) => {
            state.recorder.mark_event();
            if !delta.is_empty() {
                state.recorder.mark_token();
            }
            state.reply.push_str(&delta);
            Some(Ok(LlmStreamEvent::TextDelta(delta)))
        }
        Some(Ok(LlmStreamEvent::Completed {
            usage,
            finish_reason,
            fallback_used,
        })) => {
            state.usage = usage.clone();
            state.fallback_used |= fallback_used;
            state.completed = true;
            let recorder = std::mem::replace(&mut state.recorder, MetricsRecorder::start());
            let outcome = ChatOutcome {
                reply: state.reply.clone(),
                metrics: recorder.finish(&state.provider_name, &state.model, true),
                usage: state.usage.clone(),
                fallback_used: state.fallback_used,
                executed_tools: Vec::new(),
                tool_results: Vec::new(),
            };
            state.status.record_success(&outcome);
            if let Some(attempt) = state.attempt.take() {
                attempt.complete();
            }
            state.done = true;
            Some(Ok(LlmStreamEvent::Completed {
                usage,
                finish_reason,
                fallback_used,
            }))
        }
        Some(Err(err)) => {
            state.status.record_failure(&err);
            if let Some(attempt) = state.attempt.take() {
                attempt.complete();
            }
            state.done = true;
            Some(Err(err))
        }
        None => {
            if !state.completed {
                state.status.record_failure(&LlmError::provider(
                    "LLM stream ended before completion",
                    "stream",
                ));
            }
            if let Some(attempt) = state.attempt.take() {
                attempt.complete();
            }
            state.done = true;
            None
        }
    }
}

/// 诊断输出只保留可行动的错误类别，绝不透传可能含密钥或请求体的原始错误。
fn safe_error_summary(error: &LlmError) -> String {
    let lower = error.message.to_ascii_lowercase();
    let http_status = http_status_from_message(&error.message);
    let status = http_status
        .map(|value| format!("（HTTP {value}）"))
        .unwrap_or_default();

    if error.code == "timeout" || lower.contains("timed out") || lower.contains("timeout") {
        return "上游请求超时".to_owned();
    }
    if error.code == "safety_blocked"
        || lower.contains("prompt_blocked")
        || lower.contains("moderation policy")
    {
        return format!("上游安全策略拦截{status}");
    }
    if lower.contains("unauthorized")
        || lower.contains("authentication")
        || lower.contains("api key")
        || lower.contains("api_key")
        || matches!(http_status, Some(401 | 403))
    {
        return format!("上游鉴权失败{status}");
    }
    if lower.contains("model not found")
        || lower.contains("unknown model")
        || lower.contains("invalid model")
        || matches!(http_status, Some(404))
    {
        return format!("模型或接口不存在{status}");
    }
    if lower.contains("parameter")
        || lower.contains("invalid request")
        || lower.contains("bad request")
        || matches!(http_status, Some(400 | 422))
    {
        return format!("请求参数或接口格式错误{status}");
    }
    if lower.contains("rate limit") || matches!(http_status, Some(429)) {
        return format!("上游限流{status}");
    }
    if !status.is_empty() {
        return format!("上游请求失败{status}");
    }
    if lower.contains("http_error@http") {
        return "上游网络请求失败".to_owned();
    }
    match error.code.as_str() {
        "config" => "LLM 配置错误".to_owned(),
        "provider_error" => "上游响应解析或内容异常".to_owned(),
        "http_error" => "上游网络请求失败".to_owned(),
        "bad_request" => "LLM 请求参数错误".to_owned(),
        "rate_limited" => "上游限流".to_owned(),
        "upstream_unavailable" => "上游服务不可用".to_owned(),
        _ => "上游调用失败".to_owned(),
    }
}

fn http_status_from_message(message: &str) -> Option<u16> {
    let upper = message.to_ascii_uppercase();
    let rest = upper.split("HTTP").nth(1)?.trim_start();
    let digits = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.len() == 3 {
        digits.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::LlmMetrics;
    use std::collections::HashMap;

    #[test]
    fn error_summary_classifies_and_never_returns_secret_detail() {
        let error = LlmError::http(
            "OpenAI returned HTTP 401: Authorization: Bearer sk-secret-value API_KEY=hidden",
        );

        let summary = safe_error_summary(&error);

        assert_eq!(summary, "上游鉴权失败（HTTP 401）");
        assert!(!summary.contains("secret"));
        assert!(!summary.contains("Bearer"));
        assert!(!summary.contains("API_KEY"));
    }

    #[test]
    fn aggregate_route_error_is_not_misclassified_by_generic_model_word() {
        let error = LlmError::provider(
            "all model candidates failed: #0 openai:gpt -> http_error@http",
            "provider_route",
        );

        assert_eq!(safe_error_summary(&error), "上游网络请求失败");
    }

    #[test]
    fn safety_blocked_summary_does_not_expose_provider_detail() {
        let error = LlmError::new(
            "safety_blocked",
            "Chat Completions returned HTTP 400: prompt_blocked moderation policy detail",
            "http",
        );

        let summary = safe_error_summary(&error);

        assert_eq!(summary, "上游安全策略拦截（HTTP 400）");
        assert!(!summary.contains("prompt_blocked"));
        assert!(!summary.contains("moderation"));
    }

    #[test]
    fn initial_snapshot_is_unverified() {
        assert_eq!(
            UpstreamStatus::default().snapshot().state,
            UpstreamState::Unverified
        );
    }

    #[test]
    fn records_success_with_final_route_and_fallback_flag() {
        let status = UpstreamStatus::default();
        status.record_success(&ChatOutcome {
            reply: "ok".to_owned(),
            metrics: LlmMetrics {
                provider: "deepseek".to_owned(),
                model: "deepseek-chat".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: None,
            fallback_used: true,
            executed_tools: Vec::new(),
            tool_results: Vec::new(),
        });

        let snapshot = status.snapshot();
        assert_eq!(snapshot.state, UpstreamState::Available);
        assert!(snapshot.last_success_at.is_some());
        assert_eq!(snapshot.provider.as_deref(), Some("deepseek"));
        assert_eq!(snapshot.model.as_deref(), Some("deepseek-chat"));
        assert!(snapshot.fallback_used);
    }

    #[test]
    fn records_failure_without_raw_error_and_keeps_last_success_time() {
        let status = UpstreamStatus::default();
        status.record_success(&ChatOutcome {
            reply: "ok".to_owned(),
            metrics: LlmMetrics {
                provider: "openai".to_owned(),
                model: "gpt-test".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: None,
            fallback_used: false,
            executed_tools: Vec::new(),
            tool_results: Vec::new(),
        });
        let success_at = status.snapshot().last_success_at;

        status.record_failure(&LlmError::http(
            "HTTP 401 Authorization: Bearer sk-secret-secret-secret",
        ));

        let snapshot = status.snapshot();
        assert_eq!(snapshot.state, UpstreamState::Error);
        assert_eq!(snapshot.last_success_at, success_at);
        assert_eq!(
            snapshot.error_summary.as_deref(),
            Some("上游鉴权失败（HTTP 401）")
        );
        let serialized = serde_json::to_string(&snapshot).unwrap();
        assert!(!serialized.contains("Authorization"));
        assert!(!serialized.contains("Bearer"));
        assert!(!serialized.contains("sk-secret"));
    }

    struct PendingProvider;

    #[async_trait]
    impl LlmProvider for PendingProvider {
        async fn chat(&self, _req: ChatRequest) -> Result<ChatOutcome, LlmError> {
            std::future::pending().await
        }

        fn name(&self) -> &'static str {
            "pending"
        }

        fn model(&self) -> &str {
            "pending-model"
        }

        fn stream_enabled(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn cancelled_provider_future_records_timeout() {
        let status = UpstreamStatus::default();
        let provider = observe_provider(Arc::new(PendingProvider), status.clone());
        let request = ChatRequest {
            session_id: "diagnostic:test".to_owned(),
            model: None,
            messages: Vec::new(),
            context_budget: None,
            metadata: HashMap::from([("purpose".to_owned(), "chat".to_owned())]),
        };

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), provider.chat(request))
                .await
                .is_err()
        );
        let snapshot = status.snapshot();
        assert_eq!(snapshot.state, UpstreamState::Error);
        assert_eq!(snapshot.error_summary.as_deref(), Some("上游请求超时"));
    }

    #[tokio::test]
    async fn session_title_call_does_not_override_main_call_status() {
        let status = UpstreamStatus::default();
        status.record_success(&ChatOutcome {
            reply: "main reply".to_owned(),
            metrics: LlmMetrics {
                provider: "deepseek".to_owned(),
                model: "deepseek-chat".to_owned(),
                stream: false,
                ttfe_ms: None,
                ttft_ms: None,
                total_latency_ms: 1,
            },
            usage: None,
            fallback_used: true,
            executed_tools: Vec::new(),
            tool_results: Vec::new(),
        });
        let provider = observe_provider(Arc::new(PendingProvider), status.clone());
        let request = ChatRequest {
            session_id: "diagnostic:title".to_owned(),
            model: None,
            messages: Vec::new(),
            context_budget: None,
            metadata: HashMap::from([
                ("purpose".to_owned(), "session_title".to_owned()),
                ("health_observation".to_owned(), "ignore".to_owned()),
            ]),
        };

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), provider.chat(request))
                .await
                .is_err()
        );
        let snapshot = status.snapshot();
        assert_eq!(snapshot.state, UpstreamState::Available);
        assert_eq!(snapshot.provider.as_deref(), Some("deepseek"));
        assert_eq!(snapshot.model.as_deref(), Some("deepseek-chat"));
        assert!(snapshot.fallback_used);
    }

    #[tokio::test]
    async fn explicit_session_title_call_is_still_observed() {
        let status = UpstreamStatus::default();
        let provider = observe_provider(Arc::new(PendingProvider), status.clone());
        let request = ChatRequest {
            session_id: "diagnostic:manual-title".to_owned(),
            model: None,
            messages: Vec::new(),
            context_budget: None,
            metadata: HashMap::from([("purpose".to_owned(), "session_title".to_owned())]),
        };

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(1), provider.chat(request))
                .await
                .is_err()
        );
        assert_eq!(status.snapshot().state, UpstreamState::Error);
    }
}

//! 候选链失败聚合与错误分类 helper。
//!
//! 这里集中存放：
//! * `ModelAttemptFailure`：单次候选尝试的失败记录。
//! * `aggregate_route_error`：把整条候选链的所有失败聚合成一个 `provider_route` 错误。
//! * `should_try_next_model` / `model_error_kind`：判定是否允许跨模型降级与观测错误分类。
//! * `model_task_name`：从请求 metadata 取出任务名，用于错误聚合与日志关联。
//!
//! 这些 helper 被候选链执行（`routing`）和流式状态机（`stream_state`）共享，
//! 单独成模块以避免 routing 与 stream_state 互相依赖。

use crate::{
    error::LlmError,
    provider::types::{ChatRequest, ModelId, ModelProvider},
};

/// 一次候选模型尝试失败的记录，用于聚合候选链失败信息。
///
/// 仅在 provider 内部使用，记录候选索引、provider、模型名和原始错误，
/// 不向调用方暴露具体业务语义。
#[derive(Debug)]
pub(crate) struct ModelAttemptFailure {
    /// 候选在链中的位置索引。
    pub(crate) index: usize,
    /// 实际尝试的 provider。
    pub(crate) provider: ModelProvider,
    /// 实际尝试的模型名。
    pub(crate) model: String,
    /// 该候选返回的原始错误。
    pub(crate) error: LlmError,
}

impl ModelAttemptFailure {
    pub(crate) fn new(
        index: usize,
        provider: ModelProvider,
        candidate: &ModelId,
        error: LlmError,
    ) -> Self {
        Self {
            index,
            provider,
            model: candidate.name.clone(),
            error,
        }
    }
}

/// 判断当前错误是否允许跨候选模型降级。
///
/// 这里只接收上游传输、限流、超时、空响应和 provider 协议类失败；配置错误、
/// 本地请求构造错误和业务参数错误会直接返回，避免把本地问题放大成多次计费请求。
pub(crate) fn should_try_next_model(err: &LlmError) -> bool {
    matches!(
        err.code.as_str(),
        "timeout" | "provider_error" | "http_error" | "rate_limited" | "upstream_unavailable"
    )
}

/// 统一错误分类，供观测日志使用。
///
/// `provider_error` 进一步按 stage 细分为流式错误、JSON 解析错误等，便于排查上游协议问题。
pub(crate) fn model_error_kind(err: &LlmError) -> &'static str {
    match err.code.as_str() {
        "timeout" => "timeout",
        "http_error" => "http_error",
        "provider_error" if matches!(err.stage.as_str(), "stream" | "sse") => "stream_error",
        "provider_error" if err.stage == "json" => "invalid_response",
        "provider_error" => "provider_error",
        "rate_limited" => "rate_limited",
        "upstream_unavailable" => "upstream_unavailable",
        "bad_request" => "permanent",
        "config" => "config",
        _ => "permanent",
    }
}

/// 从请求 metadata 中取出任务名，用于错误聚合与日志关联。
///
/// 没有显式 `purpose` 时回退到 `"chat"`，保持原有日志与聚合文案一致。
pub(crate) fn model_task_name(req: &ChatRequest) -> &str {
    req.metadata
        .get("purpose")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("chat")
}

/// 聚合候选链所有失败信息为单个 `provider_route` 错误。
///
/// 文案格式与原实现保持一致：`#<index> <provider>:<model> -> <code>@<stage>`，
/// 用 `; ` 分隔，便于在调用方日志中一次性看到整条链的失败原因。
pub(crate) fn aggregate_route_error(task: &str, failures: Vec<ModelAttemptFailure>) -> LlmError {
    let details = failures
        .into_iter()
        .map(|failure| {
            format!(
                "#{} {}:{} -> {}@{}",
                failure.index,
                failure.provider.as_str(),
                failure.model,
                failure.error.code,
                failure.error.stage
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    LlmError::provider(
        format!("all model candidates failed for task `{task}`: {details}"),
        "provider_route",
    )
}

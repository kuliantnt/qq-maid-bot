//! 流式候选链状态机。
//!
//! `stream_chat` 使用 `stream::unfold` 驱动一个跨候选降级的状态机，本模块封装其状态与
//! 状态推进逻辑。降级规则与原实现保持一致：
//!
//! * 候选流在产出任意非空正文增量之前失败，允许尝试下一个候选（错误可恢复时）；
//! * 已经产出非空正文增量后，流的错误或提前结束视为不可恢复，直接返回 `stream_after_delta` 错误，
//!   避免把已经吐出的内容丢弃导致下游拿到一段拼接错乱的回复。

use futures::StreamExt;

use crate::{
    error::LlmError,
    provider::{
        DynLlmProvider, LlmStream, LlmStreamEvent,
        types::{ChatRequest, ModelId, ModelProvider},
    },
};

use super::route_error::{
    ModelAttemptFailure, aggregate_route_error, model_error_kind, should_try_next_model,
    unavailable_provider_error,
};

/// 流式候选链执行期间持有的状态。
///
/// 每次从 `next_route_stream_event` 推进后，状态会随 `stream::unfold` 一起回传，
/// 直到 `done` 置位、流终止。
pub(crate) struct RouteStreamState {
    pub(crate) req: ChatRequest,
    pub(crate) task: String,
    pub(crate) candidates: Vec<ModelId>,
    pub(crate) providers: Vec<(ModelProvider, DynLlmProvider)>,
    pub(crate) default_provider: ModelProvider,
    pub(crate) candidate_index: usize,
    pub(crate) current_stream: Option<LlmStream>,
    pub(crate) current_attempt: Option<(usize, ModelProvider, ModelId)>,
    pub(crate) failures: Vec<ModelAttemptFailure>,
    pub(crate) emitted_non_empty_delta: bool,
    pub(crate) done: bool,
}

/// 推进流式候选链状态机，返回下一个事件。
///
/// 返回 `None` 表示流已正常结束，`Some(Err(..))` 表示聚合后的候选链错误或不可恢复错误。
pub(crate) async fn next_route_stream_event(
    state: &mut RouteStreamState,
) -> Option<Result<LlmStreamEvent, LlmError>> {
    loop {
        if state.done {
            return None;
        }
        if state.current_stream.is_none() {
            match start_next_route_candidate(state).await {
                Ok(true) => {}
                Ok(false) => {
                    state.done = true;
                    return Some(Err(aggregate_route_error(
                        &state.task,
                        std::mem::take(&mut state.failures),
                    )));
                }
                Err(err) => {
                    state.done = true;
                    return Some(Err(err));
                }
            }
        }

        let Some(stream) = state.current_stream.as_mut() else {
            continue;
        };
        match stream.next().await {
            Some(Ok(LlmStreamEvent::TextDelta(delta))) => {
                if !delta.is_empty() {
                    state.emitted_non_empty_delta = true;
                }
                return Some(Ok(LlmStreamEvent::TextDelta(delta)));
            }
            Some(Ok(LlmStreamEvent::Completed {
                usage,
                finish_reason,
                fallback_used,
            })) => {
                if !state.emitted_non_empty_delta {
                    // 流式 provider 在没有产出任何有效正文时给出完成事件，视为空响应，
                    // 记录失败后继续尝试后续候选，避免向下游返回空回复。
                    let err =
                        LlmError::provider("LLM stream returned empty text output", "provider");
                    record_current_route_failure(state, err);
                    state.current_stream = None;
                    state.current_attempt = None;
                    continue;
                }
                // candidate_index > 0 表示当前候选不是链路首个候选，已经发生过跨模型降级。
                let fallback_used = fallback_used
                    || state
                        .current_attempt
                        .as_ref()
                        .is_some_and(|(index, _, _)| *index > 0);
                state.done = true;
                return Some(Ok(LlmStreamEvent::Completed {
                    usage,
                    finish_reason,
                    fallback_used,
                }));
            }
            Some(Err(err)) => {
                if state.emitted_non_empty_delta {
                    // 已经向下流吐过正文，后续任何错误都不能再降级到新候选，
                    // 否则下游会拿到截断+重新拼接的内容，统一标记为 stream_after_delta 便于排障。
                    state.done = true;
                    return Some(Err(LlmError::new(
                        err.code,
                        err.message,
                        "stream_after_delta",
                    )));
                }
                record_current_route_failure(state, err);
                state.current_stream = None;
                state.current_attempt = None;
            }
            None => {
                if state.emitted_non_empty_delta {
                    state.done = true;
                    return Some(Err(LlmError::provider(
                        "LLM stream ended before completion after emitting text",
                        "stream_after_delta",
                    )));
                }
                let err = LlmError::provider("LLM stream ended without completion event", "stream");
                record_current_route_failure(state, err);
                state.current_stream = None;
                state.current_attempt = None;
            }
        }
    }
}

/// 启动下一个候选的流，成功时挂到 `current_stream` 上；失败按可恢复性记录或返回。
///
/// 返回 `Ok(true)` 表示已挂上新候选流，`Ok(false)` 表示所有候选已尝试完毕，
/// `Err(..)` 表示遇到不可恢复错误可直接抛出。
async fn start_next_route_candidate(state: &mut RouteStreamState) -> Result<bool, LlmError> {
    while state.candidate_index < state.candidates.len() {
        let index = state.candidate_index;
        state.candidate_index += 1;
        let candidate = state.candidates[index].clone();
        let provider_kind = candidate.provider.unwrap_or(state.default_provider);
        let Some(provider) = state
            .providers
            .iter()
            .find(|(kind, _)| *kind == provider_kind)
            .map(|(_, provider)| provider.clone())
        else {
            let err = unavailable_provider_error(provider_kind, &candidate);
            let fallback =
                state.candidate_index < state.candidates.len() && should_try_next_model(&err);
            tracing::warn!(
                task = state.task.as_str(),
                candidate_index = index,
                provider = provider_kind.as_str(),
                model = %candidate.name,
                result = "skipped",
                error_code = err.code.as_str(),
                error_stage = err.stage.as_str(),
                error_kind = model_error_kind(&err),
                fallback,
                "model candidate provider is not available"
            );
            if !fallback {
                state.failures.push(ModelAttemptFailure::new(
                    index,
                    provider_kind,
                    &candidate,
                    err,
                ));
                return Ok(false);
            }
            state.failures.push(ModelAttemptFailure::new(
                index,
                provider_kind,
                &candidate,
                err,
            ));
            continue;
        };
        let mut candidate_req = state.req.clone();
        candidate_req.model = Some(candidate.to_request_model());
        match provider.stream_chat(candidate_req).await {
            Ok(stream) => {
                tracing::debug!(
                    task = state.task.as_str(),
                    candidate_index = index,
                    provider = provider_kind.as_str(),
                    model = %candidate.name,
                    result = "stream_started",
                    "model candidate stream started"
                );
                state.current_stream = Some(stream);
                state.current_attempt = Some((index, provider_kind, candidate));
                return Ok(true);
            }
            Err(err) => {
                let fallback =
                    state.candidate_index < state.candidates.len() && should_try_next_model(&err);
                tracing::warn!(
                    task = state.task.as_str(),
                    candidate_index = index,
                    provider = provider_kind.as_str(),
                    model = %candidate.name,
                    result = "failed",
                    error_code = err.code.as_str(),
                    error_stage = err.stage.as_str(),
                    error_kind = model_error_kind(&err),
                    fallback,
                    "model candidate stream init failed"
                );
                if !fallback {
                    return Err(err);
                }
                state.failures.push(ModelAttemptFailure::new(
                    index,
                    provider_kind,
                    &candidate,
                    err,
                ));
            }
        }
    }
    Ok(false)
}

/// 记录当前候选（已产出正文之前）的失败信息，并按可恢复性决定是否还有候选可降级。
fn record_current_route_failure(state: &mut RouteStreamState, err: LlmError) {
    let Some((index, provider_kind, candidate)) = state.current_attempt.take() else {
        // 没有记录到当前候选信息时，仍按 default_provider 占位记录一条失败，
        // 保证聚合错误不会遗漏。
        state.failures.push(ModelAttemptFailure {
            index: state.candidate_index,
            provider: state.default_provider,
            model: "<unknown>".to_owned(),
            error: err,
        });
        return;
    };
    let fallback = state.candidate_index < state.candidates.len() && should_try_next_model(&err);
    tracing::warn!(
        task = state.task.as_str(),
        candidate_index = index,
        provider = provider_kind.as_str(),
        model = %candidate.name,
        result = "failed",
        error_code = err.code.as_str(),
        error_stage = err.stage.as_str(),
        error_kind = model_error_kind(&err),
        fallback,
        "model candidate stream failed before text delta"
    );
    state.failures.push(ModelAttemptFailure::new(
        index,
        provider_kind,
        &candidate,
        err,
    ));
    if !fallback {
        // 不可恢复且后续不再有可降级候选时，直接把游标推到末尾终止候选链。
        state.candidate_index = state.candidates.len();
    }
}

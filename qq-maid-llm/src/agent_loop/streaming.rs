//! Agent 单步流式推进、可见输出所有权与受预算约束的非流式兼容回退。

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};

use futures::future::{Either, select};
use tokio::time::{Duration, timeout};

use crate::{
    agent_loop::{
        AgentRunHandle, AgentStreamingDiagnostics, AgentTextDeltaDelivery, AgentTextDeltaFuture,
        AgentTextDeltaSink, AgentToolResult,
    },
    error::LlmError,
};

use super::{session::AgentStepSession, types::AgentStep};

const MIN_NON_STREAM_FALLBACK_START_BUDGET: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(super) struct StreamingAdvanceOptions {
    pub(super) final_delta_sink: Option<AgentTextDeltaSink>,
    pub(super) streaming_timeout: Duration,
    pub(super) non_stream_timeout: Duration,
    pub(super) round: usize,
}

pub(super) async fn advance_with_optional_streaming(
    session: &mut (dyn AgentStepSession + Send),
    results: &[AgentToolResult],
    allow_tool_calls: bool,
    options: StreamingAdvanceOptions,
    run_handle: &AgentRunHandle,
) -> Result<AgentAdvance, LlmError> {
    let StreamingAdvanceOptions {
        final_delta_sink,
        streaming_timeout,
        non_stream_timeout,
        round,
    } = options;
    let Some(sink) = final_delta_sink else {
        return advance_non_stream_with_timeout(
            session,
            results,
            allow_tool_calls,
            non_stream_timeout,
        )
        .await
        .map(|step| AgentAdvance {
            step,
            fallback_used: false,
        });
    };
    let emitted_visible_delta = Arc::new(AtomicBool::new(false));
    let tracked_sink = track_visible_delta_sink(sink, emitted_visible_delta.clone());
    let activity_counter = session.streaming_activity_counter();
    let streaming_started = Instant::now();
    let streaming = advance_streaming_until_complete_or_first_activity_timeout(
        session,
        results,
        allow_tool_calls,
        tracked_sink,
        activity_counter,
        streaming_timeout,
    )
    .await;
    let streaming_elapsed_ms = streaming_started.elapsed().as_millis();
    match streaming {
        StreamingAttempt::Completed(Ok(Some(step))) => Ok(AgentAdvance {
            step,
            fallback_used: false,
        }),
        StreamingAttempt::Completed(Ok(None)) => {
            fallback_to_non_stream(
                session,
                results,
                allow_tool_calls,
                non_stream_timeout,
                round,
                streaming_elapsed_ms,
                "advance_streaming_none",
                None,
                false,
                run_handle,
            )
            .await
        }
        StreamingAttempt::Completed(Err(err)) if !emitted_visible_delta.load(Ordering::SeqCst) => {
            let diagnostics = session.streaming_diagnostics();
            let fallback_reason = diagnostics
                .fallback_reason
                .as_deref()
                .unwrap_or_else(|| classify_streaming_error(&err));
            if diagnostics.saw_text_delta || diagnostics.buffered_text_chars > 0 {
                log_streaming_fallback_skipped(
                    session,
                    round,
                    allow_tool_calls,
                    streaming_elapsed_ms,
                    fallback_reason,
                    &err,
                    &diagnostics,
                    run_handle.remaining_budget(),
                    run_handle,
                    "stream_had_valid_text",
                );
                return Err(err);
            }
            if diagnostics.explicit_failure_event {
                log_streaming_fallback_skipped(
                    session,
                    round,
                    allow_tool_calls,
                    streaming_elapsed_ms,
                    fallback_reason,
                    &err,
                    &diagnostics,
                    run_handle.remaining_budget(),
                    run_handle,
                    "explicit_failure_event",
                );
                return Err(err);
            }
            fallback_to_non_stream(
                session,
                results,
                allow_tool_calls,
                non_stream_timeout,
                round,
                streaming_elapsed_ms,
                fallback_reason,
                Some(&err),
                true,
                run_handle,
            )
            .await
        }
        StreamingAttempt::FirstActivityTimedOut
            if !emitted_visible_delta.load(Ordering::SeqCst) =>
        {
            fallback_to_non_stream(
                session,
                results,
                allow_tool_calls,
                non_stream_timeout,
                round,
                streaming_elapsed_ms,
                "streaming_step_timeout",
                None,
                true,
                run_handle,
            )
            .await
        }
        StreamingAttempt::Completed(Err(err)) => Err(err),
        StreamingAttempt::FirstActivityTimedOut => {
            Err(LlmError::timeout("agent_stream_after_delta"))
        }
    }
}

enum StreamingAttempt {
    Completed(Result<Option<AgentStep>, LlmError>),
    FirstActivityTimedOut,
}

async fn advance_streaming_until_complete_or_first_activity_timeout(
    session: &mut (dyn AgentStepSession + Send),
    results: &[AgentToolResult],
    allow_tool_calls: bool,
    tracked_sink: AgentTextDeltaSink,
    activity_counter: Option<Arc<AtomicUsize>>,
    first_activity_timeout: Duration,
) -> StreamingAttempt {
    let Some(activity_counter) = activity_counter else {
        return match timeout(
            first_activity_timeout,
            session.advance_streaming(results, allow_tool_calls, tracked_sink),
        )
        .await
        {
            Ok(result) => StreamingAttempt::Completed(result),
            Err(_) => StreamingAttempt::FirstActivityTimedOut,
        };
    };

    let streaming = Box::pin(session.advance_streaming(results, allow_tool_calls, tracked_sink));
    let deadline = Box::pin(tokio::time::sleep(first_activity_timeout));
    match select(streaming, deadline).await {
        Either::Left((result, _)) => StreamingAttempt::Completed(result),
        Either::Right((_, streaming)) => {
            if activity_counter.load(Ordering::SeqCst) > 0 {
                StreamingAttempt::Completed(streaming.await)
            } else {
                StreamingAttempt::FirstActivityTimedOut
            }
        }
    }
}

#[derive(Debug)]
pub(super) struct AgentAdvance {
    pub(super) step: AgentStep,
    pub(super) fallback_used: bool,
}

async fn advance_non_stream_with_timeout(
    session: &mut (dyn AgentStepSession + Send),
    results: &[AgentToolResult],
    allow_tool_calls: bool,
    step_timeout: Duration,
) -> Result<AgentStep, LlmError> {
    timeout(step_timeout, session.advance(results, allow_tool_calls))
        .await
        .map_err(|_| LlmError::timeout("agent_step"))?
}

#[allow(clippy::too_many_arguments)]
async fn fallback_to_non_stream(
    session: &mut (dyn AgentStepSession + Send),
    results: &[AgentToolResult],
    allow_tool_calls: bool,
    non_stream_timeout: Duration,
    round: usize,
    streaming_elapsed_ms: u128,
    fallback_reason: &str,
    err: Option<&LlmError>,
    fallback_used: bool,
    run_handle: &AgentRunHandle,
) -> Result<AgentAdvance, LlmError> {
    let diagnostics = session.streaming_diagnostics();
    let fallback_remaining_budget = run_handle.remaining_budget();
    let minimum_start_budget =
        std::cmp::min(non_stream_timeout, MIN_NON_STREAM_FALLBACK_START_BUDGET);
    if fallback_remaining_budget.is_some_and(|remaining| remaining < minimum_start_budget) {
        let budget_error = LlmError::timeout("agent_loop");
        log_streaming_fallback_skipped(
            session,
            round,
            allow_tool_calls,
            streaming_elapsed_ms,
            fallback_reason,
            err.unwrap_or(&budget_error),
            &diagnostics,
            fallback_remaining_budget,
            run_handle,
            "insufficient_remaining_budget",
        );
        return Err(budget_error);
    }
    let effective_timeout = fallback_remaining_budget
        .map(|remaining| std::cmp::min(non_stream_timeout, remaining))
        .unwrap_or(non_stream_timeout);
    let fallback_started = Instant::now();
    let result =
        advance_non_stream_with_timeout(session, results, allow_tool_calls, effective_timeout)
            .await;
    let non_stream_fallback_elapsed_ms = fallback_started.elapsed().as_millis();
    let agent_diagnostics = run_handle.snapshot();
    tracing::info!(
        provider = session.provider(),
        model = %session.model(),
        round,
        allow_tool_calls,
        follows_tool_results = !results.is_empty(),
        streaming_elapsed_ms,
        fallback_reason,
        error_code = err.map(|item| item.code.as_str()).unwrap_or("none"),
        error_stage = err.map(|item| item.stage.as_str()).unwrap_or("none"),
        http_status = diagnostics.http_status,
        stream_end_kind = diagnostics.stream_end_kind.as_deref().unwrap_or("unknown"),
        last_sse_event_type = diagnostics.last_sse_event_type.as_deref().unwrap_or("none"),
        normal_eof = diagnostics.normal_eof,
        connection_reset = diagnostics.connection_reset,
        parse_error = diagnostics.parse_error,
        explicit_failure_event = diagnostics.explicit_failure_event,
        incomplete_reason = diagnostics.incomplete_reason.as_deref().unwrap_or("none"),
        tool_execution_attempted = agent_diagnostics.tool_execution_attempted,
        tool_executed = !agent_diagnostics.executed_tools.is_empty(),
        saw_text_delta = diagnostics.saw_text_delta,
        chunk_count = diagnostics.chunk_count,
        sse_event_count = diagnostics.sse_event_count,
        saw_done = diagnostics.saw_done,
        saw_completed = diagnostics.saw_completed,
        buffered_delta_count = diagnostics.buffered_delta_count,
        buffered_text_chars = diagnostics.buffered_text_chars,
        visible_text_chars = diagnostics.visible_text_chars,
        active_function_call_count = diagnostics.active_function_call_count,
        stream_remaining_budget_ms = fallback_remaining_budget.map(|value| value.as_millis()),
        fallback_remaining_budget_ms = fallback_remaining_budget.map(|value| value.as_millis()),
        fallback_timeout_ms = effective_timeout.as_millis(),
        fallback_skipped_reason = "none",
        non_stream_fallback_elapsed_ms,
        non_stream_fallback_succeeded = result.is_ok(),
        "流式 Agent 降级请求已完成"
    );
    result
        .map(|step| AgentAdvance {
            step,
            fallback_used,
        })
        .map_err(|mut err| {
            if fallback_used {
                let mut diagnostics = err.agent.take().map(|item| *item).unwrap_or_default();
                diagnostics.streaming_fallback_used = true;
                err.with_agent(diagnostics)
            } else {
                err
            }
        })
}

#[allow(clippy::too_many_arguments)]
fn log_streaming_fallback_skipped(
    session: &(dyn AgentStepSession + Send),
    round: usize,
    allow_tool_calls: bool,
    streaming_elapsed_ms: u128,
    fallback_reason: &str,
    err: &LlmError,
    diagnostics: &AgentStreamingDiagnostics,
    remaining_budget: Option<Duration>,
    run_handle: &AgentRunHandle,
    skipped_reason: &str,
) {
    let agent_diagnostics = run_handle.snapshot();
    tracing::warn!(
        provider = session.provider(),
        model = %session.model(),
        round,
        allow_tool_calls,
        streaming_elapsed_ms,
        fallback_reason,
        error_code = err.code.as_str(),
        error_stage = err.stage.as_str(),
        http_status = diagnostics.http_status,
        stream_end_kind = diagnostics.stream_end_kind.as_deref().unwrap_or("unknown"),
        last_sse_event_type = diagnostics.last_sse_event_type.as_deref().unwrap_or("none"),
        normal_eof = diagnostics.normal_eof,
        connection_reset = diagnostics.connection_reset,
        parse_error = diagnostics.parse_error,
        explicit_failure_event = diagnostics.explicit_failure_event,
        incomplete_reason = diagnostics.incomplete_reason.as_deref().unwrap_or("none"),
        tool_execution_attempted = agent_diagnostics.tool_execution_attempted,
        tool_executed = !agent_diagnostics.executed_tools.is_empty(),
        saw_text_delta = diagnostics.saw_text_delta,
        buffered_delta_count = diagnostics.buffered_delta_count,
        buffered_text_chars = diagnostics.buffered_text_chars,
        visible_text_chars = diagnostics.visible_text_chars,
        active_function_call_count = diagnostics.active_function_call_count,
        stream_remaining_budget_ms = remaining_budget.map(|value| value.as_millis()),
        fallback_remaining_budget_ms = remaining_budget.map(|value| value.as_millis()),
        fallback_skipped_reason = skipped_reason,
        "已跳过流式 Agent 降级请求"
    );
}

fn classify_streaming_error(err: &LlmError) -> &'static str {
    if err.code == "http_error" || err.stage == "http" || err.stage == "sse" {
        "http_sse_parse_error"
    } else {
        "provider_error_other"
    }
}

fn track_visible_delta_sink(
    sink: AgentTextDeltaSink,
    emitted_visible_delta: Arc<AtomicBool>,
) -> AgentTextDeltaSink {
    Arc::new(move |delta| {
        let sink = sink.clone();
        let emitted_visible_delta = emitted_visible_delta.clone();
        Box::pin(async move {
            let has_visible_text = !delta.is_empty();
            let delivery = sink(delta).await?;
            if has_visible_text && delivery == AgentTextDeltaDelivery::Visible {
                // 只有下游确认正文已进入可见发送链路后，才关闭本轮安全降级。
                emitted_visible_delta.store(true, Ordering::SeqCst);
            }
            Ok(delivery)
        }) as AgentTextDeltaFuture
    })
}

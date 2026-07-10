//! Agent Loop 统一循环控制。
//!
//! [`run_agent_loop`] 是 #138 的核心：接管轮次推进、最大轮数、`tool_loop_limit`
//! 退出、同轮工具的 prepare-before-execute、依赖跳过、`ok:false` 业务失败
//! 识别、执行异常转结构化输出、`executed_tools` / `tool_results` 轨迹、usage
//! 合并与 `ChatOutcome` 装配。Provider 只需通过 [`AgentStepSession`](super::session::AgentStepSession)
//! 提供“一次模型请求 → 一个 `AgentStep`”的协议适配。
//!
//! 非流式语义：返回与改造前等价的完整结果；工具副作用只在此执行一次，不因
//! 后续模型或发送重试而重复。

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Instant;

use futures::future::{Either, select};
use tokio::time::{Duration, timeout};
use tracing::{debug, warn};

use crate::{
    agent_loop::{
        AgentRunDiagnostics, AgentRunHandle, AgentStopReason, AgentTextDeltaFuture,
        AgentTextDeltaSink, ToolLoopProgressSink,
    },
    error::LlmError,
    metrics::MetricsRecorder,
    provider::types::TokenUsage,
    provider::{
        ChatOutcome,
        tool_loop::{ToolLoopCall, ToolLoopExecutor},
    },
    tool::{ToolContext, ToolRegistry},
};

use super::session::AgentStepSession;
use super::types::AgentAttemptBaseline;
use super::types::{AgentStep, AgentToolCall, AgentToolResult};

// 只限制首个有效流事件；开始出流后由 Core 的整体请求预算接管。
const AGENT_STREAMING_FIRST_ACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);
const AGENT_NON_STREAM_STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// 运行统一 Agent Loop。
///
/// 调用方（通常是 `LlmProvider::chat_with_tools` 默认实现）提供已创建的
/// `AgentStepSession` 与工具执行依赖；本函数负责轮次推进、工具执行、最大轮数
/// 限制和最终 `ChatOutcome` 装配。
pub async fn run_agent_loop(
    session: Box<dyn AgentStepSession + Send>,
    tools: ToolRegistry,
    tool_context: ToolContext,
    max_rounds: usize,
    progress_sink: Option<ToolLoopProgressSink>,
    final_delta_sink: Option<AgentTextDeltaSink>,
) -> Result<ChatOutcome, LlmError> {
    run_agent_loop_with_handle(
        session,
        tools,
        tool_context,
        max_rounds,
        progress_sink,
        final_delta_sink,
        None,
    )
    .await
}

/// 运行统一 Agent Loop，并与 Core 共享实时轨迹和取消信号。
pub async fn run_agent_loop_with_handle(
    session: Box<dyn AgentStepSession + Send>,
    tools: ToolRegistry,
    tool_context: ToolContext,
    max_rounds: usize,
    progress_sink: Option<ToolLoopProgressSink>,
    final_delta_sink: Option<AgentTextDeltaSink>,
    run_handle: Option<AgentRunHandle>,
) -> Result<ChatOutcome, LlmError> {
    run_agent_loop_with_timeouts(
        session,
        tools,
        tool_context,
        max_rounds,
        progress_sink,
        final_delta_sink,
        run_handle,
        AGENT_STREAMING_FIRST_ACTIVITY_TIMEOUT,
        AGENT_NON_STREAM_STEP_TIMEOUT,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_agent_loop_with_timeouts(
    mut session: Box<dyn AgentStepSession + Send>,
    tools: ToolRegistry,
    tool_context: ToolContext,
    max_rounds: usize,
    progress_sink: Option<ToolLoopProgressSink>,
    final_delta_sink: Option<AgentTextDeltaSink>,
    run_handle: Option<AgentRunHandle>,
    streaming_timeout: Duration,
    non_stream_timeout: Duration,
) -> Result<ChatOutcome, LlmError> {
    if tools.is_empty() {
        return Err(LlmError::new(
            "bad_request",
            "tool loop requires at least one registered tool",
            "tool_loop",
        ));
    }
    if max_rounds == 0 {
        return Err(LlmError::new(
            "bad_request",
            "tool loop max_rounds must be positive",
            "tool_loop",
        ));
    }

    let provider = session.provider().to_owned();
    let model = session.model().to_owned();
    let recorder = MetricsRecorder::start();
    let run_handle = run_handle.unwrap_or_default();
    let attempt_baseline = run_handle.begin_attempt();
    let mut executor = ToolLoopExecutor::new(&tools, &tool_context, progress_sink);
    let mut usage: Option<TokenUsage> = None;
    let mut emitted_tools = Vec::new();
    let mut fallback_used = false;
    // 上一轮工具执行结果；首轮为空，由 Loop 在执行后回填给下一轮 advance。
    let mut results: Vec<AgentToolResult> = Vec::new();

    for round in 0..=max_rounds {
        if run_handle.is_cancelled() {
            return Err(agent_error(
                LlmError::new("cancelled", "agent run cancelled", "agent_loop"),
                &run_handle,
                &executor,
                AgentStopReason::Cancelled,
                attempt_baseline,
            ));
        }
        // model_rounds 表示已发起请求次数，包含最终超时或取消的在途请求。
        run_handle.update(|diagnostics| diagnostics.model_rounds += 1);
        // 最后一轮不允许继续工具调用；Responses 会据此设置 tool_choice=none，
        // Chat Completions 忽略此值，由下方的 max_rounds 兜底统一退出。
        let allow_tool_calls = round < max_rounds;
        let advance_future = advance_with_optional_streaming(
            session.as_mut(),
            &results,
            allow_tool_calls,
            final_delta_sink.clone(),
            streaming_timeout,
            non_stream_timeout,
            round,
        );
        let advance_future = Box::pin(advance_future);
        let cancellation = Box::pin(run_handle.cancelled());
        let advance_result = match select(advance_future, cancellation).await {
            Either::Left((result, _)) => result,
            Either::Right((_, _)) => Err(LlmError::new(
                "cancelled",
                "agent run cancelled",
                "agent_loop",
            )),
        };
        let advance = match advance_result {
            Ok(advance) => advance,
            Err(err) => {
                let reason = run_handle
                    .snapshot()
                    .stop_reason
                    .unwrap_or_else(|| stop_reason_for_error(&err));
                return Err(agent_error(
                    err,
                    &run_handle,
                    &executor,
                    reason,
                    attempt_baseline,
                ));
            }
        };
        fallback_used |= advance.fallback_used;
        if advance.fallback_used {
            run_handle.update(|diagnostics| diagnostics.streaming_fallback_used = true);
        }
        match advance.step {
            AgentStep::FinalAnswer {
                reply,
                usage: step_usage,
            } => {
                usage = merge_usage(usage, step_usage);
                debug!(
                    provider = provider.as_str(),
                    model = %model,
                    tool_loop_used = true,
                    model_rounds = run_handle.snapshot().model_rounds,
                    "agent loop completed with final reply"
                );
                return Ok(ChatOutcome {
                    reply,
                    metrics: recorder.finish(&provider, &model, false),
                    usage,
                    fallback_used,
                    agent: finish_diagnostics(
                        &run_handle,
                        &executor,
                        &emitted_tools,
                        agent_stop_reason(&emitted_tools, &executor),
                        attempt_baseline,
                    ),
                });
            }
            AgentStep::ToolCalls {
                calls,
                usage: step_usage,
            } => {
                usage = merge_usage(usage, step_usage);
                emitted_tools.extend(calls.iter().map(|call| call.name.clone()));
                run_handle.update(|diagnostics| {
                    diagnostics
                        .emitted_tools
                        .truncate(attempt_baseline.emitted_tools);
                    diagnostics.emitted_tools.extend_from_slice(&emitted_tools);
                });
                // 已到最大轮数仍要求工具调用：统一返回 tool_loop_limit，
                // 不再执行这一批调用，避免超出预算的副作用。
                if round >= max_rounds {
                    warn!(
                        provider = provider.as_str(),
                        model = %model,
                        tool_loop_used = true,
                        model_rounds = run_handle.snapshot().model_rounds,
                        max_rounds = max_rounds,
                        "agent loop exceeded maximum rounds"
                    );
                    return Err(agent_error(
                        LlmError::new(
                            "tool_loop_limit",
                            "tool loop exceeded maximum rounds",
                            "tool_loop",
                        ),
                        &run_handle,
                        &executor,
                        AgentStopReason::MaxRounds,
                        attempt_baseline,
                    ));
                }
                results =
                    execute_tool_batch(&calls, round, &mut executor, &run_handle, attempt_baseline)
                        .await
                        .map_err(|err| {
                            let reason = stop_reason_for_error(&err);
                            agent_error(err, &run_handle, &executor, reason, attempt_baseline)
                        })?;
                sync_diagnostics(&run_handle, &executor, &emitted_tools, attempt_baseline);
            }
        }
    }

    Err(agent_error(
        LlmError::new(
            "tool_loop_limit",
            "tool loop exceeded maximum rounds",
            "tool_loop",
        ),
        &run_handle,
        &executor,
        AgentStopReason::MaxRounds,
        attempt_baseline,
    ))
}

pub(super) async fn advance_with_optional_streaming(
    session: &mut (dyn AgentStepSession + Send),
    results: &[AgentToolResult],
    allow_tool_calls: bool,
    final_delta_sink: Option<AgentTextDeltaSink>,
    streaming_timeout: Duration,
    non_stream_timeout: Duration,
    round: usize,
) -> Result<AgentAdvance, LlmError> {
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
            )
            .await
        }
        StreamingAttempt::Completed(Err(err)) if !emitted_visible_delta.load(Ordering::SeqCst) => {
            let diagnostics = session.streaming_diagnostics();
            let fallback_reason = diagnostics
                .fallback_reason
                .as_deref()
                .unwrap_or_else(|| classify_streaming_error(&err));
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
) -> Result<AgentAdvance, LlmError> {
    let diagnostics = session.streaming_diagnostics();
    let fallback_started = Instant::now();
    let result =
        advance_non_stream_with_timeout(session, results, allow_tool_calls, non_stream_timeout)
            .await;
    let non_stream_fallback_elapsed_ms = fallback_started.elapsed().as_millis();
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
        chunk_count = diagnostics.chunk_count,
        sse_event_count = diagnostics.sse_event_count,
        saw_done = diagnostics.saw_done,
        saw_completed = diagnostics.saw_completed,
        buffered_delta_count = diagnostics.buffered_delta_count,
        active_function_call_count = diagnostics.active_function_call_count,
        non_stream_fallback_elapsed_ms,
        non_stream_fallback_succeeded = result.is_ok(),
        "streaming agent fallback completed"
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

fn classify_streaming_error(err: &LlmError) -> &'static str {
    if err.code == "http_error" || err.stage == "http" || err.stage == "sse" {
        "http_sse_parse_error"
    } else {
        "provider_error_other"
    }
}

fn agent_stop_reason(emitted_tools: &[String], executor: &ToolLoopExecutor<'_>) -> AgentStopReason {
    if emitted_tools.is_empty() {
        return AgentStopReason::DirectAnswer;
    }
    if executor.rejected_call() || executor.executed_tools().is_empty() {
        return AgentStopReason::Rejected;
    }
    let results = executor.tool_results();
    if results.iter().any(|result| {
        result
            .output
            .get("requires_clarification")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    }) {
        return AgentStopReason::Clarify;
    }
    if !results.is_empty() && results.iter().all(|result| !result.succeeded) {
        return AgentStopReason::Failed;
    }
    AgentStopReason::ToolUsed
}

fn stop_reason_for_error(err: &LlmError) -> AgentStopReason {
    match err.code.as_str() {
        "timeout" => AgentStopReason::Timeout,
        "cancelled" => AgentStopReason::Cancelled,
        "tool_loop_limit" => AgentStopReason::MaxRounds,
        _ => AgentStopReason::Failed,
    }
}

fn sync_diagnostics(
    run_handle: &AgentRunHandle,
    executor: &ToolLoopExecutor<'_>,
    emitted_tools: &[String],
    baseline: AgentAttemptBaseline,
) {
    run_handle.update(|diagnostics| {
        diagnostics.emitted_tools.truncate(baseline.emitted_tools);
        diagnostics.emitted_tools.extend_from_slice(emitted_tools);
        diagnostics.tool_execution_attempted |= executor.execution_attempted();
        diagnostics.executed_tools.truncate(baseline.executed_tools);
        diagnostics.executed_tools.extend(executor.executed_tools());
        diagnostics.tool_results.truncate(baseline.tool_results);
        diagnostics.tool_results.extend(executor.tool_results());
    });
}

fn finish_diagnostics(
    run_handle: &AgentRunHandle,
    executor: &ToolLoopExecutor<'_>,
    emitted_tools: &[String],
    stop_reason: AgentStopReason,
    baseline: AgentAttemptBaseline,
) -> AgentRunDiagnostics {
    sync_diagnostics(run_handle, executor, emitted_tools, baseline);
    run_handle.set_stop_reason(stop_reason);
    run_handle.snapshot()
}

fn agent_error(
    mut err: LlmError,
    run_handle: &AgentRunHandle,
    executor: &ToolLoopExecutor<'_>,
    stop_reason: AgentStopReason,
    baseline: AgentAttemptBaseline,
) -> LlmError {
    if let Some(partial) = err.agent.take() {
        run_handle.update(|diagnostics| {
            diagnostics.streaming_fallback_used |= partial.streaming_fallback_used;
        });
    }
    let snapshot = run_handle.snapshot();
    let emitted_tools = snapshot.emitted_tools[baseline.emitted_tools..].to_vec();
    err.with_agent(finish_diagnostics(
        run_handle,
        executor,
        &emitted_tools,
        stop_reason,
        baseline,
    ))
}

fn track_visible_delta_sink(
    sink: AgentTextDeltaSink,
    emitted_visible_delta: Arc<AtomicBool>,
) -> AgentTextDeltaSink {
    Arc::new(move |delta| {
        let sink = sink.clone();
        let emitted_visible_delta = emitted_visible_delta.clone();
        Box::pin(async move {
            emitted_visible_delta.store(true, Ordering::SeqCst);
            sink(delta).await
        }) as AgentTextDeltaFuture
    })
}

/// 执行同轮一批工具调用，返回回填给下一轮 `advance` 的结果。
///
/// 同轮工具调用必须先完成全部参数预绑定，再允许任何工具修改状态；Todo 的
/// 可见编号选择依赖这个边界，不能边 prepare 边执行。依赖跳过、`ok:false`
/// 业务失败识别与执行异常转结构化输出均由 `ToolLoopExecutor` 统一处理。
async fn execute_tool_batch(
    calls: &[AgentToolCall],
    round: usize,
    executor: &mut ToolLoopExecutor<'_>,
    run_handle: &AgentRunHandle,
    baseline: AgentAttemptBaseline,
) -> Result<Vec<AgentToolResult>, LlmError> {
    executor.reset_dependency_chain();
    let prepared_calls = calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            executor.prepare_call(
                ToolLoopCall {
                    name: &call.name,
                    call_id: &call.call_id,
                    arguments: &call.arguments,
                },
                round,
                index,
            )
        })
        .collect::<Vec<_>>();
    let mut results = Vec::with_capacity(calls.len());
    for (call, prepared) in calls.iter().zip(prepared_calls) {
        if run_handle.is_cancelled() {
            return Err(LlmError::new(
                "cancelled",
                "agent run cancelled before tool execution",
                "tool_loop",
            ));
        }
        let tool_results_before = executor.tool_results().len();
        let output = executor
            .execute_prepared_call(prepared, |tool_name, attempt_executed_tools| {
                run_handle.update(|diagnostics| {
                    diagnostics.executed_tools.truncate(baseline.executed_tools);
                    diagnostics
                        .executed_tools
                        .extend_from_slice(attempt_executed_tools);
                    diagnostics
                        .tools_with_unknown_result
                        .push(tool_name.to_owned());
                });
            })
            .await;
        if executor.tool_results().len() > tool_results_before {
            run_handle.update(|diagnostics| {
                if let Some(index) = diagnostics
                    .tools_with_unknown_result
                    .iter()
                    .position(|name| name == &call.name)
                {
                    diagnostics.tools_with_unknown_result.remove(index);
                }
            });
        }
        let snapshot = run_handle.snapshot();
        let emitted_tools = snapshot.emitted_tools[baseline.emitted_tools..].to_vec();
        sync_diagnostics(run_handle, executor, &emitted_tools, baseline);
        let output = output?;
        results.push(AgentToolResult {
            call_id: call.call_id.clone(),
            output: output.output,
        });
    }
    Ok(results)
}

/// 合并多轮 token 用量；任一缺失时保留另一侧。
fn merge_usage(current: Option<TokenUsage>, next: Option<TokenUsage>) -> Option<TokenUsage> {
    match (current, next) {
        (None, next) => next,
        (current, None) => current,
        (Some(left), Some(right)) => Some(TokenUsage {
            input_tokens: add_optional(left.input_tokens, right.input_tokens),
            cached_input_tokens: add_optional(left.cached_input_tokens, right.cached_input_tokens),
            output_tokens: add_optional(left.output_tokens, right.output_tokens),
            total_tokens: add_optional(left.total_tokens, right.total_tokens),
        }),
    }
}

fn add_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left + right),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

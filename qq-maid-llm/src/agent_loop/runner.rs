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

use std::time::Instant;

use futures::future::{Either, select};
use tokio::time::Duration;
use tracing::{debug, warn};

use crate::{
    agent_loop::{
        AgentRunDiagnostics, AgentRunHandle, AgentStopReason, AgentTextDeltaSink,
        ToolLoopProgressSink, tool_result_chars,
    },
    error::LlmError,
    metrics::MetricsRecorder,
    provider::types::TokenUsage,
    provider::{
        ChatOutcome,
        tool_loop::{ToolCallStartDecision, ToolLoopCall, ToolLoopExecutor},
    },
    tool::{ToolContext, ToolRegistry},
};

use super::session::AgentStepSession;
use super::streaming::{StreamingAdvanceOptions, advance_with_optional_streaming};
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
    let run_handle = run_handle.unwrap_or_default();
    let attempt_baseline = run_handle.take_candidate_attempt();
    if tools.is_empty() {
        run_handle.set_stop_reason(AgentStopReason::Failed);
        return Err(LlmError::new(
            "bad_request",
            "tool loop requires at least one registered tool",
            "tool_loop",
        )
        .with_agent(run_handle.snapshot()));
    }
    if max_rounds == 0 {
        run_handle.set_stop_reason(AgentStopReason::Failed);
        return Err(LlmError::new(
            "bad_request",
            "tool loop max_rounds must be positive",
            "tool_loop",
        )
        .with_agent(run_handle.snapshot()));
    }

    let provider = session.provider().to_owned();
    let model = session.model().to_owned();
    let recorder = MetricsRecorder::start();
    let mut executor = ToolLoopExecutor::new(&tools, &tool_context, progress_sink);
    let mut usage: Option<TokenUsage> = None;
    let mut emitted_tools = Vec::new();
    let mut fallback_used = false;
    let mut force_finalization_without_tools = false;
    // 上一轮工具执行结果；首轮为空，由 Loop 在执行后回填给下一轮 advance。
    let mut results: Vec<AgentToolResult> = Vec::new();

    for round in 0..=max_rounds {
        // model_rounds 表示已发起请求次数，包含最终超时或取消的在途请求。
        if let Err(err) = run_handle.start_model_round() {
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
        // 最后一轮或最终回答预算阶段都在协议层禁用工具；Provider 若仍返回
        // tool call，下面会直接受控终止，不能再开启模型轮次。
        let preserve_finalization_budget = force_finalization_without_tools
            || (run_handle.has_completed_tool_result_since(attempt_baseline.tool_results)
                && run_handle.should_preserve_finalization_budget());
        let allow_tool_calls = round < max_rounds && !preserve_finalization_budget;
        // Issue #361 诊断：每轮开始采样会话输入尺寸与进程内存，观察 Tool Loop
        // 多轮输入是否有界；只输出计数与尺寸，不输出正文。采样全部放进 DEBUG
        // 门控，默认级别不触碰会话上下文与 /proc 读取。
        let round_size = if tracing::enabled!(tracing::Level::DEBUG) {
            let estimate = session.input_size_estimate();
            let mem = qq_maid_common::process_mem::process_memory_sample();
            Some((estimate, mem))
        } else {
            None
        };
        debug!(
            provider = provider.as_str(),
            model = %model,
            round,
            allow_tool_calls,
            preserve_finalization_budget,
            input_item_count = round_size.as_ref().map(|(estimate, _)| estimate.item_count),
            input_estimated_chars = round_size
                .as_ref()
                .map(|(estimate, _)| estimate.estimated_chars),
            input_tool_result_chars = round_size
                .as_ref()
                .map(|(estimate, _)| estimate.tool_result_chars),
            rss_kb = round_size.as_ref().and_then(|(_, mem)| mem.rss_kb),
            vm_size_kb = round_size.as_ref().and_then(|(_, mem)| mem.vm_size_kb),
            pss_kb = round_size.as_ref().and_then(|(_, mem)| mem.pss_kb),
            private_dirty_kb = round_size.as_ref().and_then(|(_, mem)| mem.private_dirty_kb),
            remaining_budget_ms = run_handle.remaining_budget().map(|value| value.as_millis()),
            "正在开始 Agent 模型轮次"
        );
        let advance_future = advance_with_optional_streaming(
            session.as_mut(),
            &results,
            allow_tool_calls,
            StreamingAdvanceOptions {
                final_delta_sink: final_delta_sink.clone(),
                streaming_timeout,
                non_stream_timeout,
                round,
            },
            &run_handle,
        );
        let model_round_started = Instant::now();
        let advance_future = Box::pin(advance_future);
        let cancellation = Box::pin(run_handle.cancelled());
        let advance_result = if let Some(remaining_budget) = run_handle.remaining_budget() {
            let advance_or_cancel = Box::pin(async {
                match select(advance_future, cancellation).await {
                    Either::Left((result, _)) => result,
                    Either::Right((_, _)) => Err(LlmError::new(
                        "cancelled",
                        "agent run cancelled",
                        "agent_loop",
                    )),
                }
            });
            let budget = Box::pin(tokio::time::sleep(remaining_budget));
            match select(advance_or_cancel, budget).await {
                Either::Left((result, _)) => result,
                Either::Right((_, _)) => {
                    run_handle.cancel(AgentStopReason::Timeout);
                    Err(LlmError::timeout("agent_loop"))
                }
            }
        } else {
            match select(advance_future, cancellation).await {
                Either::Left((result, _)) => result,
                Either::Right((_, _)) => Err(LlmError::new(
                    "cancelled",
                    "agent run cancelled",
                    "agent_loop",
                )),
            }
        };
        debug!(
            provider = provider.as_str(),
            model = %model,
            round,
            model_round_elapsed_ms = model_round_started.elapsed().as_millis(),
            model_round_succeeded = advance_result.is_ok(),
            remaining_budget_ms = run_handle.remaining_budget().map(|value| value.as_millis()),
            "Agent 模型轮次已结束"
        );
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
                output_parts,
                usage: step_usage,
            } => {
                let step_input_tokens = step_usage.as_ref().and_then(|item| item.input_tokens);
                usage = merge_usage(usage, step_usage);
                // Issue #361 诊断：最终输入尺寸与进程内存只在 DEBUG 开启时采样，
                // 默认级别不触碰会话上下文与 /proc 读取；DEBUG 关闭时这些字段
                // 记录为空，避免 INFO 日志出现无意义的估算值。
                let final_size = if tracing::enabled!(tracing::Level::DEBUG) {
                    let estimate = session.input_size_estimate();
                    let mem = qq_maid_common::process_mem::process_memory_sample();
                    Some((estimate, mem))
                } else {
                    None
                };
                tracing::info!(
                    provider = provider.as_str(),
                    model = %model,
                    tool_loop_used = true,
                    model_rounds = run_handle.snapshot().model_rounds,
                    input_tokens = step_input_tokens,
                    input_item_count = final_size.as_ref().map(|(estimate, _)| estimate.item_count),
                    input_estimated_chars = final_size
                        .as_ref()
                        .map(|(estimate, _)| estimate.estimated_chars),
                    input_tool_result_chars = final_size
                        .as_ref()
                        .map(|(estimate, _)| estimate.tool_result_chars),
                    rss_kb = final_size.as_ref().and_then(|(_, mem)| mem.rss_kb),
                    vm_size_kb = final_size.as_ref().and_then(|(_, mem)| mem.vm_size_kb),
                    pss_kb = final_size.as_ref().and_then(|(_, mem)| mem.pss_kb),
                    private_dirty_kb = final_size.as_ref().and_then(|(_, mem)| mem.private_dirty_kb),
                    "agent_loop_request_end"
                );
                debug!(
                    provider = provider.as_str(),
                    model = %model,
                    tool_loop_used = true,
                    model_rounds = run_handle.snapshot().model_rounds,
                    "Agent Loop 已生成最终回复"
                );
                return Ok(ChatOutcome {
                    reply,
                    output_parts,
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
                let step_input_tokens = step_usage.as_ref().and_then(|item| item.input_tokens);
                usage = merge_usage(usage, step_usage);
                tracing::debug!(
                    provider = provider.as_str(),
                    model = %model,
                    round,
                    tool_call_count = calls.len(),
                    input_tokens = step_input_tokens,
                    "agent_loop_after_model_round"
                );
                emitted_tools.extend(calls.iter().map(|call| call.name.clone()));
                run_handle.update(|diagnostics| {
                    diagnostics
                        .emitted_tools
                        .truncate(attempt_baseline.emitted_tools);
                    diagnostics.emitted_tools.extend_from_slice(&emitted_tools);
                });
                if !allow_tool_calls {
                    let (code, message, reason) = if preserve_finalization_budget {
                        (
                            "tool_calls_disabled",
                            "provider returned tool calls while final answer budget disabled tools",
                            AgentStopReason::Failed,
                        )
                    } else {
                        (
                            "tool_loop_limit",
                            "tool loop returned tool calls when tool calls are disabled",
                            AgentStopReason::MaxRounds,
                        )
                    };
                    warn!(
                        provider = provider.as_str(),
                        model = %model,
                        round,
                        preserve_finalization_budget,
                        tool_call_count = calls.len(),
                        "禁用工具后 Provider 仍返回了工具调用"
                    );
                    return Err(agent_error(
                        LlmError::new(code, message, "tool_loop"),
                        &run_handle,
                        &executor,
                        reason,
                        attempt_baseline,
                    ));
                }
                // 已到最大轮数仍要求工具调用：统一返回 tool_loop_limit，
                // 不再执行这一批调用，避免超出预算的副作用。
                if round >= max_rounds {
                    warn!(
                        provider = provider.as_str(),
                        model = %model,
                        tool_loop_used = true,
                        model_rounds = run_handle.snapshot().model_rounds,
                        max_rounds = max_rounds,
                        "Agent Loop 已超过最大轮数"
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
                // 模型请求本身可能消耗掉大部分请求预算；进入工具批次前必须用同一
                // deadline 重新判断，不能沿用模型轮次开始前的旧结论。
                let batch_budget_reserved = run_handle.should_preserve_finalization_budget();
                let has_completed_result =
                    run_handle.has_completed_tool_result_since(attempt_baseline.tool_results);
                let has_successful_result =
                    run_handle.has_successful_tool_result_since(attempt_baseline.tool_results);
                if batch_budget_reserved && !has_completed_result {
                    let tool = calls
                        .first()
                        .map(|call| call.name.as_str())
                        .unwrap_or("none");
                    warn!(
                        tool,
                        round,
                        remaining_budget_ms =
                            run_handle.remaining_budget().map(|value| value.as_millis()),
                        skipped_for_finalization_reserve = true,
                        has_completed_result,
                        has_successful_result,
                        "仅剩最终回答预算，已拒绝 Agent 工具批次"
                    );
                    return Err(agent_error(
                        finalization_budget_error(),
                        &run_handle,
                        &executor,
                        AgentStopReason::Failed,
                        attempt_baseline,
                    ));
                }
                force_finalization_without_tools |= batch_budget_reserved;
                let batch = execute_tool_batch(
                    &calls,
                    round,
                    &provider,
                    &model,
                    &mut executor,
                    &run_handle,
                    attempt_baseline,
                )
                .await
                .map_err(|err| {
                    let reason = stop_reason_for_error(&err);
                    agent_error(err, &run_handle, &executor, reason, attempt_baseline)
                })?;
                results = batch.results;
                force_finalization_without_tools |= batch.skipped_for_finalization;
                sync_diagnostics(&run_handle, &executor, &emitted_tools, attempt_baseline);
                // after_tool_result：只记录本轮结果的独立体积（不 clone、不序列化）。
                // 本批结果尚未由 Provider 追加到会话 input，会话真实输入尺寸在
                // Provider `advance` 的 append 之后、payload 构造之前单独记录
                // （agent_loop_input_after_append），避免把“未追加”误标为“追加后”。
                // 整段诊断计算放在 DEBUG 门控内：默认级别不触碰大型 Tool Result。
                if tracing::enabled!(tracing::Level::DEBUG) {
                    let result_chars = tool_result_chars(&results);
                    let result_mem = qq_maid_common::process_mem::process_memory_sample();
                    tracing::debug!(
                        provider = provider.as_str(),
                        model = %model,
                        round,
                        tool_result_count = results.len(),
                        tool_result_chars = result_chars,
                        rss_kb = result_mem.rss_kb,
                        vm_size_kb = result_mem.vm_size_kb,
                        pss_kb = result_mem.pss_kb,
                        private_dirty_kb = result_mem.private_dirty_kb,
                        "after_tool_result"
                    );
                }
                // 工具启动时预算可能充足，但执行完成后已经进入最终回答预留区。
                // 此时必须基于刚同步的真实结果重新判断，不能沿用批次启动前的状态。
                let preserve_after_batch = run_handle.should_preserve_finalization_budget();
                let has_completed_result_after_batch =
                    run_handle.has_completed_tool_result_since(attempt_baseline.tool_results);
                let has_successful_result_after_batch =
                    run_handle.has_successful_tool_result_since(attempt_baseline.tool_results);
                debug!(
                    round,
                    has_completed_result_after_batch,
                    has_successful_result_after_batch,
                    "已完成 Agent 工具结果的完成与成功状态分类"
                );
                if preserve_after_batch {
                    if has_completed_result_after_batch {
                        force_finalization_without_tools = true;
                    } else {
                        warn!(
                            round,
                            remaining_budget_ms =
                                run_handle.remaining_budget().map(|value| value.as_millis()),
                            has_completed_result = false,
                            "Agent 工具批次已耗尽工具预算，且没有已完成的结果"
                        );
                        return Err(agent_error(
                            finalization_budget_error(),
                            &run_handle,
                            &executor,
                            AgentStopReason::Failed,
                            attempt_baseline,
                        ));
                    }
                }
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
        // ToolLoopExecutor 内 result_index / retry_of 是候选局部下标；累计
        // diagnostics 需要换成全局下标，否则跨 Provider 候选时会误指向前一个
        // 候选的结果。tool_attempts 长度也独立截断，不假设与 tool_results 相等。
        diagnostics.tool_attempts.truncate(baseline.tool_attempts);
        diagnostics
            .tool_attempts
            .extend(executor.tool_attempts().into_iter().map(|mut attempt| {
                attempt.result_index += baseline.tool_results;
                if let Some(retry_of) = attempt.retry_of.as_mut() {
                    *retry_of += baseline.tool_results;
                }
                attempt
            }));
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

/// 执行同轮一批工具调用，返回回填给下一轮 `advance` 的结果。
///
/// 同轮工具调用必须先完成全部参数预绑定，再允许任何工具修改状态；Todo 的
/// 可见编号选择依赖这个边界，不能边 prepare 边执行。依赖跳过、`ok:false`
/// 业务失败识别与执行异常转结构化输出均由 `ToolLoopExecutor` 统一处理。
async fn execute_tool_batch(
    calls: &[AgentToolCall],
    round: usize,
    provider: &str,
    model: &str,
    executor: &mut ToolLoopExecutor<'_>,
    run_handle: &AgentRunHandle,
    baseline: AgentAttemptBaseline,
) -> Result<ToolBatchOutcome, LlmError> {
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
                calls.len(),
                run_handle.tool_execution_deadline(),
            )
        })
        .collect::<Vec<_>>();
    executor.begin_batch();
    let mut results = Vec::with_capacity(calls.len());
    let mut stop_remaining_batch = false;
    let mut skipped_for_finalization = false;
    for (call, prepared) in calls.iter().zip(prepared_calls) {
        let tool_started_at = Instant::now();
        let output = executor
            .execute_prepared_call(
                prepared,
                |tool_name, _effect| {
                    if stop_remaining_batch {
                        return Ok(ToolCallStartDecision::SkipForFinalAnswer);
                    }
                    let has_completed_result =
                        run_handle.has_completed_tool_result_since(baseline.tool_results);
                    let reserve_reached = run_handle.should_preserve_finalization_budget();
                    debug!(
                        tool = tool_name,
                        round,
                        remaining_budget_ms =
                            run_handle.remaining_budget().map(|value| value.as_millis()),
                        skipped_for_finalization_reserve = reserve_reached,
                        has_completed_result,
                        "已检查 Agent 工具启动预算"
                    );
                    if !reserve_reached {
                        return Ok(ToolCallStartDecision::Execute);
                    }
                    if has_completed_result {
                        Ok(ToolCallStartDecision::SkipForFinalAnswer)
                    } else {
                        Err(finalization_budget_error())
                    }
                },
                |tool_name, effect| run_handle.try_start_tool(tool_name, effect),
                |result| run_handle.record_tool_result(result),
            )
            .await;
        let tool_duration_ms = tool_started_at.elapsed().as_millis();
        debug!(
            tool = call.name,
            round,
            tool_elapsed_ms = tool_duration_ms,
            tool_succeeded = output.is_ok(),
            remaining_budget_ms = run_handle.remaining_budget().map(|value| value.as_millis()),
            "Agent 工具调用已结束"
        );
        let snapshot = run_handle.snapshot();
        let emitted_tools = snapshot.emitted_tools[baseline.emitted_tools..].to_vec();
        sync_diagnostics(run_handle, executor, &emitted_tools, baseline);
        let output = output?;
        log_structured_tool_failure(
            call,
            round,
            provider,
            model,
            tool_duration_ms,
            &output.output,
        );
        skipped_for_finalization |= output.skipped_for_finalization;
        stop_remaining_batch |= output.stop_remaining_batch;
        results.push(AgentToolResult {
            call_id: call.call_id.clone(),
            output: output.output,
        });
    }
    executor.finish_batch();
    Ok(ToolBatchOutcome {
        results,
        skipped_for_finalization,
    })
}

fn log_structured_tool_failure(
    call: &AgentToolCall,
    round: usize,
    agent_provider: &str,
    agent_model: &str,
    duration_ms: u128,
    output: &str,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return;
    };
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(false) {
        return;
    }
    let error = value.get("error").unwrap_or(&serde_json::Value::Null);
    warn!(
        tool_name = call.name.as_str(),
        tool_call_id = call.call_id.as_str(),
        attempt = value
            .get("attempts")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(round as u64 + 1),
        duration_ms = duration_ms.min(u128::from(u64::MAX)) as u64,
        error_kind = error
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("internal_error"),
        retriable = error
            .get("retriable")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        backend = value
            .get("backend")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown"),
        upstream_status = ?error.get("upstream_status").and_then(serde_json::Value::as_u64),
        provider = value
            .get("provider")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(agent_provider),
        model = value
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(agent_model),
        failure_layer = error
            .get("stage")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("tool_loop"),
        "Agent 工具调用失败"
    );
}

struct ToolBatchOutcome {
    results: Vec<AgentToolResult>,
    skipped_for_finalization: bool,
}

fn finalization_budget_error() -> LlmError {
    LlmError::new(
        "request_budget_reserved_for_final_answer",
        "request budget is insufficient to start a tool and no trusted tool result is available",
        "tool_loop",
    )
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

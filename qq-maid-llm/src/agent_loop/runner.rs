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
    atomic::{AtomicBool, Ordering},
};

use tracing::{debug, warn};

use crate::{
    agent_loop::{AgentTextDeltaFuture, AgentTextDeltaSink, ToolLoopProgressSink},
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
use super::types::{AgentStep, AgentToolCall, AgentToolResult};

/// 运行统一 Agent Loop。
///
/// 调用方（通常是 `LlmProvider::chat_with_tools` 默认实现）提供已创建的
/// `AgentStepSession` 与工具执行依赖；本函数负责轮次推进、工具执行、最大轮数
/// 限制和最终 `ChatOutcome` 装配。
pub async fn run_agent_loop(
    mut session: Box<dyn AgentStepSession + Send>,
    tools: ToolRegistry,
    tool_context: ToolContext,
    max_rounds: usize,
    progress_sink: Option<ToolLoopProgressSink>,
    final_delta_sink: Option<AgentTextDeltaSink>,
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
    let mut executor = ToolLoopExecutor::new(&tools, &tool_context, progress_sink);
    let mut usage: Option<TokenUsage> = None;
    // 上一轮工具执行结果；首轮为空，由 Loop 在执行后回填给下一轮 advance。
    let mut results: Vec<AgentToolResult> = Vec::new();

    for round in 0..=max_rounds {
        // 最后一轮不允许继续工具调用；Responses 会据此设置 tool_choice=none，
        // Chat Completions 忽略此值，由下方的 max_rounds 兜底统一退出。
        let allow_tool_calls = round < max_rounds;
        let step = advance_with_optional_streaming(
            session.as_mut(),
            &results,
            allow_tool_calls,
            final_delta_sink.clone(),
        )
        .await?;
        match step {
            AgentStep::FinalAnswer {
                reply,
                usage: step_usage,
            } => {
                usage = merge_usage(usage, step_usage);
                debug!(
                    provider = provider.as_str(),
                    model = %model,
                    tool_loop_used = true,
                    tool_loop_rounds = round,
                    "agent loop completed with final reply"
                );
                return Ok(ChatOutcome {
                    reply,
                    metrics: recorder.finish(&provider, &model, false),
                    usage,
                    fallback_used: false,
                    executed_tools: executor.executed_tools(),
                    tool_results: executor.tool_results(),
                });
            }
            AgentStep::ToolCalls {
                calls,
                usage: step_usage,
            } => {
                usage = merge_usage(usage, step_usage);
                // 已到最大轮数仍要求工具调用：统一返回 tool_loop_limit，
                // 不再执行这一批调用，避免超出预算的副作用。
                if round >= max_rounds {
                    warn!(
                        provider = provider.as_str(),
                        model = %model,
                        tool_loop_used = true,
                        tool_loop_rounds = round,
                        max_rounds = max_rounds,
                        "agent loop exceeded maximum rounds"
                    );
                    return Err(LlmError::new(
                        "tool_loop_limit",
                        "tool loop exceeded maximum rounds",
                        "tool_loop",
                    ));
                }
                results = execute_tool_batch(&calls, round, &mut executor).await?;
            }
        }
    }

    Err(LlmError::new(
        "tool_loop_limit",
        "tool loop exceeded maximum rounds",
        "tool_loop",
    ))
}

async fn advance_with_optional_streaming(
    session: &mut (dyn AgentStepSession + Send),
    results: &[AgentToolResult],
    allow_tool_calls: bool,
    final_delta_sink: Option<AgentTextDeltaSink>,
) -> Result<AgentStep, LlmError> {
    let Some(sink) = final_delta_sink else {
        return session.advance(results, allow_tool_calls).await;
    };
    let emitted_visible_delta = Arc::new(AtomicBool::new(false));
    let tracked_sink = track_visible_delta_sink(sink, emitted_visible_delta.clone());
    match session
        .advance_streaming(results, allow_tool_calls, tracked_sink)
        .await
    {
        Ok(Some(step)) => Ok(step),
        Ok(None) => session.advance(results, allow_tool_calls).await,
        Err(err) if !emitted_visible_delta.load(Ordering::SeqCst) => {
            debug!(
                provider = session.provider(),
                model = %session.model(),
                allow_tool_calls,
                error_code = err.code.as_str(),
                error_stage = err.stage.as_str(),
                "streaming agent advance failed before visible delta; falling back to non-stream advance"
            );
            session.advance(results, allow_tool_calls).await
        }
        Err(err) => Err(err),
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
        let output = executor.execute_prepared_call(prepared).await?;
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

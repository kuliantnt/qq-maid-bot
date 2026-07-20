//! Tool Loop 内部执行语义。
//!
//! Provider 只负责各自协议的 payload、工具调用解析和结果回填格式；
//! 工具准备、执行失败、依赖跳过、结果轨迹和稳定调用 ID 在这里统一维护，
//! 避免 Responses 与 Chat Completions 两条协议分支各自漂移。

use std::collections::HashMap;

use serde_json::{Value, json};
use tracing::debug;

use crate::{
    agent_loop::{ToolLoopProgressEvent, ToolLoopProgressSink},
    error::LlmError,
    provider::{ToolExecutionAttempt, ToolExecutionResult},
    tool::{PreparedToolCall, ToolCallDependency, ToolContext, ToolEffect, ToolRegistry},
};

pub(crate) struct ToolLoopExecutor<'a> {
    tools: &'a ToolRegistry,
    base_context: &'a ToolContext,
    previous_call_succeeded: bool,
    executed_tools: Vec<String>,
    tool_results: Vec<ToolExecutionResult>,
    tool_attempts: Vec<ToolExecutionAttempt>,
    progress_sink: Option<ToolLoopProgressSink>,
    execution_attempted: bool,
    rejected_call: bool,
    completed_read_only_calls: HashMap<String, String>,
    call_counts: HashMap<String, usize>,
    last_batch: Vec<BatchAttempt>,
    current_batch: Vec<BatchAttempt>,
}

pub(crate) struct ToolLoopCall<'a> {
    pub(crate) name: &'a str,
    pub(crate) call_id: &'a str,
    pub(crate) arguments: &'a str,
}

pub(crate) struct ToolLoopCallOutput {
    pub(crate) output: String,
    pub(crate) skipped_for_finalization: bool,
}

pub(crate) enum ToolCallStartDecision {
    Execute,
    SkipForFinalAnswer,
}

pub(crate) struct PreparedToolLoopCall {
    tool_name: String,
    prepared: Result<PreparedToolCall, LlmError>,
    call_id: String,
    round: usize,
    batch_len: usize,
}

#[derive(Debug, Clone)]
struct BatchAttempt {
    result_index: usize,
    call_id: String,
    name: String,
    arguments: Value,
    succeeded: bool,
    executed: bool,
    round: usize,
}

impl<'a> ToolLoopExecutor<'a> {
    pub(crate) fn new(
        tools: &'a ToolRegistry,
        base_context: &'a ToolContext,
        progress_sink: Option<ToolLoopProgressSink>,
    ) -> Self {
        Self {
            tools,
            base_context,
            previous_call_succeeded: true,
            executed_tools: Vec::new(),
            tool_results: Vec::new(),
            progress_sink,
            execution_attempted: false,
            rejected_call: false,
            completed_read_only_calls: HashMap::new(),
            call_counts: HashMap::new(),
            tool_attempts: Vec::new(),
            last_batch: Vec::new(),
            current_batch: Vec::new(),
        }
    }

    pub(crate) fn reset_dependency_chain(&mut self) {
        self.previous_call_succeeded = true;
    }

    pub(crate) fn prepare_call(
        &mut self,
        call: ToolLoopCall<'_>,
        round: usize,
        index: usize,
        batch_len: usize,
        execution_deadline: Option<tokio::time::Instant>,
    ) -> PreparedToolLoopCall {
        self.execution_attempted = true;
        let mut context = self.base_context.clone();
        context.tool_call_id = Some(stable_tool_call_id(
            &context.task_id,
            call.call_id,
            round,
            index,
        ));
        context.execution_deadline = execution_deadline;
        PreparedToolLoopCall {
            tool_name: call.name.to_owned(),
            prepared: self.tools.prepare_json(&context, call.name, call.arguments),
            call_id: call.call_id.to_owned(),
            round,
            batch_len,
        }
    }

    pub(crate) fn begin_batch(&mut self) {
        self.current_batch.clear();
    }

    pub(crate) fn finish_batch(&mut self) {
        self.last_batch = std::mem::take(&mut self.current_batch);
    }

    pub(crate) async fn execute_prepared_call(
        &mut self,
        call: PreparedToolLoopCall,
        before_start: impl FnOnce(&str, ToolEffect) -> Result<ToolCallStartDecision, LlmError>,
        on_started: impl FnOnce(&str, ToolEffect) -> Result<(), LlmError>,
        on_result: impl FnOnce(ToolExecutionResult),
    ) -> Result<ToolLoopCallOutput, LlmError> {
        let PreparedToolLoopCall {
            tool_name: requested_tool_name,
            prepared,
            call_id,
            round,
            batch_len,
        } = call;
        let mut skipped_for_finalization = false;
        let mut tool_started = false;
        let prepared_arguments = prepared
            .as_ref()
            .ok()
            .map(|call| (call.name.clone(), call.arguments.clone()));
        let retry_of = self.retry_parent(
            &call_id,
            round,
            batch_len,
            prepared_arguments
                .as_ref()
                .map(|(name, arguments)| (name.as_str(), arguments)),
        );
        let (tool_name, output, succeeded) = match prepared {
            Ok(prepared) => {
                let tool_name = prepared.name.clone();
                let read_only_key = prepared
                    .deduplication_key
                    .as_ref()
                    .map(|key| format!("{}:{key}", prepared.name));
                if prepared.max_calls_per_request.is_some_and(|limit| {
                    self.call_counts.get(&tool_name).copied().unwrap_or(0) >= limit
                }) {
                    // 达到请求级上限不是工具故障；返回结构化提示，让模型在下一轮
                    // 基于已有证据收尾，而不是重试同一个查询。
                    tracing::warn!(
                        tool = %tool_name,
                        calls = self.call_counts.get(&tool_name).copied().unwrap_or(0),
                        max_calls = prepared.max_calls_per_request,
                        force_finalization = true,
                        "tool request limit reached"
                    );
                    skipped_for_finalization = true;
                    (
                        tool_name,
                        tool_limit_output(prepared.max_calls_per_request.unwrap_or(0)),
                        false,
                    )
                } else if let Some(cached_output) = read_only_key
                    .as_ref()
                    .and_then(|key| self.completed_read_only_calls.get(key))
                {
                    // 缓存只保存已明确成功的只读结果；命中只回传紧凑引用，避免把
                    // 完整证据再次写入上下文。缓存命中仍计入请求级调用次数。
                    debug!(tool = %tool_name, "agent read-only tool cache hit");
                    *self.call_counts.entry(tool_name.clone()).or_default() += 1;
                    (tool_name, compact_cached_output(cached_output), true)
                } else if prepared.dependency == ToolCallDependency::PreviousCallSuccess
                    && !self.previous_call_succeeded
                {
                    (
                        tool_name,
                        tool_skip_output("dependency_previous_call_failed"),
                        false,
                    )
                } else {
                    match before_start(&tool_name, prepared.effect)? {
                        ToolCallStartDecision::SkipForFinalAnswer => {
                            skipped_for_finalization = true;
                            (
                                tool_name,
                                tool_skip_output("request_budget_reserved_for_final_answer"),
                                false,
                            )
                        }
                        ToolCallStartDecision::Execute => {
                            *self.call_counts.entry(tool_name.clone()).or_default() += 1;
                            tool_started = true;
                            self.emit_progress(ToolLoopProgressEvent::ToolCallStarted {
                                tool_name: tool_name.clone(),
                            })
                            .await?;
                            // progress await 返回后仍需在共享生命周期锁内重新检查取消；只有
                            // 原子启动转换成功，才创建工具 future 并越过副作用边界。
                            on_started(&tool_name, prepared.effect)?;
                            if prepared.effect == ToolEffect::SideEffecting {
                                // 写操作可能改变后续查询结果；只读去重只能跨越没有状态变化的
                                // 连续查询段，不能让“查询 -> 修改 -> 再查询”复用旧判断。
                                self.completed_read_only_calls.clear();
                            }
                            self.executed_tools.push(tool_name.clone());
                            match self.tools.execute_prepared(prepared).await {
                                Ok(output) => {
                                    let succeeded = tool_output_indicates_success(&output);
                                    if succeeded && let Some(key) = read_only_key {
                                        self.completed_read_only_calls.insert(key, output.clone());
                                    }
                                    (tool_name, output, succeeded)
                                }
                                Err(err) => (tool_name, tool_error_output(&err), false),
                            }
                        }
                    }
                }
            }
            Err(err) => {
                self.rejected_call = true;
                (requested_tool_name, tool_error_output(&err), false)
            }
        };
        self.previous_call_succeeded = succeeded;
        let event = if succeeded {
            ToolLoopProgressEvent::ToolCallFinished {
                tool_name: tool_name.clone(),
            }
        } else {
            ToolLoopProgressEvent::ToolCallFailed {
                tool_name: tool_name.clone(),
            }
        };
        let result = tool_execution_result(&tool_name, &output, succeeded);
        let result_index = self.tool_results.len();
        self.tool_results.push(result.clone());
        self.tool_attempts.push(ToolExecutionAttempt {
            result_index,
            call_id: call_id.clone(),
            round,
            retry_of,
        });
        if let Some((name, arguments)) = prepared_arguments {
            self.current_batch.push(BatchAttempt {
                result_index,
                call_id,
                name,
                arguments,
                succeeded,
                executed: tool_started,
                round,
            });
        }
        // 工具已经完成后先落可信轨迹，再通知上层；receiver 此时关闭不能抹掉结果。
        on_result(result);
        self.emit_progress(event).await?;
        Ok(ToolLoopCallOutput {
            output,
            skipped_for_finalization,
        })
    }

    pub(crate) fn executed_tools(&self) -> Vec<String> {
        self.executed_tools.clone()
    }

    pub(crate) fn tool_results(&self) -> Vec<ToolExecutionResult> {
        self.tool_results.clone()
    }

    pub(crate) fn tool_attempts(&self) -> Vec<ToolExecutionAttempt> {
        self.tool_attempts.clone()
    }

    pub(crate) fn execution_attempted(&self) -> bool {
        self.execution_attempted
    }

    pub(crate) fn rejected_call(&self) -> bool {
        self.rejected_call
    }

    async fn emit_progress(&self, event: ToolLoopProgressEvent) -> Result<(), LlmError> {
        let Some(sink) = &self.progress_sink else {
            return Ok(());
        };
        // progress sink 是 Core stream 的取消边界：返回 Err 表示上层不再消费事件，
        // 继续执行工具可能产生无人接收的副作用，因此必须把错误向外传播。
        sink(event).await
    }

    fn retry_parent(
        &self,
        call_id: &str,
        round: usize,
        batch_len: usize,
        prepared: Option<(&str, &Value)>,
    ) -> Option<usize> {
        // Provider 协议没有统一的 parent-call 字段。只有“上一轮单个调用失败、
        // 当前轮仍是单个同工具调用”才建立保守的重试关系；没有复用 call_id 时还
        // 要求参数完全一致。同轮多个调用和跨轮不同参数调用均保持独立，避免按
        // 工具名或展示文本误合并。
        if batch_len != 1 || self.last_batch.len() != 1 {
            return None;
        }
        let previous = &self.last_batch[0];
        let (name, arguments) = prepared?;
        if previous.succeeded
            || !previous.executed
            || previous.round + 1 != round
            || previous.name != name
        {
            return None;
        }
        // 优先使用 provider 复用的真实 call_id；不同协议的重试通常会生成新 ID，
        // 此时才退回到严格的单例批次 + 同参数边界。
        let same_call_id = !call_id.trim().is_empty() && previous.call_id == call_id;
        if !same_call_id && previous.arguments != *arguments {
            return None;
        }
        Some(previous.result_index)
    }
}

fn stable_tool_call_id(task_id: &str, call_id: &str, round: usize, index: usize) -> String {
    let call_id = call_id.trim();
    if !call_id.is_empty() {
        format!("{task_id}:{call_id}")
    } else {
        // 兼容上游未返回稳定 call_id 的场景，回退到 request + round + index。
        format!("{task_id}:round-{round}:call-{index}")
    }
}

fn tool_error_output(err: &LlmError) -> String {
    serde_json::to_string(&json!({
        "ok": false,
        "error": {
            "code": err.code,
            "message": err.message,
            "stage": err.stage,
        }
    }))
    .unwrap_or_else(|_| r#"{"ok":false,"error":{"code":"tool_output_error","message":"failed to serialize tool error","stage":"tool_loop"}}"#.to_owned())
}

fn tool_skip_output(reason: &str) -> String {
    serde_json::to_string(&json!({
        "ok": false,
        "skipped": true,
        "reason": reason,
    }))
    .unwrap_or_else(|_| {
        r#"{"ok":false,"skipped":true,"reason":"dependency_previous_call_failed"}"#.to_owned()
    })
}

fn tool_output_indicates_success(output: &str) -> bool {
    // 业务工具失败统一约定为 {"ok":false,...}；这里不理解具体业务字段，
    // 只把明确失败用于依赖跳过和通用执行轨迹。
    serde_json::from_str::<Value>(output)
        .ok()
        .and_then(|value| value.get("ok").and_then(Value::as_bool))
        .unwrap_or(true)
}

fn tool_execution_result(name: &str, output: &str, succeeded: bool) -> ToolExecutionResult {
    let output = serde_json::from_str::<Value>(output).unwrap_or_else(|_| json!(output));
    ToolExecutionResult {
        name: name.to_owned(),
        output,
        succeeded,
    }
}

fn compact_cached_output(output: &str) -> String {
    // 保留成功语义和去重标记，不重复注入首次检索的完整证据。
    serde_json::to_string(&json!({
        "ok": true,
        "deduplicated": true,
        "message": "已使用本次请求中相同检索的已有证据。",
    }))
    .unwrap_or_else(|_| output.to_owned())
}

fn tool_limit_output(limit: usize) -> String {
    serde_json::to_string(&json!({
        "ok": false,
        "error_code": "tool_call_limit",
        "limit": limit,
        "message": "本次请求的知识检索次数已达上限，请基于已有证据直接回答。",
    }))
    .unwrap_or_else(|_| r#"{"ok":false,"error_code":"tool_call_limit"}"#.to_owned())
}

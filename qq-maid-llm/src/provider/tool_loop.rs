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
    provider::ToolExecutionResult,
    tool::{PreparedToolCall, ToolCallDependency, ToolContext, ToolEffect, ToolRegistry},
};

pub(crate) struct ToolLoopExecutor<'a> {
    tools: &'a ToolRegistry,
    base_context: &'a ToolContext,
    previous_call_succeeded: bool,
    executed_tools: Vec<String>,
    tool_results: Vec<ToolExecutionResult>,
    progress_sink: Option<ToolLoopProgressSink>,
    execution_attempted: bool,
    rejected_call: bool,
    completed_read_only_calls: HashMap<String, String>,
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
    ) -> PreparedToolLoopCall {
        self.execution_attempted = true;
        let mut context = self.base_context.clone();
        context.tool_call_id = Some(stable_tool_call_id(
            &context.task_id,
            call.call_id,
            round,
            index,
        ));
        PreparedToolLoopCall {
            tool_name: call.name.to_owned(),
            prepared: self.tools.prepare_json(&context, call.name, call.arguments),
        }
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
        } = call;
        let mut skipped_for_finalization = false;
        let (tool_name, output, succeeded) = match prepared {
            Ok(prepared) => {
                let tool_name = prepared.name.clone();
                let read_only_key = prepared
                    .deduplication_key
                    .as_ref()
                    .map(|key| format!("{}:{key}", prepared.name));
                if let Some(cached_output) = read_only_key
                    .as_ref()
                    .and_then(|key| self.completed_read_only_calls.get(key))
                {
                    // 缓存只保存已明确成功的只读结果；回放原始输出，避免模型把去重
                    // 误判为失败，也不能把缓存命中计作一次真实工具执行。
                    debug!(tool = %tool_name, "agent read-only tool cache hit");
                    (tool_name, cached_output.clone(), true)
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
        self.tool_results.push(result.clone());
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

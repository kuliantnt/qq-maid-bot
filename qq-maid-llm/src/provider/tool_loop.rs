//! Tool Loop 内部执行语义。
//!
//! Provider 只负责各自协议的 payload、工具调用解析和结果回填格式；
//! 工具准备、执行失败、依赖跳过、结果轨迹和稳定调用 ID 在这里统一维护，
//! 避免 Responses 与 Chat Completions 两条协议分支各自漂移。

use serde_json::{Value, json};

use crate::{
    agent_loop::{ToolLoopProgressEvent, ToolLoopProgressSink},
    error::LlmError,
    provider::ToolExecutionResult,
    tool::{PreparedToolCall, ToolCallDependency, ToolContext, ToolRegistry},
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
}

pub(crate) struct ToolLoopCall<'a> {
    pub(crate) name: &'a str,
    pub(crate) call_id: &'a str,
    pub(crate) arguments: &'a str,
}

pub(crate) struct ToolLoopCallOutput {
    pub(crate) output: String,
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
    ) -> Result<ToolLoopCallOutput, LlmError> {
        let PreparedToolLoopCall {
            tool_name: requested_tool_name,
            prepared,
        } = call;
        let (tool_name, output, succeeded) = match prepared {
            Ok(prepared) => {
                let tool_name = prepared.name.clone();
                if prepared.dependency == ToolCallDependency::PreviousCallSuccess
                    && !self.previous_call_succeeded
                {
                    (
                        tool_name,
                        tool_skip_output("dependency_previous_call_failed"),
                        false,
                    )
                } else {
                    self.emit_progress(ToolLoopProgressEvent::ToolCallStarted {
                        tool_name: tool_name.clone(),
                    })
                    .await?;
                    self.executed_tools.push(tool_name.clone());
                    match self.tools.execute_prepared(prepared).await {
                        Ok(output) => {
                            let succeeded = tool_output_indicates_success(&output);
                            (tool_name, output, succeeded)
                        }
                        Err(err) => (tool_name, tool_error_output(&err), false),
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
        self.tool_results
            .push(tool_execution_result(&tool_name, &output, succeeded));
        // 工具已经完成后先落可信轨迹，再通知上层；receiver 此时关闭不能抹掉结果。
        self.emit_progress(event).await?;
        Ok(ToolLoopCallOutput { output })
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

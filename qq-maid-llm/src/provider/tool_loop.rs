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
    completed_read_only_calls: HashMap<String, (usize, String)>,
    /// 工具显式声明可缓存的确定性失败，在同一请求内不会因模型再次调用而重复执行。
    terminal_read_only_failures: HashMap<String, String>,
    execution_counts: HashMap<String, usize>,
    last_batch: Vec<BatchAttempt>,
    /// 只跨越同一成功查询的冗余续调；新步骤、写操作或批量调用会清除锚点。
    continuation_anchor: Option<BatchAttempt>,
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
    /// 预算保护才会阻止同批次后续调用；单个工具达到请求上限不能影响其他工具。
    pub(crate) stop_remaining_batch: bool,
}

pub(crate) enum ToolCallStartDecision {
    Execute,
    SkipForFinalAnswer,
}

/// 控制用户进度事件，与 diagnostics 中是否记录 attempt/result 分离。
///
/// 缓存命中仍是一条完整 Agent 轨迹，但没有真实执行，不能伪造 Started/Finished；
/// 参数错误、预算拒绝等预执行失败则仍需要 Failed 事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolCallProgressDisposition {
    Executed,
    CacheHit,
    FailedBeforeExecution,
}

pub(crate) struct PreparedToolLoopCall {
    tool_name: String,
    prepared: Result<PreparedToolCall, LlmError>,
    call_id: String,
    round: usize,
    batch_len: usize,
    raw_arguments: String,
}

#[derive(Debug, Clone)]
struct BatchAttempt {
    result_index: usize,
    call_id: String,
    name: String,
    arguments: Value,
    execution_succeeded: bool,
    read_only: bool,
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
            terminal_read_only_failures: HashMap::new(),
            execution_counts: HashMap::new(),
            tool_attempts: Vec::new(),
            last_batch: Vec::new(),
            continuation_anchor: None,
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
            raw_arguments: call.arguments.to_owned(),
        }
    }

    pub(crate) fn begin_batch(&mut self) {
        self.current_batch.clear();
    }

    pub(crate) fn finish_batch(&mut self) {
        self.continuation_anchor = match self.current_batch.as_slice() {
            [call]
                if call.read_only
                    && call.executed
                    && self.tool_results[call.result_index].succeeded =>
            {
                Some(call.clone())
            }
            [call]
                if self.tool_attempts.last().is_some_and(|attempt| {
                    attempt.result_index == call.result_index
                        && attempt.redundant_of.is_some()
                        && attempt.redundant_of
                            == self
                                .continuation_anchor
                                .as_ref()
                                .map(|anchor| anchor.result_index)
                }) =>
            {
                self.continuation_anchor.take()
            }
            _ => None,
        };
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
            mut prepared,
            call_id,
            round,
            batch_len,
            raw_arguments,
        } = call;
        let mut skipped_for_finalization = false;
        let mut stop_remaining_batch = false;
        let mut tool_started = false;
        let mut redundant_of = None;
        let read_only = prepared
            .as_ref()
            .is_ok_and(|call| call.effect == ToolEffect::ReadOnly);
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
        if let Ok(prepared) = prepared.as_mut() {
            // 轮次与重试关系属于执行器协议元数据；业务 Tool 只读取它们做低敏诊断，
            // 不得据此改变领域执行语义。
            prepared.context.tool_round = Some(round);
            prepared.context.retry_of = retry_of;
        }
        let cache_terminal_failures = self.tools.caches_terminal_failures(&requested_tool_name);
        let terminal_failure_key = match prepared.as_ref() {
            Ok(prepared)
                if prepared.effect == ToolEffect::ReadOnly && prepared.cache_terminal_failures =>
            {
                prepared
                    .deduplication_key
                    .as_ref()
                    .map(|key| format!("{}:{key}", prepared.name))
            }
            Err(_) if cache_terminal_failures => {
                Some(format!("{requested_tool_name}:raw:{raw_arguments}"))
            }
            _ => None,
        };
        let (tool_name, output, domain_succeeded, execution_succeeded, progress_disposition) =
            match prepared {
                Ok(prepared) => {
                    let tool_name = prepared.name.clone();
                    let read_only_key = prepared
                        .deduplication_key
                        .as_ref()
                        .map(|key| format!("{}:{key}", prepared.name));
                    if let Some(cached_output) = read_only_key
                        .as_ref()
                        .and_then(|key| self.terminal_read_only_failures.get(key))
                    {
                        debug!(tool = %tool_name, "已抑制 Agent 只读工具的终态失败");
                        (
                            tool_name,
                            compact_terminal_failure(cached_output),
                            false,
                            false,
                            ToolCallProgressDisposition::CacheHit,
                        )
                    } else if let Some((result_index, cached_output)) = read_only_key
                        .as_ref()
                        .and_then(|key| self.completed_read_only_calls.get(key))
                    {
                        // 缓存只保存已完成的只读结果；命中只回传紧凑引用，避免把
                        // 完整证据再次写入上下文。缓存命中不增加真实执行次数。
                        debug!(tool = %tool_name, "Agent 只读工具命中缓存");
                        redundant_of = Some(*result_index);
                        let output = compact_cached_output(cached_output);
                        (
                            tool_name,
                            output.clone(),
                            tool_output_indicates_domain_success(&output),
                            true,
                            ToolCallProgressDisposition::CacheHit,
                        )
                    } else if prepared.max_calls_per_request.is_some_and(|limit| {
                        self.execution_counts.get(&tool_name).copied().unwrap_or(0) >= limit
                    }) {
                        // 达到请求级上限只拒绝当前工具调用；同批次其他工具仍需执行。
                        // 下一轮由上层根据该标记切换到无工具最终回答。
                        tracing::warn!(
                            tool = %tool_name,
                            executions = self.execution_counts.get(&tool_name).copied().unwrap_or(0),
                            max_executions = prepared.max_calls_per_request,
                            force_finalization = true,
                            "工具执行次数已达上限"
                        );
                        skipped_for_finalization = true;
                        (
                            tool_name,
                            tool_limit_output(prepared.max_calls_per_request.unwrap_or(0)),
                            false,
                            false,
                            ToolCallProgressDisposition::FailedBeforeExecution,
                        )
                    } else if prepared.dependency == ToolCallDependency::PreviousCallSuccess
                        && !self.previous_call_succeeded
                    {
                        (
                            tool_name,
                            tool_skip_output("dependency_previous_call_failed"),
                            false,
                            false,
                            ToolCallProgressDisposition::FailedBeforeExecution,
                        )
                    } else {
                        match before_start(&tool_name, prepared.effect)? {
                            ToolCallStartDecision::SkipForFinalAnswer => {
                                skipped_for_finalization = true;
                                stop_remaining_batch = true;
                                (
                                    tool_name,
                                    tool_skip_output("request_budget_reserved_for_final_answer"),
                                    false,
                                    false,
                                    ToolCallProgressDisposition::FailedBeforeExecution,
                                )
                            }
                            ToolCallStartDecision::Execute => {
                                tool_started = true;
                                self.emit_progress(ToolLoopProgressEvent::ToolCallStarted {
                                    tool_name: tool_name.clone(),
                                })
                                .await?;
                                // progress await 返回后仍需在共享生命周期锁内重新检查取消；只有
                                // 原子启动转换成功，才创建工具 future 并越过副作用边界。
                                on_started(&tool_name, prepared.effect)?;
                                *self.execution_counts.entry(tool_name.clone()).or_default() += 1;
                                if prepared.effect == ToolEffect::SideEffecting {
                                    // 写操作可能改变后续查询结果；只读去重只能跨越没有状态变化的
                                    // 连续查询段，不能让“查询 -> 修改 -> 再查询”复用旧判断。
                                    self.completed_read_only_calls.clear();
                                }
                                self.executed_tools.push(tool_name.clone());
                                match self.tools.execute_prepared(prepared).await {
                                    Ok(output) => {
                                        let domain_succeeded =
                                            tool_output_indicates_domain_success(&output);
                                        let execution_succeeded =
                                            tool_output_indicates_execution_success(&output);
                                        if execution_succeeded && let Some(key) = read_only_key {
                                            self.completed_read_only_calls.insert(
                                                key,
                                                (self.tool_results.len(), output.clone()),
                                            );
                                        }
                                        (
                                            tool_name,
                                            output,
                                            domain_succeeded,
                                            execution_succeeded,
                                            ToolCallProgressDisposition::Executed,
                                        )
                                    }
                                    Err(err) => (
                                        tool_name,
                                        tool_error_output(&err),
                                        false,
                                        false,
                                        ToolCallProgressDisposition::Executed,
                                    ),
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    if let Some(cached_output) = terminal_failure_key
                        .as_ref()
                        .and_then(|key| self.terminal_read_only_failures.get(key))
                    {
                        (
                            requested_tool_name,
                            compact_terminal_failure(cached_output),
                            false,
                            false,
                            ToolCallProgressDisposition::CacheHit,
                        )
                    } else {
                        self.rejected_call = true;
                        (
                            requested_tool_name,
                            tool_error_output(&err),
                            false,
                            false,
                            ToolCallProgressDisposition::FailedBeforeExecution,
                        )
                    }
                }
            };
        if progress_disposition != ToolCallProgressDisposition::CacheHit
            && output_error_retriable(&output) == Some(false)
            && let Some(key) = terminal_failure_key
        {
            self.terminal_read_only_failures.insert(key, output.clone());
        }
        // 缺参错误属于模型调用错误；只有紧邻的单例只读调用丢失已有参数、没有
        // 新增目标或选项时，才将它关联到此前成功结果。超时、不同参数、批量调用
        // 和写操作均保持独立失败；绝不以“已有成功”替代真实执行结果。
        let degraded_of = (redundant_of.is_none() && read_only)
            .then(|| {
                self.degraded_continuation(round, batch_len, prepared_arguments.as_ref(), &output)
            })
            .flatten();
        redundant_of = redundant_of.or(degraded_of);
        if let Some(index) = redundant_of {
            debug!(tool = %tool_name, round, result_index = self.tool_results.len(), redundant_of = index,
                "已关联 Agent 工具的冗余续调");
        }
        // 前序依赖只能使用领域成功。`empty_result` 虽然已完成网络请求，但不能成为
        // 依赖工具继续执行的事实依据。
        self.previous_call_succeeded = domain_succeeded;
        let event = match progress_disposition {
            ToolCallProgressDisposition::CacheHit => None,
            ToolCallProgressDisposition::Executed if execution_succeeded => {
                Some(ToolLoopProgressEvent::ToolCallFinished {
                    tool_name: tool_name.clone(),
                })
            }
            ToolCallProgressDisposition::Executed
            | ToolCallProgressDisposition::FailedBeforeExecution => {
                Some(ToolLoopProgressEvent::ToolCallFailed {
                    tool_name: tool_name.clone(),
                })
            }
        };
        let result = tool_execution_result(&tool_name, &output, domain_succeeded);
        let result_index = self.tool_results.len();
        self.tool_results.push(result.clone());
        self.tool_attempts.push(ToolExecutionAttempt {
            result_index,
            call_id: call_id.clone(),
            round,
            retry_of,
            redundant_of,
        });
        if let Some((name, arguments)) = prepared_arguments {
            self.current_batch.push(BatchAttempt {
                result_index,
                call_id,
                name,
                arguments,
                execution_succeeded,
                read_only,
                executed: tool_started,
                round,
            });
        }
        // 工具已经完成后先落可信轨迹，再通知上层；receiver 此时关闭不能抹掉结果。
        on_result(result);
        if let Some(event) = event {
            self.emit_progress(event).await?;
        }
        // 回填仍明确 ok=false，不伪造成功；提示模型复用已有证据或为新步骤补全
        // 参数，而不是把自己的退化调用归咎于用户。原始错误留在 tool_results。
        let output = if degraded_of.is_some() {
            let mut value: Value = serde_json::from_str(&output).expect("工具错误为合法 JSON");
            value["continuation_hint"] = json!(
                "此前同一查询已成功。本次调用丢失了原有参数且没有提供新目标，不代表用户缺参。若任务已完成，请基于已有结果回答；若仍有必要的新步骤，请补全该步骤参数后继续，不能声称未执行步骤成功。"
            );
            value.to_string()
        } else {
            output
        };
        Ok(ToolLoopCallOutput {
            output,
            skipped_for_finalization,
            stop_remaining_batch,
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

    fn degraded_continuation(
        &self,
        round: usize,
        batch_len: usize,
        prepared: Option<&(String, Value)>,
        output: &str,
    ) -> Option<usize> {
        if batch_len != 1 || self.last_batch.len() != 1 {
            return None;
        }
        let previous = self.continuation_anchor.as_ref()?;
        let (name, arguments) = prepared?;
        let result = self.tool_results.get(previous.result_index)?;
        if !previous.read_only
            || !previous.executed
            || !result.succeeded
            || self.last_batch[0].round + 1 != round
            || previous.name != *name
            || result.output.get("truncated").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        let output: Value = serde_json::from_str(output).ok()?;
        if !matches!(
            output.get("error")?.get("code")?.as_str()?,
            "bad_tool_arguments" | "invalid_arguments"
        ) {
            return None;
        }
        let current = arguments.as_object()?;
        let prior = previous.arguments.as_object()?;
        // 只接受原参数的退化子集；任何新增/改变的非空参数都可能代表必要的新步骤。
        let empty =
            |value: &Value| value.is_null() || value.as_str().is_some_and(|s| s.trim().is_empty());
        let no_new_information = current
            .iter()
            .all(|(key, value)| empty(value) || prior.get(key) == Some(value));
        let lost_information = prior
            .iter()
            .any(|(key, value)| !empty(value) && current.get(key).is_none_or(&empty));
        (no_new_information && lost_information).then_some(previous.result_index)
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
        if previous.execution_succeeded
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
            "kind": err.error_kind(),
            "retriable": err.retriable(),
            "upstream_status": err.upstream_status,
        }
    }))
    .unwrap_or_else(|_| r#"{"ok":false,"error":{"code":"tool_output_error","message":"failed to serialize tool error","stage":"tool_loop"}}"#.to_owned())
}

fn output_error_retriable(output: &str) -> Option<bool> {
    serde_json::from_str::<Value>(output)
        .ok()?
        .get("error")?
        .get("retriable")?
        .as_bool()
}

fn compact_terminal_failure(output: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(output) else {
        return output.to_owned();
    };
    let Some(object) = value.as_object_mut() else {
        return output.to_owned();
    };
    object.insert("deduplicated".to_owned(), Value::Bool(true));
    object.insert("retry_suppressed".to_owned(), Value::Bool(true));
    serde_json::to_string(&value).unwrap_or_else(|_| output.to_owned())
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

fn tool_output_indicates_domain_success(output: &str) -> bool {
    // `ok` 是工具领域目标是否达成的唯一依据，依赖工具和最终结果诊断均使用它。
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return true;
    };
    value.get("ok").and_then(Value::as_bool) != Some(false)
}

fn tool_output_indicates_execution_success(output: &str) -> bool {
    // 空结果等可预期的只读查询完成状态会显式标记 execution_succeeded；它们可缓存、
    // 不应重试，也应发送 Finished 进度，但不等同于领域成功。
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return true;
    };
    value.get("execution_succeeded").and_then(Value::as_bool) == Some(true)
        || value.get("ok").and_then(Value::as_bool) != Some(false)
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
    // 缓存命中只回传执行与领域状态，不把先前已回填给模型的完整答案、来源或原始
    // 结果重复注入上下文。empty_result 必须保留其可预期空结果语义。
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return output.to_owned();
    };
    let Some(object) = value.as_object() else {
        return output.to_owned();
    };
    let compact = if !tool_output_indicates_domain_success(output)
        && tool_output_indicates_execution_success(output)
    {
        json!({
            "ok": false,
            "execution_succeeded": true,
            "error": object.get("error").cloned().unwrap_or(Value::Null),
            "deduplicated": true,
        })
    } else {
        json!({"ok": true, "deduplicated": true})
    };
    serde_json::to_string(&compact).unwrap_or_else(|_| output.to_owned())
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

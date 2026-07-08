//! Agent Loop 控制器的纯逻辑单测。
//!
//! 协议适配（Responses / Chat Completions）的端到端验证保留在各自 provider
//! 模块的测试中；这里只覆盖统一循环控制本身：无工具回答、单工具、单轮多工具、
//! 多轮继续、业务失败、执行异常、最大轮数、prepare-before-execute 顺序与
//! usage 合并。

use super::*;
use crate::error::LlmError;
use crate::provider::types::TokenUsage;
use crate::tool::{ToolCallDependency, ToolContext, ToolMetadata, ToolOutput, ToolRegistry};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex as StdMutex};

fn test_context() -> ToolContext {
    ToolContext {
        task_id: "task-1".to_owned(),
        user_id: Some("u1".to_owned()),
        scope_id: "private:u1".to_owned(),
        group_member_role: None,
        tool_call_id: None,
    }
}

/// 脚本化单步会话：按预设脚本依次返回 `AgentStep`，并记录每次 advance 的入参。
#[allow(clippy::type_complexity)]
struct ScriptedSession {
    provider: &'static str,
    model: &'static str,
    script: Vec<AgentStep>,
    observed: Arc<StdMutex<Vec<(Vec<AgentToolResult>, bool)>>>,
}

impl ScriptedSession {
    fn new(provider: &'static str, model: &'static str, script: Vec<AgentStep>) -> Self {
        Self {
            provider,
            model,
            script,
            observed: Arc::new(StdMutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl AgentStepSession for ScriptedSession {
    fn provider(&self) -> &str {
        self.provider
    }
    fn model(&self) -> &str {
        self.model
    }
    async fn advance(
        &mut self,
        results: &[AgentToolResult],
        allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        self.observed
            .lock()
            .unwrap()
            .push((results.to_vec(), allow_tool_calls));
        Ok(self.script.remove(0))
    }
}

fn tool_call(name: &str, call_id: &str, args: &str) -> AgentToolCall {
    AgentToolCall {
        name: name.to_owned(),
        call_id: call_id.to_owned(),
        arguments: args.to_owned(),
    }
}

fn final_reply(text: &str) -> AgentStep {
    AgentStep::FinalAnswer {
        reply: text.to_owned(),
        usage: None,
    }
}

fn tool_calls(calls: Vec<AgentToolCall>) -> AgentStep {
    AgentStep::ToolCalls { calls, usage: None }
}

/// 可计数工具，用于验证执行次数与依赖跳过。
struct CountingTool {
    name: &'static str,
    calls: Arc<StdMutex<usize>>,
    fail: bool,
    soft_fail: bool,
    dependency: ToolCallDependency,
}

#[async_trait]
impl crate::tool::Tool for CountingTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: self.name.to_owned(),
            description: "counting tool".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
        }
    }

    fn prepare(
        &self,
        _ctx: &ToolContext,
        arguments: Value,
    ) -> Result<crate::tool::ToolPreparation, LlmError> {
        Ok(crate::tool::ToolPreparation::ready(arguments).with_dependency(self.dependency))
    }

    async fn execute(&self, _ctx: ToolContext, arguments: Value) -> Result<ToolOutput, LlmError> {
        *self.calls.lock().unwrap() += 1;
        if self.fail {
            return Err(LlmError::new("tool_failed", "simulated failure", "tool"));
        }
        if self.soft_fail {
            return Ok(ToolOutput::json(json!({
                "ok": false,
                "error_code": "soft_failure",
                "value": arguments["value"],
            })));
        }
        Ok(ToolOutput::json(json!({
            "ok": true,
            "value": arguments["value"],
        })))
    }
}

/// 记录 prepare/execute 顺序的工具，验证同轮 prepare-before-execute。
struct OrderTool {
    name: &'static str,
    sequence: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl crate::tool::Tool for OrderTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: self.name.to_owned(),
            description: "order tool".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
        }
    }

    fn prepare(
        &self,
        _ctx: &ToolContext,
        arguments: Value,
    ) -> Result<crate::tool::ToolPreparation, LlmError> {
        self.sequence
            .lock()
            .unwrap()
            .push(format!("prepare:{}", self.name));
        Ok(crate::tool::ToolPreparation::ready(arguments))
    }

    async fn execute(&self, _ctx: ToolContext, arguments: Value) -> Result<ToolOutput, LlmError> {
        self.sequence
            .lock()
            .unwrap()
            .push(format!("execute:{}", self.name));
        Ok(ToolOutput::json(json!({
            "ok": true,
            "value": arguments["value"],
        })))
    }
}

fn registry_with(tools: Vec<Arc<dyn crate::tool::Tool>>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in tools {
        registry.insert(tool).unwrap();
    }
    registry
}

#[tokio::test]
async fn no_tool_answer_completes_immediately() {
    let mut registry = ToolRegistry::new();
    registry
        .insert(Arc::new(CountingTool {
            name: "echo",
            calls: Arc::new(StdMutex::new(0)),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _)
        .unwrap();
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![final_reply("你好呀")],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "你好呀");
    assert!(outcome.executed_tools.is_empty());
}

#[tokio::test]
async fn single_tool_then_final_answer() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            final_reply("done"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(outcome.executed_tools, vec!["echo".to_owned()]);
    assert_eq!(outcome.tool_results.len(), 1);
    assert!(outcome.tool_results[0].succeeded);
}

#[tokio::test]
async fn progress_sink_reports_tool_start_and_finish() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let progress_sink = {
        let events = events.clone();
        Arc::new(move |event: ToolLoopProgressEvent| {
            let events = events.clone();
            Box::pin(async move {
                events.lock().unwrap().push(event);
                Ok(())
            }) as ToolLoopProgressFuture
        })
    };
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            final_reply("done"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 3, Some(progress_sink))
        .await
        .unwrap();

    assert_eq!(outcome.reply, "done");
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            ToolLoopProgressEvent::ToolCallStarted {
                tool_name: "echo".to_owned()
            },
            ToolLoopProgressEvent::ToolCallFinished {
                tool_name: "echo".to_owned()
            }
        ]
    );
}

#[tokio::test]
async fn progress_sink_reports_tool_failure() {
    let events = Arc::new(StdMutex::new(Vec::new()));
    let progress_sink = {
        let events = events.clone();
        Arc::new(move |event: ToolLoopProgressEvent| {
            let events = events.clone();
            Box::pin(async move {
                events.lock().unwrap().push(event);
                Ok(())
            }) as ToolLoopProgressFuture
        })
    };
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: true,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            final_reply("done"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 3, Some(progress_sink))
        .await
        .unwrap();

    assert_eq!(outcome.reply, "done");
    assert_eq!(
        *events.lock().unwrap(),
        vec![
            ToolLoopProgressEvent::ToolCallStarted {
                tool_name: "echo".to_owned()
            },
            ToolLoopProgressEvent::ToolCallFailed {
                tool_name: "echo".to_owned()
            }
        ]
    );
}

#[tokio::test]
async fn progress_sink_error_interrupts_before_tool_execution() {
    let calls = Arc::new(StdMutex::new(0));
    let progress_sink = Arc::new(move |event: ToolLoopProgressEvent| {
        Box::pin(async move {
            assert_eq!(
                event,
                ToolLoopProgressEvent::ToolCallStarted {
                    tool_name: "echo".to_owned()
                }
            );
            Err(LlmError::new(
                "cancelled",
                "stream receiver dropped",
                "stream",
            ))
        }) as ToolLoopProgressFuture
    });
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            final_reply("done"),
        ],
    ));

    let err = run_agent_loop(session, registry, test_context(), 3, Some(progress_sink))
        .await
        .unwrap_err();

    assert_eq!(err.code, "cancelled");
    assert_eq!(err.stage, "stream");
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn same_round_multiple_tools_prepare_before_execute() {
    let sequence = Arc::new(StdMutex::new(Vec::new()));
    let registry = registry_with(vec![
        Arc::new(OrderTool {
            name: "first",
            sequence: sequence.clone(),
        }) as _,
        Arc::new(OrderTool {
            name: "second",
            sequence: sequence.clone(),
        }) as _,
    ]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![
                tool_call("first", "c1", r#"{"value":"a"}"#),
                tool_call("second", "c2", r#"{"value":"b"}"#),
            ]),
            final_reply("ok"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "ok");
    assert_eq!(
        *sequence.lock().unwrap(),
        vec![
            "prepare:first".to_owned(),
            "prepare:second".to_owned(),
            "execute:first".to_owned(),
            "execute:second".to_owned(),
        ]
    );
}

#[tokio::test]
async fn multi_round_continues_after_tool_result() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            tool_calls(vec![tool_call("echo", "c2", r#"{"value":"b"}"#)]),
            final_reply("merged"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "merged");
    assert_eq!(*calls.lock().unwrap(), 2);
    assert_eq!(
        outcome.executed_tools,
        vec!["echo".to_owned(), "echo".to_owned()]
    );
}

#[tokio::test]
async fn execution_exception_still_records_result_and_continues() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "boom",
        calls: Arc::new(StdMutex::new(0)),
        fail: true,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("boom", "c1", r#"{"value":"a"}"#)]),
            final_reply("recovered"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "recovered");
    assert_eq!(outcome.tool_results.len(), 1);
    assert!(!outcome.tool_results[0].succeeded);
    assert!(outcome.tool_results[0].output["error"]["code"] == "tool_failed");
}

#[tokio::test]
async fn soft_business_failure_marks_unsucceeded() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "soft",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: true,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("soft", "c1", r#"{"value":"a"}"#)]),
            final_reply("noted"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "noted");
    assert!(!outcome.tool_results[0].succeeded);
    assert_eq!(outcome.tool_results[0].output["error_code"], "soft_failure");
}

#[tokio::test]
async fn dependency_skip_after_failure() {
    let fail_calls = Arc::new(StdMutex::new(0));
    let ok_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(CountingTool {
            name: "fail_tool",
            calls: fail_calls.clone(),
            fail: true,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _,
        Arc::new(CountingTool {
            name: "ok_tool",
            calls: ok_calls.clone(),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::PreviousCallSuccess,
        }) as _,
    ]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![
                tool_call("fail_tool", "c1", r#"{"value":"a"}"#),
                tool_call("ok_tool", "c2", r#"{"value":"b"}"#),
            ]),
            final_reply("done"),
        ],
    ));
    let outcome = run_agent_loop(session, registry, test_context(), 3, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");
    assert_eq!(*fail_calls.lock().unwrap(), 1);
    assert_eq!(*ok_calls.lock().unwrap(), 0);
    // ok_tool 因依赖跳过，仍计入轨迹且 succeeded=false。
    let ok_result = outcome
        .tool_results
        .iter()
        .find(|r| r.name == "ok_tool")
        .unwrap();
    assert!(!ok_result.succeeded);
    assert_eq!(ok_result.output["skipped"], true);
}

#[tokio::test]
async fn max_rounds_returns_tool_loop_limit_without_executing_last_batch() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    // max_rounds=1：round 0 执行一次；round 1 仍要求工具调用 → 超限。
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            tool_calls(vec![tool_call("echo", "c2", r#"{"value":"b"}"#)]),
        ],
    ));
    let err = run_agent_loop(session, registry, test_context(), 1, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "tool_loop_limit");
    assert_eq!(err.stage, "tool_loop");
    // 第二批未执行。
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn last_round_uses_allow_tool_calls_false() {
    // max_rounds=1：round 0 allow=true，round 1 allow=false。
    let mut registry = ToolRegistry::new();
    registry
        .insert(Arc::new(CountingTool {
            name: "echo",
            calls: Arc::new(StdMutex::new(0)),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _)
        .unwrap();
    let session = ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("echo", "c1", r#"{"value":"a"}"#)]),
            final_reply("ok"),
        ],
    );
    let observed_inner = session.observed.clone();
    let outcome = run_agent_loop(Box::new(session), registry, test_context(), 1, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "ok");
    let recorded = observed_inner.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert!(recorded[0].1); // round 0 allow=true
    assert!(!recorded[1].1); // round 1 allow=false
}

#[tokio::test]
async fn empty_tools_rejected_before_any_request() {
    let session = Box::new(ScriptedSession::new("mock", "m", vec![final_reply("x")]));
    let err = run_agent_loop(session, ToolRegistry::new(), test_context(), 3, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "bad_request");
    assert_eq!(err.stage, "tool_loop");
}

#[tokio::test]
async fn zero_max_rounds_rejected() {
    let mut registry = ToolRegistry::new();
    registry
        .insert(Arc::new(CountingTool {
            name: "echo",
            calls: Arc::new(StdMutex::new(0)),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _)
        .unwrap();
    let session = Box::new(ScriptedSession::new("mock", "m", vec![final_reply("x")]));
    let err = run_agent_loop(session, registry, test_context(), 0, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "bad_request");
    assert_eq!(err.stage, "tool_loop");
}

#[tokio::test]
async fn usage_merges_across_rounds() {
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            AgentStep::ToolCalls {
                calls: vec![tool_call("echo", "c1", r#"{"value":"a"}"#)],
                usage: Some(TokenUsage {
                    input_tokens: Some(10),
                    cached_input_tokens: None,
                    output_tokens: Some(3),
                    total_tokens: Some(13),
                }),
            },
            AgentStep::FinalAnswer {
                reply: "ok".to_owned(),
                usage: Some(TokenUsage {
                    input_tokens: Some(8),
                    cached_input_tokens: Some(2),
                    output_tokens: Some(4),
                    total_tokens: Some(12),
                }),
            },
        ],
    ));
    let mut registry = ToolRegistry::new();
    registry
        .insert(Arc::new(CountingTool {
            name: "echo",
            calls: Arc::new(StdMutex::new(0)),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _)
        .unwrap();
    let outcome = run_agent_loop(session, registry, test_context(), 3, None)
        .await
        .unwrap();
    let usage = outcome.usage.unwrap();
    assert_eq!(usage.input_tokens, Some(18));
    assert_eq!(usage.cached_input_tokens, Some(2));
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(usage.total_tokens, Some(25));
}

#[allow(dead_code)]
fn _ensure_value_imported(_: Value) {}

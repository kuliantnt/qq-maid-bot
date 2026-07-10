//! Agent Loop 控制器的纯逻辑单测。
//!
//! 协议适配（Responses / Chat Completions）的端到端验证保留在各自 provider
//! 模块的测试中；这里只覆盖统一循环控制本身：无工具回答、单工具、单轮多工具、
//! 多轮继续、业务失败、执行异常、最大轮数、prepare-before-execute 顺序与
//! usage 合并。

use super::*;
use crate::error::LlmError;
use crate::provider::{AgentStopReason, types::TokenUsage};
use crate::tool::{ToolCallDependency, ToolContext, ToolMetadata, ToolOutput, ToolRegistry};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Barrier, Mutex as StdMutex},
};
use tokio::sync::Notify;

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

enum StreamingAction {
    Final {
        deltas: Vec<&'static str>,
        reply: &'static str,
    },
    ToolCallsWithBufferedDraft {
        draft_delta: &'static str,
        calls: Vec<AgentToolCall>,
    },
    ErrorBeforeDelta,
    ErrorAfterDelta {
        delta: &'static str,
    },
    HangBeforeDelta,
    HangAfterDelta {
        delta: &'static str,
    },
}

struct StreamingSession {
    provider: &'static str,
    model: &'static str,
    streaming_script: VecDeque<StreamingAction>,
    fallback_script: Vec<AgentStep>,
    advance_calls: Arc<StdMutex<usize>>,
    buffered_drafts: Arc<StdMutex<Vec<String>>>,
}

impl StreamingSession {
    fn new(action: StreamingAction, fallback_script: Vec<AgentStep>) -> Self {
        Self::scripted(vec![action], fallback_script)
    }

    fn scripted(streaming_script: Vec<StreamingAction>, fallback_script: Vec<AgentStep>) -> Self {
        Self {
            provider: "mock",
            model: "m",
            streaming_script: streaming_script.into(),
            fallback_script,
            advance_calls: Arc::new(StdMutex::new(0)),
            buffered_drafts: Arc::new(StdMutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl AgentStepSession for StreamingSession {
    fn provider(&self) -> &str {
        self.provider
    }

    fn model(&self) -> &str {
        self.model
    }

    async fn advance(
        &mut self,
        _results: &[AgentToolResult],
        _allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        *self.advance_calls.lock().unwrap() += 1;
        Ok(self.fallback_script.remove(0))
    }

    async fn advance_streaming(
        &mut self,
        _results: &[AgentToolResult],
        _allow_tool_calls: bool,
        text_delta_sink: AgentTextDeltaSink,
    ) -> Result<Option<AgentStep>, LlmError> {
        let action = self
            .streaming_script
            .pop_front()
            .expect("streaming script must contain an action");
        match action {
            StreamingAction::Final { deltas, reply } => {
                for delta in deltas {
                    text_delta_sink(delta.to_owned()).await?;
                }
                Ok(Some(final_reply(reply)))
            }
            StreamingAction::ToolCallsWithBufferedDraft { draft_delta, calls } => {
                self.buffered_drafts
                    .lock()
                    .unwrap()
                    .push(draft_delta.to_owned());
                Ok(Some(tool_calls(calls)))
            }
            StreamingAction::ErrorBeforeDelta => Err(LlmError::provider(
                "stream failed before visible delta",
                "stream",
            )),
            StreamingAction::ErrorAfterDelta { delta } => {
                text_delta_sink(delta.to_owned()).await?;
                Err(LlmError::provider(
                    "stream failed after visible delta",
                    "stream_after_delta",
                ))
            }
            StreamingAction::HangBeforeDelta => std::future::pending().await,
            StreamingAction::HangAfterDelta { delta } => {
                text_delta_sink(delta.to_owned()).await?;
                std::future::pending().await
            }
        }
    }
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

struct ErrorScriptSession {
    script: VecDeque<Result<AgentStep, LlmError>>,
}

#[async_trait]
impl AgentStepSession for ErrorScriptSession {
    fn provider(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "m"
    }

    async fn advance(
        &mut self,
        _results: &[AgentToolResult],
        _allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        self.script.pop_front().expect("missing scripted result")
    }
}

struct HangingSession;

#[async_trait]
impl AgentStepSession for HangingSession {
    fn provider(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "m"
    }

    async fn advance(
        &mut self,
        _results: &[AgentToolResult],
        _allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError> {
        std::future::pending().await
    }

    async fn advance_streaming(
        &mut self,
        _results: &[AgentToolResult],
        _allow_tool_calls: bool,
        _text_delta_sink: AgentTextDeltaSink,
    ) -> Result<Option<AgentStep>, LlmError> {
        std::future::pending().await
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

struct ClarificationTool;

struct ControlledTool {
    started: Arc<Notify>,
    release: Arc<Notify>,
    calls: Arc<StdMutex<usize>>,
}

#[async_trait]
impl crate::tool::Tool for ControlledTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: "controlled".to_owned(),
            description: "controlled tool".to_owned(),
            parameters: json!({"type": "object", "additionalProperties": false}),
        }
    }

    async fn execute(&self, _ctx: ToolContext, _arguments: Value) -> Result<ToolOutput, LlmError> {
        *self.calls.lock().unwrap() += 1;
        self.started.notify_one();
        self.release.notified().await;
        Ok(ToolOutput::json(json!({"ok": true})))
    }
}

#[async_trait]
impl crate::tool::Tool for ClarificationTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: "clarify".to_owned(),
            description: "clarification tool".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        }
    }

    async fn execute(&self, _ctx: ToolContext, _arguments: Value) -> Result<ToolOutput, LlmError> {
        Ok(ToolOutput::json(json!({
            "ok": false,
            "requires_clarification": true,
        })))
    }
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

fn delta_sink(deltas: Arc<StdMutex<Vec<String>>>) -> AgentTextDeltaSink {
    Arc::new(move |delta| {
        let deltas = deltas.clone();
        Box::pin(async move {
            deltas.lock().unwrap().push(delta);
            Ok(())
        }) as AgentTextDeltaFuture
    })
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
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "你好呀");
    assert_eq!(outcome.agent.model_rounds, 1);
    assert_eq!(
        outcome.agent.stop_reason,
        Some(AgentStopReason::DirectAnswer)
    );
    assert!(outcome.agent.emitted_tools.is_empty());
    assert!(outcome.agent.executed_tools.is_empty());
    assert!(outcome.agent.tool_results.is_empty());
}

#[tokio::test]
async fn streaming_advance_final_answer_emits_real_deltas() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(StreamingSession::new(
        StreamingAction::Final {
            deltas: vec!["你", "好"],
            reply: "你好",
        },
        Vec::new(),
    ));
    let advance_calls = session.advance_calls.clone();
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let outcome = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        None,
        Some(delta_sink(deltas.clone())),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "你好");
    assert_eq!(
        *deltas.lock().unwrap(),
        vec!["你".to_owned(), "好".to_owned()]
    );
    assert_eq!(*advance_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn streaming_tool_round_suppresses_draft_then_streams_final_answer() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(StreamingSession::scripted(
        vec![
            StreamingAction::ToolCallsWithBufferedDraft {
                draft_delta: "草稿不外显",
                calls: vec![tool_call("echo", "c1", r#"{"value":"a"}"#)],
            },
            StreamingAction::Final {
                deltas: vec!["最终", "回答"],
                reply: "最终回答",
            },
        ],
        Vec::new(),
    ));
    let advance_calls = session.advance_calls.clone();
    let buffered_drafts = session.buffered_drafts.clone();
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let outcome = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        None,
        Some(delta_sink(deltas.clone())),
    )
    .await
    .unwrap();

    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(outcome.agent.executed_tools, vec!["echo".to_owned()]);
    assert_eq!(outcome.reply, "最终回答");
    assert_eq!(
        *buffered_drafts.lock().unwrap(),
        vec!["草稿不外显".to_owned()]
    );
    assert_eq!(
        *deltas.lock().unwrap(),
        vec!["最终".to_owned(), "回答".to_owned()]
    );
    assert_eq!(*advance_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn fallback_after_tool_result_does_not_repeat_tool_side_effect() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(StreamingSession::scripted(
        vec![
            StreamingAction::ToolCallsWithBufferedDraft {
                draft_delta: "不外显",
                calls: vec![tool_call("echo", "c1", r#"{"value":"a"}"#)],
            },
            StreamingAction::ErrorBeforeDelta,
        ],
        vec![final_reply("fallback summary")],
    ));

    let outcome = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        None,
        Some(delta_sink(Arc::new(StdMutex::new(Vec::new())))),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "fallback summary");
    assert!(outcome.fallback_used);
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(outcome.agent.executed_tools, vec!["echo"]);
}

#[tokio::test]
async fn streaming_advance_error_before_visible_delta_falls_back() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(StreamingSession::new(
        StreamingAction::ErrorBeforeDelta,
        vec![final_reply("fallback")],
    ));
    let advance_calls = session.advance_calls.clone();
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let outcome = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        None,
        Some(delta_sink(deltas.clone())),
    )
    .await
    .unwrap();

    assert_eq!(outcome.reply, "fallback");
    assert!(deltas.lock().unwrap().is_empty());
    assert_eq!(*advance_calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn unsupported_streaming_advance_falls_back_without_marking_failure() {
    let mut session = ScriptedSession::new("mock", "m", vec![final_reply("fallback")]);

    let advance = super::runner::advance_with_optional_streaming(
        &mut session,
        &[],
        true,
        Some(delta_sink(Arc::new(StdMutex::new(Vec::new())))),
        std::time::Duration::from_millis(50),
        std::time::Duration::from_millis(50),
        0,
    )
    .await
    .unwrap();

    assert!(!advance.fallback_used);
    assert!(matches!(advance.step, AgentStep::FinalAnswer { .. }));
}

#[tokio::test]
async fn streaming_advance_error_after_visible_delta_does_not_fallback() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(StreamingSession::new(
        StreamingAction::ErrorAfterDelta { delta: "半句" },
        vec![final_reply("fallback must not run")],
    ));
    let advance_calls = session.advance_calls.clone();
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let err = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        None,
        Some(delta_sink(deltas.clone())),
    )
    .await
    .unwrap_err();

    assert_eq!(err.stage, "stream_after_delta");
    assert_eq!(*deltas.lock().unwrap(), vec!["半句".to_owned()]);
    assert_eq!(*advance_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn streaming_advance_timeout_before_visible_delta_falls_back_once() {
    let mut session = StreamingSession::new(
        StreamingAction::HangBeforeDelta,
        vec![final_reply("fallback after timeout")],
    );
    let advance_calls = session.advance_calls.clone();

    let advance = super::runner::advance_with_optional_streaming(
        &mut session,
        &[],
        true,
        Some(delta_sink(Arc::new(StdMutex::new(Vec::new())))),
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(50),
        0,
    )
    .await
    .unwrap();

    let AgentStep::FinalAnswer { reply, .. } = advance.step else {
        panic!("expected fallback final answer");
    };
    assert_eq!(reply, "fallback after timeout");
    assert!(advance.fallback_used);
    assert_eq!(*advance_calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn streaming_advance_timeout_after_visible_delta_does_not_fallback() {
    let mut session = StreamingSession::new(
        StreamingAction::HangAfterDelta { delta: "半句" },
        vec![final_reply("fallback must not run")],
    );
    let advance_calls = session.advance_calls.clone();
    let deltas = Arc::new(StdMutex::new(Vec::new()));

    let err = super::runner::advance_with_optional_streaming(
        &mut session,
        &[],
        false,
        Some(delta_sink(deltas.clone())),
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(50),
        0,
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "timeout");
    assert_eq!(err.stage, "agent_stream_after_delta");
    assert_eq!(*deltas.lock().unwrap(), vec!["半句".to_owned()]);
    assert_eq!(*advance_calls.lock().unwrap(), 0);
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
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");
    assert_eq!(*calls.lock().unwrap(), 1);
    assert_eq!(outcome.agent.model_rounds, 2);
    assert_eq!(outcome.agent.stop_reason, Some(AgentStopReason::ToolUsed));
    assert_eq!(outcome.agent.executed_tools, vec!["echo".to_owned()]);
    assert_eq!(outcome.agent.tool_results.len(), 1);
    assert!(outcome.agent.tool_results[0].succeeded);
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

    let outcome = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        Some(progress_sink),
        None,
    )
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

    let outcome = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        Some(progress_sink),
        None,
    )
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

    let err = run_agent_loop(
        session,
        registry,
        test_context(),
        3,
        Some(progress_sink),
        None,
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "cancelled");
    assert_eq!(err.stage, "stream");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Cancelled));
    assert_eq!(diagnostics.emitted_tools, vec!["echo"]);
    assert!(diagnostics.tool_execution_attempted);
    assert!(diagnostics.executed_tools.is_empty());
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn progress_sink_error_after_tool_completion_keeps_real_result() {
    let calls = Arc::new(StdMutex::new(0));
    let progress_sink = Arc::new(move |event: ToolLoopProgressEvent| {
        Box::pin(async move {
            match event {
                ToolLoopProgressEvent::ToolCallStarted { .. } => Ok(()),
                ToolLoopProgressEvent::ToolCallFinished { .. } => Err(LlmError::new(
                    "cancelled",
                    "stream receiver dropped after tool completion",
                    "stream",
                )),
                ToolLoopProgressEvent::ToolCallFailed { .. } => {
                    panic!("successful tool must not emit failed progress")
                }
            }
        }) as ToolLoopProgressFuture
    });
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let handle = AgentRunHandle::default();

    let err = run_agent_loop_with_handle(
        Box::new(ScriptedSession::new(
            "mock",
            "m",
            vec![tool_calls(vec![tool_call(
                "echo",
                "c1",
                r#"{"value":"a"}"#,
            )])],
        )),
        registry,
        test_context(),
        3,
        Some(progress_sink),
        None,
        Some(handle),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "cancelled");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.executed_tools, ["echo"]);
    assert_eq!(diagnostics.tool_results.len(), 1);
    assert!(diagnostics.tools_with_unknown_result.is_empty());
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn non_stream_timeout_returns_structured_agent_failure() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);

    let err = super::runner::run_agent_loop_with_timeouts(
        Box::new(HangingSession),
        registry,
        test_context(),
        3,
        None,
        None,
        None,
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(10),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "timeout");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
    assert!(!diagnostics.streaming_fallback_used);
}

#[tokio::test]
async fn streaming_first_activity_timeout_and_fallback_timeout_keep_diagnostics() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);

    let err = super::runner::run_agent_loop_with_timeouts(
        Box::new(HangingSession),
        registry,
        test_context(),
        3,
        None,
        Some(delta_sink(Arc::new(StdMutex::new(Vec::new())))),
        None,
        std::time::Duration::from_millis(10),
        std::time::Duration::from_millis(10),
    )
    .await
    .unwrap_err();

    assert_eq!(err.code, "timeout");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
    assert!(diagnostics.streaming_fallback_used);
}

#[tokio::test]
async fn shared_handle_cancel_interrupts_inflight_model_round() {
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: Arc::new(StdMutex::new(0)),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let handle = AgentRunHandle::default();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        super::runner::run_agent_loop_with_timeouts(
            Box::new(HangingSession),
            registry,
            test_context(),
            3,
            None,
            None,
            Some(task_handle),
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(1),
        )
        .await
    });
    while handle.snapshot().model_rounds == 0 {
        tokio::task::yield_now().await;
    }
    handle.cancel(AgentStopReason::Cancelled);

    let err = task.await.unwrap().unwrap_err();
    assert_eq!(err.code, "cancelled");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Cancelled));
}

#[tokio::test]
async fn cancellation_while_tool_started_progress_is_blocked_prevents_tool_start() {
    let progress_started = Arc::new(Notify::new());
    let progress_release = Arc::new(Notify::new());
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let sink_started = progress_started.clone();
    let sink_release = progress_release.clone();
    let progress_sink = Arc::new(move |event| {
        let sink_started = sink_started.clone();
        let sink_release = sink_release.clone();
        Box::pin(async move {
            if matches!(event, ToolLoopProgressEvent::ToolCallStarted { .. }) {
                sink_started.notify_one();
                sink_release.notified().await;
            }
            Ok(())
        }) as ToolLoopProgressFuture
    }) as ToolLoopProgressSink;
    let handle = AgentRunHandle::default();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        run_agent_loop_with_handle(
            Box::new(ScriptedSession::new(
                "mock",
                "m",
                vec![
                    tool_calls(vec![tool_call("echo", "c1", r#"{"value":"x"}"#)]),
                    final_reply("must not run"),
                ],
            )),
            registry,
            test_context(),
            3,
            Some(progress_sink),
            None,
            Some(task_handle),
        )
        .await
    });

    progress_started.notified().await;
    handle.cancel(AgentStopReason::Timeout);
    progress_release.notify_one();

    let err = task.await.unwrap().unwrap_err();
    assert_eq!(err.code, "timeout");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
    assert!(diagnostics.executed_tools.is_empty());
    assert!(diagnostics.tools_with_unknown_result.is_empty());
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn cancellation_during_tool_waits_for_result_and_stops_remaining_work() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let controlled_calls = Arc::new(StdMutex::new(0));
    let later_calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![
        Arc::new(ControlledTool {
            started: started.clone(),
            release: release.clone(),
            calls: controlled_calls.clone(),
        }) as _,
        Arc::new(CountingTool {
            name: "later",
            calls: later_calls.clone(),
            fail: false,
            soft_fail: false,
            dependency: ToolCallDependency::None,
        }) as _,
    ]);
    let handle = AgentRunHandle::default();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        run_agent_loop_with_handle(
            Box::new(ScriptedSession::new(
                "mock",
                "m",
                vec![
                    tool_calls(vec![
                        tool_call("controlled", "c1", "{}"),
                        tool_call("later", "c2", r#"{"value":"b"}"#),
                    ]),
                    final_reply("must not run"),
                ],
            )),
            registry,
            test_context(),
            3,
            None,
            None,
            Some(task_handle),
        )
        .await
    });

    started.notified().await;
    let inflight = handle.snapshot();
    assert_eq!(inflight.executed_tools, ["controlled"]);
    assert_eq!(inflight.tools_with_unknown_result, ["controlled"]);
    assert!(inflight.tool_results.is_empty());
    handle.cancel(AgentStopReason::Timeout);
    release.notify_one();

    let err = task.await.unwrap().unwrap_err();
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
    assert_eq!(diagnostics.model_rounds, 1);
    assert_eq!(diagnostics.executed_tools, ["controlled"]);
    assert_eq!(diagnostics.tool_results.len(), 1);
    assert!(diagnostics.tools_with_unknown_result.is_empty());
    assert_eq!(*controlled_calls.lock().unwrap(), 1);
    assert_eq!(*later_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn cleanup_abort_after_started_tool_preserves_unknown_result() {
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(ControlledTool {
        started: started.clone(),
        release,
        calls: calls.clone(),
    }) as _]);
    let handle = AgentRunHandle::default();
    let task_handle = handle.clone();
    let task = tokio::spawn(async move {
        run_agent_loop_with_handle(
            Box::new(ScriptedSession::new(
                "mock",
                "m",
                vec![tool_calls(vec![tool_call("controlled", "c1", "{}")])],
            )),
            registry,
            test_context(),
            3,
            None,
            None,
            Some(task_handle),
        )
        .await
    });

    started.notified().await;
    handle.cancel(AgentStopReason::Timeout);
    task.abort();
    let _ = task.await;

    let diagnostics = handle.snapshot();
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Timeout));
    assert_eq!(diagnostics.executed_tools, ["controlled"]);
    assert!(diagnostics.tool_results.is_empty());
    assert_eq!(diagnostics.tools_with_unknown_result, ["controlled"]);
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[test]
fn new_candidate_attempt_clears_failed_but_external_termination_wins() {
    let handle = AgentRunHandle::default();
    handle.set_stop_reason(AgentStopReason::Failed);
    handle.begin_candidate_attempt().unwrap();
    assert_eq!(handle.snapshot().stop_reason, None);

    handle.cancel(AgentStopReason::Timeout);
    handle.set_stop_reason(AgentStopReason::Failed);
    assert_eq!(
        handle.snapshot().stop_reason,
        Some(AgentStopReason::Timeout)
    );
}

#[test]
fn cancel_and_begin_candidate_are_linearized_by_one_lifecycle_lock() {
    for reason in [AgentStopReason::Timeout, AgentStopReason::Cancelled] {
        for _ in 0..128 {
            let handle = AgentRunHandle::default();
            handle.set_stop_reason(AgentStopReason::Failed);
            let barrier = Arc::new(Barrier::new(3));
            let cancel_handle = handle.clone();
            let cancel_barrier = barrier.clone();
            let cancel = std::thread::spawn(move || {
                cancel_barrier.wait();
                cancel_handle.cancel(reason);
            });
            let attempt_handle = handle.clone();
            let attempt_barrier = barrier.clone();
            let begin = std::thread::spawn(move || {
                attempt_barrier.wait();
                let _ = attempt_handle.begin_candidate_attempt();
            });

            barrier.wait();
            cancel.join().unwrap();
            begin.join().unwrap();

            assert!(handle.is_cancelled());
            assert_eq!(handle.snapshot().stop_reason, Some(reason));
        }
    }
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
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
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
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "merged");
    assert_eq!(*calls.lock().unwrap(), 2);
    assert_eq!(outcome.agent.model_rounds, 3);
    assert_eq!(
        outcome.agent.executed_tools,
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
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "recovered");
    assert_eq!(outcome.agent.model_rounds, 2);
    assert_eq!(outcome.agent.stop_reason, Some(AgentStopReason::Failed));
    assert_eq!(outcome.agent.tool_results.len(), 1);
    assert!(!outcome.agent.tool_results[0].succeeded);
    assert!(outcome.agent.tool_results[0].output["error"]["code"] == "tool_failed");
}

#[tokio::test]
async fn model_failure_after_tool_execution_keeps_partial_trace() {
    let calls = Arc::new(StdMutex::new(0));
    let registry = registry_with(vec![Arc::new(CountingTool {
        name: "echo",
        calls: calls.clone(),
        fail: false,
        soft_fail: false,
        dependency: ToolCallDependency::None,
    }) as _]);
    let session = Box::new(ErrorScriptSession {
        script: VecDeque::from([
            Ok(tool_calls(vec![tool_call(
                "echo",
                "c1",
                r#"{"value":"a"}"#,
            )])),
            Err(LlmError::provider("second round failed", "provider")),
        ]),
    });

    let err = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap_err();

    assert_eq!(*calls.lock().unwrap(), 1);
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 2);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::Failed));
    assert_eq!(diagnostics.emitted_tools, vec!["echo"]);
    assert_eq!(diagnostics.executed_tools, vec!["echo"]);
    assert_eq!(diagnostics.tool_results.len(), 1);
    assert!(diagnostics.tool_results[0].succeeded);
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
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "noted");
    assert!(!outcome.agent.tool_results[0].succeeded);
    assert_eq!(
        outcome.agent.tool_results[0].output["error_code"],
        "soft_failure"
    );
}

#[tokio::test]
async fn clarification_tool_result_sets_clarify_stop_reason() {
    let registry = registry_with(vec![Arc::new(ClarificationTool) as _]);
    let session = Box::new(ScriptedSession::new(
        "mock",
        "m",
        vec![
            tool_calls(vec![tool_call("clarify", "c1", "{}")]),
            final_reply("请补充具体目标。"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();

    assert_eq!(outcome.agent.model_rounds, 2);
    assert_eq!(outcome.agent.stop_reason, Some(AgentStopReason::Clarify));
    assert_eq!(outcome.agent.executed_tools, vec!["clarify"]);
    assert!(!outcome.agent.tool_results[0].succeeded);
}

#[tokio::test]
async fn unknown_tool_is_emitted_and_attempted_but_rejected() {
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
            tool_calls(vec![tool_call("unknown_tool", "c1", r#"{"value":"a"}"#)]),
            final_reply("无法执行该工具。"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();

    assert_eq!(outcome.agent.emitted_tools, vec!["unknown_tool"]);
    assert_eq!(outcome.agent.model_rounds, 2);
    assert!(outcome.agent.tool_execution_attempted);
    assert_eq!(outcome.agent.stop_reason, Some(AgentStopReason::Rejected));
    assert!(outcome.agent.executed_tools.is_empty());
    assert_eq!(outcome.agent.tool_results.len(), 1);
    assert_eq!(outcome.agent.tool_results[0].name, "unknown_tool");
    assert!(!outcome.agent.tool_results[0].succeeded);
}

#[tokio::test]
async fn invalid_tool_arguments_are_emitted_and_attempted_but_not_executed() {
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
            tool_calls(vec![tool_call("echo", "c1", "not-json")]),
            final_reply("参数无效，未执行。"),
        ],
    ));

    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();

    assert_eq!(outcome.agent.emitted_tools, vec!["echo"]);
    assert_eq!(outcome.agent.model_rounds, 2);
    assert!(outcome.agent.tool_execution_attempted);
    assert_eq!(outcome.agent.stop_reason, Some(AgentStopReason::Rejected));
    assert!(outcome.agent.executed_tools.is_empty());
    assert_eq!(outcome.agent.tool_results.len(), 1);
    assert_eq!(outcome.agent.tool_results[0].name, "echo");
    assert!(!outcome.agent.tool_results[0].succeeded);
    assert_eq!(*calls.lock().unwrap(), 0);
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
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
        .await
        .unwrap();
    assert_eq!(outcome.reply, "done");
    assert_eq!(*fail_calls.lock().unwrap(), 1);
    assert_eq!(*ok_calls.lock().unwrap(), 0);
    // ok_tool 因依赖跳过，仍计入轨迹且 succeeded=false。
    let ok_result = outcome
        .agent
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
    let err = run_agent_loop(session, registry, test_context(), 1, None, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, "tool_loop_limit");
    assert_eq!(err.stage, "tool_loop");
    let diagnostics = err.agent.expect("missing agent diagnostics");
    assert_eq!(diagnostics.model_rounds, 2);
    assert_eq!(diagnostics.stop_reason, Some(AgentStopReason::MaxRounds));
    assert_eq!(diagnostics.emitted_tools, vec!["echo", "echo"]);
    assert_eq!(diagnostics.executed_tools, vec!["echo"]);
    assert_eq!(diagnostics.tool_results.len(), 1);
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
    let outcome = run_agent_loop(Box::new(session), registry, test_context(), 1, None, None)
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
    let err = run_agent_loop(session, ToolRegistry::new(), test_context(), 3, None, None)
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
    let err = run_agent_loop(session, registry, test_context(), 0, None, None)
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
    let outcome = run_agent_loop(session, registry, test_context(), 3, None, None)
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

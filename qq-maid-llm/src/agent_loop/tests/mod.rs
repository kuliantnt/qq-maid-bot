//! Agent Loop 控制器的纯逻辑单测。
//!
//! 协议适配（Responses / Chat Completions）的端到端验证保留在各自 provider
//! 模块的测试中；这里只覆盖统一循环控制本身：无工具回答、单工具、单轮多工具、
//! 多轮继续、业务失败、执行异常、最大轮数、prepare-before-execute 顺序与
//! usage 合并。

use super::runner;
use super::*;
use crate::error::LlmError;
use crate::provider::{AgentStopReason, types::TokenUsage};
use crate::tool::{
    ToolCallDependency, ToolContext, ToolEffect, ToolMetadata, ToolOutput, ToolRegistry,
};
use async_trait::async_trait;
use qq_maid_common::identity_context::{
    ConversationKind, ExecutionActorContext, ExecutionConversationContext,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Barrier, Mutex as StdMutex},
};
use tokio::sync::Notify;

fn test_context() -> ToolContext {
    ToolContext {
        task_id: "task-1".to_owned(),
        actor: ExecutionActorContext {
            user_id: Some("u1".to_owned()),
            group_member_role: None,
        },
        conversation: ExecutionConversationContext {
            platform: "test".to_owned(),
            account_id: None,
            kind: ConversationKind::Private,
            target_id: Some("u1".to_owned()),
            scope_id: "private:u1".to_owned(),
            interaction_scope_id: "private:u1".to_owned(),
        },
        tool_call_id: None,
        execution_deadline: None,
    }
}

/// 脚本化单步会话：按预设脚本依次返回 `AgentStep`，并记录每次 advance 的入参。
#[allow(clippy::type_complexity)]
struct ScriptedSession {
    provider: &'static str,
    model: &'static str,
    script: Vec<AgentStep>,
    delays: Vec<std::time::Duration>,
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
            delays: Vec::new(),
            observed: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    fn with_delays(
        provider: &'static str,
        model: &'static str,
        script: Vec<AgentStep>,
        delays: Vec<std::time::Duration>,
    ) -> Self {
        assert_eq!(script.len(), delays.len());
        Self {
            provider,
            model,
            script,
            delays,
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
        if !self.delays.is_empty() {
            tokio::time::sleep(self.delays.remove(0)).await;
        }
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

struct SlowReadOnlyTool {
    calls: Arc<StdMutex<usize>>,
    delay: std::time::Duration,
}

struct SlowFailingReadOnlyTool {
    calls: Arc<StdMutex<usize>>,
    delay: std::time::Duration,
}

struct NamedSlowReadOnlyTool {
    name: &'static str,
    calls: Arc<StdMutex<usize>>,
    delay: std::time::Duration,
}

#[async_trait]
impl crate::tool::Tool for NamedSlowReadOnlyTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: self.name.to_owned(),
            description: "named read-only tool".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, _ctx: ToolContext, arguments: Value) -> Result<ToolOutput, LlmError> {
        *self.calls.lock().unwrap() += 1;
        tokio::time::sleep(self.delay).await;
        Ok(ToolOutput::json(json!({
            "ok": true,
            "value": arguments["value"],
        })))
    }
}

#[async_trait]
impl crate::tool::Tool for SlowReadOnlyTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: "search".to_owned(),
            description: "read-only search".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, _ctx: ToolContext, arguments: Value) -> Result<ToolOutput, LlmError> {
        *self.calls.lock().unwrap() += 1;
        tokio::time::sleep(self.delay).await;
        Ok(ToolOutput::json(json!({
            "ok": true,
            "value": arguments["value"],
        })))
    }
}

#[async_trait]
impl crate::tool::Tool for SlowFailingReadOnlyTool {
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            name: "search".to_owned(),
            description: "failing read-only search".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"],
                "additionalProperties": false
            }),
        }
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, _ctx: ToolContext, arguments: Value) -> Result<ToolOutput, LlmError> {
        *self.calls.lock().unwrap() += 1;
        tokio::time::sleep(self.delay).await;
        Ok(ToolOutput::json(json!({
            "ok": false,
            "error_code": "search_failed",
            "value": arguments["value"],
        })))
    }
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

mod budget_timeout;
mod cancel;
mod fallback;
mod streaming;
mod tool_execution;

#[allow(dead_code)]
fn _ensure_value_imported(_: Value) {}

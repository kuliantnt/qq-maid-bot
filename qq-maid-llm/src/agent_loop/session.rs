//! Provider 侧单步会话契约。
//!
//! [`AgentStepSession`] 是 Provider 把各自协议的一次模型请求转换为统一
//! [`AgentStep`](super::types::AgentStep) 的挂载点。实现方持有自己的协议形态
//! 对话状态（如 Responses `input` 或 Chat Completions `messages`），并在
//! `advance` 中完成：构建 payload、上下文预算校验、发送请求、解析 usage 与
//! tool calls / 最终文本、回填上一轮工具结果。
//!
//! **不应**在此决定最大轮数或 Loop 退出条件——那是 [`run_agent_loop`](super::runner::run_agent_loop)
//! 的统一职责。这也是 #138 的核心收敛点：不同 Provider 不再各自决定退出条件。

use std::sync::{Arc, atomic::AtomicUsize};

use crate::error::LlmError;

use super::types::{AgentStep, AgentTextDeltaSink, AgentToolResult};

/// 单次 Agent 流式推进的脱敏诊断快照。
///
/// 这里只允许保存协议状态与计数，不得保存正文、SSE 原文、工具参数或鉴权信息。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentStreamingDiagnostics {
    pub fallback_reason: Option<String>,
    pub chunk_count: usize,
    pub sse_event_count: usize,
    pub saw_done: bool,
    pub saw_completed: bool,
    pub buffered_delta_count: usize,
    pub active_function_call_count: usize,
}

/// Provider 侧单步会话：把各自协议的一次模型请求转换为统一 `AgentStep`。
#[async_trait::async_trait]
pub trait AgentStepSession: Send {
    /// Provider 名（用于 metrics 与日志）。
    fn provider(&self) -> &str;
    /// 本会话实际使用的模型名（已解析前缀，用于 metrics）。
    fn model(&self) -> &str;
    /// 返回最近一次流式推进的脱敏协议诊断。
    fn streaming_diagnostics(&self) -> AgentStreamingDiagnostics {
        AgentStreamingDiagnostics::default()
    }
    /// 可选的流活动计数器。Provider 每收到一个有效协议事件即递增，
    /// runner 用它区分“首包超时”和“已经开始传输但尚未完成”。
    fn streaming_activity_counter(&self) -> Option<Arc<AtomicUsize>> {
        None
    }
    /// 用上一轮工具执行结果推进一步。
    ///
    /// - `results`：上一轮工具执行结果；首轮为空切片。
    /// - `allow_tool_calls`：是否允许本轮产生工具调用。当为 `false` 时，协议层
    ///   必须显式设置等价于 `tool_choice=none` 的禁用选项；Provider 违反约束仍
    ///   返回工具调用时，由 `run_agent_loop` 受控终止。
    async fn advance(
        &mut self,
        results: &[AgentToolResult],
        allow_tool_calls: bool,
    ) -> Result<AgentStep, LlmError>;

    /// 可选的流式单轮推进。
    ///
    /// 返回 `Ok(None)` 表示当前 Provider/协议不支持 Tool Loop 流式推进，调用方
    /// 应回退到 [`advance`](Self::advance)。实现方必须遵守：
    ///
    /// - `allow_tool_calls=true` 时，模型文本 delta 只能先在 Provider 内部缓存；
    ///   只有流结束且确认本轮没有 tool call，才可作为最终回答释放给 `text_delta_sink`。
    /// - `allow_tool_calls=false` 时，本轮已进入禁止继续工具调用的最终回答阶段，
    ///   可以边接收真实 provider delta 边转发。
    /// - 如果本轮产生 tool call，不得向 `text_delta_sink` 发送任何模型草稿。
    /// - 一旦已经发送用户可见 delta，后续错误必须原样返回，不能改走非流式重放。
    /// - 流式推进失败或超时时，会话状态必须仍可用同一批 `results` 执行一次
    ///   `advance`；Provider 不得在流式响应完整结束前提交本轮协议状态。
    async fn advance_streaming(
        &mut self,
        _results: &[AgentToolResult],
        _allow_tool_calls: bool,
        _text_delta_sink: AgentTextDeltaSink,
    ) -> Result<Option<AgentStep>, LlmError> {
        Ok(None)
    }
}

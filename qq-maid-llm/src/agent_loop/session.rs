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

use crate::error::LlmError;

use super::types::{AgentStep, AgentTextDeltaSink, AgentToolResult};

/// Provider 侧单步会话：把各自协议的一次模型请求转换为统一 `AgentStep`。
#[async_trait::async_trait]
pub trait AgentStepSession: Send {
    /// Provider 名（用于 metrics 与日志）。
    fn provider(&self) -> &str;
    /// 本会话实际使用的模型名（已解析前缀，用于 metrics）。
    fn model(&self) -> &str;
    /// 用上一轮工具执行结果推进一步。
    ///
    /// - `results`：上一轮工具执行结果；首轮为空切片。
    /// - `allow_tool_calls`：是否允许本轮产生工具调用。当为 `false` 时，
    ///   Responses 可设置 `tool_choice=none`；Chat Completions 等不支持
    ///   显式关闭的协议可忽略此参数，由 `run_agent_loop` 统一兜底最大轮数。
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

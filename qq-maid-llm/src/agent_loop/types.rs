//! Agent Loop 协议无关的契约类型。
//!
//! 这些类型是 `AgentStepSession` 与 `run_agent_loop` 之间的公共语言，也是
//! `LlmProvider::begin_agent_session` 的公开签名组成部分，因此必须 `pub`。
//! 不含任何协议形态（Responses `input` / Chat Completions `messages`）。

use std::{future::Future, pin::Pin, sync::Arc};

use crate::error::LlmError;
use crate::provider::types::{ChatRequest, TokenUsage};
use crate::tool::ToolRegistry;

/// 单次模型请求后，Provider 解析出的统一“下一步动作”。
///
/// 协议无关：无论 Responses 的 `function_call` 还是 Chat Completions 的
/// `tool_calls`，都归一为同一组语义。
#[derive(Debug, Clone)]
pub enum AgentStep {
    /// 模型给出最终文本回复，循环应结束。
    FinalAnswer {
        /// 最终回复正文。
        reply: String,
        /// 本轮模型请求的 token 用量。
        usage: Option<TokenUsage>,
    },
    /// 模型请求执行一批工具调用；循环执行后继续下一轮。
    ToolCalls {
        /// 本批工具调用（同轮可多个）。
        calls: Vec<AgentToolCall>,
        /// 本轮模型请求的 token 用量。
        usage: Option<TokenUsage>,
    },
}

/// 协议无关的工具调用。
#[derive(Debug, Clone)]
pub struct AgentToolCall {
    /// 工具名。
    pub name: String,
    /// 模型下发的稳定调用 ID（无则由 Loop 本地生成回退 ID）。
    pub call_id: String,
    /// 原始 JSON 参数字符串。
    pub arguments: String,
}

/// 回传给 Provider 的工具执行结果摘要。
///
/// 只携带协议回填所需字段（call_id + 输出正文）；是否算业务成功由 `runner`
/// 的 `ToolLoopExecutor` 在 `tool_results` 中单独记录，避免 Provider 理解业务
/// 字段。
#[derive(Debug, Clone)]
pub struct AgentToolResult {
    /// 对应 [`AgentToolCall::call_id`]。
    pub call_id: String,
    /// 回传给模型的工具输出正文（已序列化为字符串）。
    pub output: String,
}

/// Tool Loop 内部产生的受控进度事件。
///
/// 事件只携带服务端白名单工具名和执行结果状态，不包含工具参数、原始输出或
/// provider 协议 payload；上层 Core 可据此映射成用户可见的安全状态提示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolLoopProgressEvent {
    ToolCallStarted { tool_name: String },
    ToolCallFinished { tool_name: String },
    ToolCallFailed { tool_name: String },
}

pub type ToolLoopProgressFuture =
    Pin<Box<dyn Future<Output = Result<(), LlmError>> + Send + 'static>>;

pub type ToolLoopProgressSink =
    Arc<dyn Fn(ToolLoopProgressEvent) -> ToolLoopProgressFuture + Send + Sync + 'static>;

/// 创建 [`AgentStepSession`] 的请求。
#[derive(Clone, Copy)]
pub struct AgentSessionRequest<'a> {
    /// 基础聊天请求（含消息、模型、上下文预算）。
    pub chat: &'a ChatRequest,
    /// 服务端白名单工具；Session 只读取 metadata 构建协议 tool defs，
    /// 不负责执行。
    pub tools: &'a ToolRegistry,
}

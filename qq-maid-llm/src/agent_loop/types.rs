//! Agent Loop 协议无关的契约类型。
//!
//! 这些类型是 `AgentStepSession` 与 `run_agent_loop` 之间的公共语言，也是
//! `LlmProvider::begin_agent_session` 的公开签名组成部分，因此必须 `pub`。
//! 不含任何协议形态（Responses `input` / Chat Completions `messages`）。

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use crate::error::LlmError;
use crate::provider::types::{ChatRequest, TokenUsage};
use crate::tool::ToolRegistry;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Notify;

/// Tool Loop 中单次工具执行的结果摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolExecutionResult {
    pub name: String,
    pub output: Value,
    pub succeeded: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStopReason {
    DirectAnswer,
    ToolUsed,
    Clarify,
    Rejected,
    Failed,
    MaxRounds,
    Timeout,
    Cancelled,
}

impl AgentStopReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DirectAnswer => "direct_answer",
            Self::ToolUsed => "tool_used",
            Self::Clarify => "clarify",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::MaxRounds => "max_rounds",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Agent Runtime 的统一执行轨迹，同时用于成功输出与受控失败。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AgentRunDiagnostics {
    /// 已发起的模型请求次数。首轮请求计为 1；超时或取消的在途请求也计入。
    pub model_rounds: usize,
    /// 模型返回过的结构化工具名，包含未知、未授权和参数非法的调用。
    pub emitted_tools: Vec<String>,
    /// 服务端是否进入过 prepare / 校验 / 执行流程。
    pub tool_execution_attempted: bool,
    /// 已实际开始执行的工具名；参数校验失败或启动前取消不计入。
    pub executed_tools: Vec<String>,
    /// 已经形成可信结果的工具执行摘要。
    pub tool_results: Vec<ToolExecutionResult>,
    /// 本轮是否从 Agent 流式单步回退到非流式单步。
    pub streaming_fallback_used: bool,
    /// Agent Runtime 的最终停止原因；运行中快照为 None。
    pub stop_reason: Option<AgentStopReason>,
}

/// Agent Runtime 与 Core 共享的轨迹快照和取消边界。
#[derive(Debug, Clone)]
pub struct AgentRunHandle {
    diagnostics: Arc<Mutex<AgentRunDiagnostics>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    cancel_notify: Arc<Notify>,
}

impl Default for AgentRunHandle {
    fn default() -> Self {
        Self {
            diagnostics: Arc::new(Mutex::new(AgentRunDiagnostics::default())),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cancel_notify: Arc::new(Notify::new()),
        }
    }
}

impl AgentRunHandle {
    pub fn snapshot(&self) -> AgentRunDiagnostics {
        self.diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn update(&self, update: impl FnOnce(&mut AgentRunDiagnostics)) {
        let mut diagnostics = self
            .diagnostics
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut diagnostics);
    }

    pub fn cancel(&self, reason: AgentStopReason) {
        self.update(|diagnostics| {
            if diagnostics.stop_reason.is_none() {
                diagnostics.stop_reason = Some(reason);
            }
        });
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // 单个 Agent run 只有一个取消 waiter；notify_one 会保留 permit，避免检查与等待间丢通知。
        self.cancel_notify.notify_one();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.cancel_notify.notified().await;
    }
}

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

/// Tool Loop 进度事件接收器，同时承担取消通道语义。
///
/// 返回 `Err` 表示上层 stream 已取消、receiver 已关闭或无法继续安全投递进度；
/// Agent Loop 必须中断后续工具执行。普通日志/观测失败不应通过该 sink 返回。
pub type ToolLoopProgressSink =
    Arc<dyn Fn(ToolLoopProgressEvent) -> ToolLoopProgressFuture + Send + Sync + 'static>;

pub type AgentTextDeltaFuture =
    Pin<Box<dyn Future<Output = Result<(), LlmError>> + Send + 'static>>;

/// Tool Loop 最终用户可见正文增量接收器。
///
/// 该 sink 只能接收已经确认属于最终回答的文本；Provider 在仍允许工具调用的轮次
/// 必须先缓存模型 delta，确认没有 tool call 后再释放，避免外显工具轮草稿。
pub type AgentTextDeltaSink = Arc<dyn Fn(String) -> AgentTextDeltaFuture + Send + Sync + 'static>;

/// 创建 [`AgentStepSession`] 的请求。
#[derive(Clone, Copy)]
pub struct AgentSessionRequest<'a> {
    /// 基础聊天请求（含消息、模型、上下文预算）。
    pub chat: &'a ChatRequest,
    /// 服务端白名单工具；Session 只读取 metadata 构建协议 tool defs，
    /// 不负责执行。
    pub tools: &'a ToolRegistry,
}

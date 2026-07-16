//! Agent Loop 协议无关的契约类型。
//!
//! 这些类型是 `AgentStepSession` 与 `run_agent_loop` 之间的公共语言，也是
//! `LlmProvider::begin_agent_session` 的公开签名组成部分，因此必须 `pub`。
//! 不含任何协议形态（Responses `input` / Chat Completions `messages`）。

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::error::LlmError;
use crate::provider::types::{ChatRequest, TokenUsage};
use crate::tool::ToolRegistry;
use serde::Serialize;
use serde_json::Value;
use tokio::{sync::Notify, time::Instant};

/// Agent 请求默认留给最后一轮无工具回答的时间。
///
/// 实际预留还会按总请求时长的四分之一裁剪，避免短请求只剩收尾预算。
pub const DEFAULT_FINALIZATION_RESERVE: Duration = Duration::from_secs(25);

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
    /// 整次请求已发起的模型请求次数，跨候选累计；超时或取消的在途请求也计入。
    pub model_rounds: usize,
    /// 整次请求中模型返回过的结构化工具名，跨候选累计。
    pub emitted_tools: Vec<String>,
    /// 服务端是否进入过 prepare / 校验 / 执行流程。
    pub tool_execution_attempted: bool,
    /// 整次请求中已实际开始执行的工具名，跨候选累计；参数校验失败或启动前取消不计入。
    pub executed_tools: Vec<String>,
    /// 已实际开始执行且可能修改外部状态的工具名。
    pub side_effecting_tools_started: Vec<String>,
    /// 已经明确完成的工具执行摘要，包含成功结果和结构化失败结果。
    pub tool_results: Vec<ToolExecutionResult>,
    /// 已开始但尚未形成可信结果的工具名。
    ///
    /// Core 取消或超时等待预算耗尽时，该字段明确表示副作用结果仍不确定，
    /// 调用方不得据此自动重试或切换候选重新执行。
    pub tools_with_unknown_result: Vec<String>,
    /// 本轮是否从 Agent 流式单步回退到非流式单步。
    pub streaming_fallback_used: bool,
    /// Agent Runtime 的最终停止原因；运行中快照为 None。
    pub stop_reason: Option<AgentStopReason>,
}

/// Agent Runtime 与 Core 共享的轨迹快照和取消边界。
#[derive(Debug, Clone)]
pub struct AgentRunHandle {
    state: Arc<Mutex<AgentRunState>>,
    cancel_notify: Arc<Notify>,
}

#[derive(Debug, Default)]
struct AgentRunState {
    diagnostics: AgentRunDiagnostics,
    pending_attempt: Option<AgentAttemptBaseline>,
    deadline: Option<Instant>,
    finalization_reserve: Duration,
}

impl Default for AgentRunHandle {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(AgentRunState::default())),
            cancel_notify: Arc::new(Notify::new()),
        }
    }
}

impl AgentRunHandle {
    /// 创建带统一请求截止时间的运行句柄，并使用默认最终回答预留。
    pub fn with_timeout(request_timeout: Duration) -> Self {
        Self::with_timeout_and_finalization_reserve(request_timeout, DEFAULT_FINALIZATION_RESERVE)
    }

    /// 创建带统一请求截止时间的运行句柄，并按总预算裁剪配置的最终回答预留。
    pub fn with_timeout_and_finalization_reserve(
        request_timeout: Duration,
        configured_reserve: Duration,
    ) -> Self {
        let reserve = std::cmp::min(configured_reserve, request_timeout / 4);
        Self {
            state: Arc::new(Mutex::new(AgentRunState {
                deadline: Some(Instant::now() + request_timeout),
                finalization_reserve: reserve,
                ..AgentRunState::default()
            })),
            cancel_notify: Arc::new(Notify::new()),
        }
    }

    pub fn snapshot(&self) -> AgentRunDiagnostics {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .diagnostics
            .clone()
    }

    pub(crate) fn update(&self, update: impl FnOnce(&mut AgentRunDiagnostics)) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        update(&mut state.diagnostics);
    }

    /// 开始一个候选模型 attempt。
    ///
    /// diagnostics 的计数和工具轨迹属于整次请求，不能在候选间清空；这里只清理
    /// 上一候选的临时终止原因，并返回本 attempt 写入累计 Vec 时使用的基线。
    pub(crate) fn begin_candidate_attempt(&self) -> Result<AgentAttemptBaseline, LlmError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(reason) = request_termination_reason(&state.diagnostics) {
            return Err(termination_error(reason, "before model candidate")
                .with_agent(state.diagnostics.clone()));
        }
        if matches!(
            state.diagnostics.stop_reason,
            Some(AgentStopReason::Failed | AgentStopReason::MaxRounds)
        ) {
            state.diagnostics.stop_reason = None;
        }
        let baseline = AgentAttemptBaseline::from_diagnostics(&state.diagnostics);
        state.pending_attempt = Some(baseline);
        Ok(baseline)
    }

    /// Provider 默认兼容路径可独立接收请求，也可由 routing 预先开始候选。
    /// 这里只确保候选已注册，不重复清理终止态或重置基线。
    pub(crate) fn ensure_candidate_attempt(&self) -> Result<AgentAttemptBaseline, LlmError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(reason) = request_termination_reason(&state.diagnostics) {
            return Err(termination_error(reason, "before agent session")
                .with_agent(state.diagnostics.clone()));
        }
        if let Some(baseline) = state.pending_attempt {
            return Ok(baseline);
        }
        if matches!(
            state.diagnostics.stop_reason,
            Some(AgentStopReason::Failed | AgentStopReason::MaxRounds)
        ) {
            state.diagnostics.stop_reason = None;
        }
        let baseline = AgentAttemptBaseline::from_diagnostics(&state.diagnostics);
        state.pending_attempt = Some(baseline);
        Ok(baseline)
    }

    /// Runner 只领取上层已开始的候选基线，不再承担 attempt 初始化职责。
    pub(crate) fn take_candidate_attempt(&self) -> AgentAttemptBaseline {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .pending_attempt
            .take()
            .unwrap_or_else(|| AgentAttemptBaseline::from_diagnostics(&state.diagnostics))
    }

    /// 在线性化边界内记录一次模型请求已经发起。
    pub(crate) fn start_model_round(&self) -> Result<(), LlmError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(reason) = request_termination_reason(&state.diagnostics) {
            return Err(termination_error(reason, "before model request")
                .with_agent(state.diagnostics.clone()));
        }
        state.diagnostics.model_rounds += 1;
        Ok(())
    }

    /// 在异步 provider 调用返回后重新确认请求未被外部终止。
    pub(crate) fn ensure_request_active(&self, context: &str) -> Result<(), LlmError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(reason) = request_termination_reason(&state.diagnostics) {
            return Err(termination_error(reason, context).with_agent(state.diagnostics.clone()));
        }
        Ok(())
    }

    /// 检查外部终止并原子记录工具已经越过副作用启动边界。
    pub(crate) fn try_start_tool(
        &self,
        tool_name: &str,
        effect: crate::tool::ToolEffect,
    ) -> Result<(), LlmError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(reason) = request_termination_reason(&state.diagnostics) {
            return Err(termination_error(reason, "before tool execution")
                .with_agent(state.diagnostics.clone()));
        }
        state.diagnostics.executed_tools.push(tool_name.to_owned());
        if effect == crate::tool::ToolEffect::SideEffecting {
            state
                .diagnostics
                .side_effecting_tools_started
                .push(tool_name.to_owned());
            state
                .diagnostics
                .tools_with_unknown_result
                .push(tool_name.to_owned());
        }
        Ok(())
    }

    /// 当前剩余请求预算；未配置 deadline 的兼容调用返回 None。
    pub fn remaining_budget(&self) -> Option<Duration> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    /// 工具执行最晚截止时间。该时间已经扣除最终回答预留，工具不得占用这段预算。
    pub(crate) fn tool_execution_deadline(&self) -> Option<Instant> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.deadline.map(|deadline| {
            deadline
                .checked_sub(state.finalization_reserve)
                .unwrap_or(deadline)
        })
    }

    /// 是否应停止继续调用工具，把剩余时间留给基于已有结果的简短收尾。
    pub(crate) fn should_preserve_finalization_budget(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.deadline.is_some_and(|deadline| {
            deadline.saturating_duration_since(Instant::now()) <= state.finalization_reserve
        })
    }

    /// 是否已经获得至少一个可以支撑事实回答的成功工具结果。
    pub(crate) fn has_successful_tool_result_since(&self, baseline: usize) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.diagnostics.tool_results[baseline..]
            .iter()
            .any(|result| result.succeeded)
    }

    /// 是否已有工具形成明确结果，不论成功或失败。
    ///
    /// 已完成失败可安全回填给最终模型解释，但不能由
    /// [`has_successful_tool_result_since`](Self::has_successful_tool_result_since) 当作事实依据。
    pub(crate) fn has_completed_tool_result_since(&self, baseline: usize) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.diagnostics.tool_results.len() > baseline
    }

    /// 工具真实结果先写入共享轨迹，再投递完成进度，避免 sink 失败遮蔽结果。
    pub(crate) fn record_tool_result(&self, result: ToolExecutionResult) {
        self.update(|diagnostics| {
            if let Some(index) = diagnostics
                .tools_with_unknown_result
                .iter()
                .position(|name| name == &result.name)
            {
                diagnostics.tools_with_unknown_result.remove(index);
            }
            diagnostics.tool_results.push(result);
        });
    }

    pub(crate) fn set_stop_reason(&self, reason: AgentStopReason) {
        self.update(|diagnostics| {
            // Core 的整次请求终止信号优先于候选内部失败，避免清理过程中被改回 failed。
            if !matches!(
                diagnostics.stop_reason,
                Some(AgentStopReason::Timeout | AgentStopReason::Cancelled)
            ) {
                diagnostics.stop_reason = Some(reason);
            }
        });
    }

    pub(crate) fn set_stop_reason_if_unset(&self, reason: AgentStopReason) {
        self.update(|diagnostics| {
            if diagnostics.stop_reason.is_none() {
                diagnostics.stop_reason = Some(reason);
            }
        });
    }

    pub fn cancel(&self, reason: AgentStopReason) {
        debug_assert!(matches!(
            reason,
            AgentStopReason::Timeout | AgentStopReason::Cancelled
        ));
        // 外部终止与 attempt / 工具启动共用同一把锁；锁内写入即为不可逆线性化点。
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if request_termination_reason(&state.diagnostics).is_none() {
            state.diagnostics.stop_reason = Some(reason);
        }
        drop(state);
        // 单个 Agent run 只有一个取消 waiter；notify_one 会保留 permit，避免检查与等待间丢通知。
        self.cancel_notify.notify_one();
    }

    pub fn is_cancelled(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        request_termination_reason(&state.diagnostics).is_some()
    }

    pub(crate) async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.cancel_notify.notified().await;
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AgentAttemptBaseline {
    pub(crate) emitted_tools: usize,
    pub(crate) executed_tools: usize,
    pub(crate) tool_results: usize,
}

impl AgentAttemptBaseline {
    fn from_diagnostics(diagnostics: &AgentRunDiagnostics) -> Self {
        Self {
            emitted_tools: diagnostics.emitted_tools.len(),
            executed_tools: diagnostics.executed_tools.len(),
            tool_results: diagnostics.tool_results.len(),
        }
    }
}

fn request_termination_reason(diagnostics: &AgentRunDiagnostics) -> Option<AgentStopReason> {
    diagnostics.stop_reason.filter(|reason| {
        matches!(
            reason,
            AgentStopReason::Timeout | AgentStopReason::Cancelled
        )
    })
}

fn termination_error(reason: AgentStopReason, context: &str) -> LlmError {
    match reason {
        AgentStopReason::Timeout => LlmError::new(
            "timeout",
            format!("agent run timed out {context}"),
            "agent_loop",
        ),
        AgentStopReason::Cancelled => LlmError::new(
            "cancelled",
            format!("agent run cancelled {context}"),
            "agent_loop",
        ),
        _ => unreachable!("request termination must be timeout or cancelled"),
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

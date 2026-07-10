//! Provider 无关的统一 Agent Loop 状态机（#138 核心产物）。
//!
//! 把“请求模型下一步动作 → 直接回答候选 → 一个或多个 tool call → 工具执行
//! 结果 → 继续下一轮 → 业务失败/执行异常 → 最大轮数 → 最终完成”收敛为单一
//! 循环控制，避免 Responses 与 Chat Completions 两条协议分支各自维护退出
//! 条件。
//!
//! ## 职责边界
//!
//! - **统一循环控制（本模块）**：轮次推进、最大轮数、`tool_loop_limit` 退出、
//!   同轮工具的 prepare-before-execute、依赖跳过、`ok:false` 业务失败识别、
//!   执行异常转结构化输出、`executed_tools` / `tool_results` 轨迹、usage
//!   合并、`ChatOutcome` 装配。核心实现在 [`runner`]。
//! - **协议适配（Provider 侧 [`AgentStepSession`])**：把各自协议的一次模型
//!   请求转换为统一 [`AgentStep`]，并维护自己的协议形态对话状态（Responses
//!   `input` 或 Chat Completions `messages`）。Provider 不再决定最大轮数或
//!   退出条件。
//! - **业务后处理（Core）**：何时进入 Loop、session/history 提交时机、Todo
//!   成功文案验真、diagnostics。工具副作用只由本模块的 `ToolLoopExecutor`
//!   执行一次，Core 后处理不再重复调用工具。
//!
//! ## 扩展点
//!
//! [`AgentStepSession`] 是后续 #139（澄清/Pending 恢复）与 #140（Core 事件流）
//! 的挂载点：未来可在 `advance` 之间插入暂停/恢复或事件回调，而不需要让
//! `qq-maid-llm` 反向依赖 `qq-maid-core`。届时新增 `event`、`resume` 等子
//! 模块即可，不必改动 `runner` 与 `types`。未适配 Tool Calling 的 provider
//! 只需让 `begin_agent_session` 返回 `None`，`LlmProvider::chat_with_tools`
//! 默认实现会安全回退到普通 `chat`，保留旧路径。
//!
//! ## 子模块布局
//!
//! | 子模块 | 职责 |
//! |--------|------|
//! | [`types`] | 协议无关的 `AgentStep` / `AgentToolCall` / `AgentToolResult` / `AgentSessionRequest` |
//! | [`session`] | `AgentStepSession` trait（Provider 单步适配契约） |
//! | [`runner`] | `run_agent_loop` 统一循环控制 + usage 合并 |

pub mod runner;
pub mod session;
pub mod types;

pub use runner::{run_agent_loop, run_agent_loop_with_handle};
pub use session::{AgentStepSession, AgentStreamingDiagnostics};
pub use types::{
    AgentRunDiagnostics, AgentRunHandle, AgentSessionRequest, AgentStep, AgentStopReason,
    AgentTextDeltaFuture, AgentTextDeltaSink, AgentToolCall, AgentToolResult, ToolExecutionResult,
    ToolLoopProgressEvent, ToolLoopProgressFuture, ToolLoopProgressSink,
};

#[cfg(test)]
mod tests;

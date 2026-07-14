//! OpenAI Responses 原生 Function Tool Loop 的协议适配层。
//!
//! 本模块只处理 Responses 协议层的 function call / function_call_output 往返，
//! 把一次模型请求转换为统一 [`crate::agent_loop::AgentStep`]。轮次推进、最大轮数、
//! 工具执行和退出条件由 `qq_maid_llm::agent_loop::run_agent_loop` 统一控制；
//! 本模块不维护自己的循环。具体业务能力由上层 crate 通过 `ToolRegistry` 注册，
//! 避免 LLM crate 反向依赖 Core。

mod diagnostics;
mod payload;
mod response;
mod session;
mod streaming;

pub(crate) use session::ResponsesAgentSession;

#[cfg(test)]
mod tests;

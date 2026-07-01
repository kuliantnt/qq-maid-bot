//! QQ Maid Bot 的 LLM 基础设施 crate。
//!
//! 本 crate 只负责模型调用协议、Provider 路由、fallback、SSE、usage、
//! 健康观测和 OpenAI Web Search 协议；业务 prompt、session、memory、
//! todo、RSS 排版等仍保留在上层业务 crate。

pub mod agent_loop;
pub mod config;
pub mod context_budget;
pub mod error;
pub mod metrics;
pub mod provider;
pub mod service;
pub mod sse;
pub mod tool;
pub mod web_search;

pub use error::{ErrorInfo, LlmError};
pub use service::LlmService;

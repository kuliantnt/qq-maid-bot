//! 通用工具函数和类型模块。
//!
//! 提供 LLM 调用指标采集（metrics）、SSE 解析和时间上下文解析等辅助功能。

pub mod metrics;
pub(crate) mod sse;
pub mod time_context;

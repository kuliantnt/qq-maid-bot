//! 时间上下文兼容入口。
//!
//! 实现已抽到 `qq-maid-common`，这里保留原模块路径，避免 LLM 内部 flow
//! 和测试大面积改 import。

pub use qq_maid_common::time_context::*;

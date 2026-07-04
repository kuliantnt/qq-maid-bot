//! Core 进程内服务契约。
//!
//! Gateway 只依赖本模块暴露的强类型边界，不直接访问 Core 内部 store、HTTP
//! route 或 provider 细节。scope_key 统一由 Core 根据会话目标派生，避免跨层出现
//! 两套会话归属事实。

mod errors;
mod handle;
mod streaming;
mod types;

pub use handle::CoreHandle;
pub use qq_maid_llm::provider::status::{UpstreamState, UpstreamStatusSnapshot};
pub use types::*;

pub(crate) use errors::{error_core_error, warn_core_error};
pub(crate) use streaming::{
    ProgressStatusConfig, output_policy_for_stream, start_core_response_stream,
};

#[cfg(test)]
pub(crate) use errors::safe_error_message;

#[cfg(test)]
mod tests;

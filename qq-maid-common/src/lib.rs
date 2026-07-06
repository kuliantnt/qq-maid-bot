//! 小女仆机器人 共享基础工具。
//!
//! 这里只放 gateway 和 LLM 都可能复用、且不依赖业务状态的通用逻辑。

pub mod identity_context;
pub mod input_part;
pub mod markdown_strip;
pub mod redaction;
pub mod text;
pub mod time_context;

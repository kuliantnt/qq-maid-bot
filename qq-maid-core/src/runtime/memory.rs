//! 记忆操作的重导出。
//!
//! 将 `storage::memory` 模块中的全部公开类型和函数重新导出到运行时层，
//! 供 runtime 模块内的其他子模块统一使用。

pub use crate::storage::memory::*;

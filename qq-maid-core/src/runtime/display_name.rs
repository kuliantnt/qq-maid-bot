//! 手动展示名存储的重导出。
//!
//! 将 `storage::display_name` 中的公开类型重新导出到运行时层，供 respond 等子模块统一使用。

pub use crate::storage::display_name::*;

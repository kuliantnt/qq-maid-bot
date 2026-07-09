//! 会话操作的重导出。
//!
//! 将 `storage::session` 模块中的全部公开类型和函数重新导出到运行时层。

pub use crate::runtime::freshness::query_is_fresh;
pub use crate::storage::session::*;

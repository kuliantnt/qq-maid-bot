//! 待办操作的重导出。
//!
//! 将 `storage::todo` 模块中的全部公开类型和函数重新导出到运行时层。
//!
//! `ops` 子模块提供统一的状态变更门面，负责把存储层变更和 session 快照
//! 副作用聚合到一处，供指令侧和工具调用侧共用，避免两套链路各自维护
//! “清空 `last_todo_query` / 更新 `last_todo_action`”的时序。
//!
//! `edit_patch` 子模块提供指令侧和工具调用侧共用的编辑增量补丁类型与 apply 逻辑。

pub mod edit_patch;
pub mod ops;
pub mod status;

pub use crate::storage::todo::*;
pub use edit_patch::TodoEditPatch;

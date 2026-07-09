//! Todo 展示格式化薄适配。
//!
//! 具体 Todo 展示语义已迁入 `runtime/tools/todo/format.rs`，respond 层只保留
//! 旧模块路径下的转发，避免命令分发和 pending 适配继续承载业务实现。

pub(crate) use crate::runtime::tools::todo::format::*;

//! Todo Tool 回执薄适配。
//!
//! 具体 Tool 结果聚合、回执生成和相关列表快照刷新已迁入
//! `runtime/tools/todo/receipt.rs`，respond 层只消费领域产出的展示结果。

pub(crate) use crate::runtime::tools::todo::receipt::*;

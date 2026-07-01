//! Markdown 剥离工具的兼容入口。
//!
//! 实现已迁移到 `qq-maid-common::markdown_strip`，这里保留原模块路径，
//! 避免 `runtime/respond` 内部各 flow（聊天、记忆、天气、帮助等）以及测试
//! 大面积改 import。新增使用点应直接引用 `qq_maid_common::markdown_strip`。

pub use qq_maid_common::markdown_strip::strip_markdown_for_chat;

//! 运行时层模块导出。
//!
//! 将聊天机器人的运行时能力（slash 命令、待办、记忆、联网搜索、天气查询等）
//! 以模块形式统一对外暴露，供上层调度使用。

pub mod command;
pub mod display_name;
pub mod group_role;
pub mod knowledge;
pub mod memory;
pub mod notification;
pub mod pending;
pub mod prompt;
pub mod push;
pub mod respond;
pub mod rss;
pub mod session;
pub mod tools;
pub mod train;
pub mod translation;
pub mod visible_entity;
pub mod weather;

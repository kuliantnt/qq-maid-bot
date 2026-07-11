//! 数据持久化存储模块。
//!
//! 提供长期记忆（memory）、会话、通知、知识库和 SQLite 数据库基础能力。
//! 各业务域共用项目级 SQLite。SQLite 连接、PRAGMA 和 migration
//! 统一放在 `database` 模块，业务模块只保留自身表结构和查询逻辑。

pub mod database;
pub mod display_name;
pub mod identity_rebaseline;
pub mod knowledge;
pub mod memory;
pub mod migrations;
pub mod notification;
pub mod session;

pub use migrations::APP_MIGRATIONS;

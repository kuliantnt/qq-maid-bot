//! 数据持久化存储模块。
//!
//! 提供长期记忆（memory）、会话（session）、待办事项（todo）和 SQLite 数据库能力。
//! Memory、Session、Todo、RSS 共用项目级 SQLite。SQLite 连接、PRAGMA 和 migration
//! 统一放在 `database` 模块，业务模块只保留自身表结构和查询逻辑。

pub mod database;
pub mod memory;
pub mod migrations;
pub mod rss;
pub mod session;
pub mod todo;

pub use migrations::APP_MIGRATIONS;

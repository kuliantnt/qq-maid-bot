//! Todo Agent 在 Respond 边界的集成测试。
//!
//! 这里只验证高层编排语义；Tool schema、参数解析、内部编号和持久化细节由
//! `runtime::tools::todo` 的领域测试负责。

mod guard;
mod pending;
mod query;
mod write;

//! Todo Tool。
//!
//! 这些 Tool 只把模型参数适配到现有 TodoStore、Session 快照和 pending 机制。
//! 内部 ID 不返回给模型；跨轮次编号只来自用户实际看到的列表，Tool Loop 内部
//! `list_todos` 查询只保留在当前 task 的临时选择上下文中。
//!
//! 模块拆分（保持公共导出与历史不变）：
//! - `common`：常量、选择/引用类型与参数解析 helper、错误转换。
//! - `scope`：`TodoToolScope` 与可见编号 / 最近对象解析。
//! - `selection`：prepare/execute 共用的预解析与结果映射helper。
//! - `json`：面向模型的 JSON 序列化与状态文案。
//! - `list`/`create`/`complete`/`edit`/`cancel`/`restore`/`delete`：各 Tool 实现。

mod common;
mod json;
mod scope;
mod selection;

mod cancel;
mod complete;
mod create;
mod delete;
mod edit;
mod list;
mod restore;

pub use cancel::CancelTodoTool;
pub use complete::CompleteTodoTool;
pub use create::CreateTodoTool;
pub use delete::DeleteTodoTool;
pub use edit::EditTodoTool;
pub use list::ListTodoTool;
pub use restore::RestoreTodoTool;

#[cfg(test)]
mod tests;

//! Core 业务 Tool 适配层。
//!
//! 本模块只负责把现有业务执行器包装成模型可调用 Tool。未来 Skill 层可以引用这些
//! Tool 的元数据和说明，但不应让 Tool 依赖 Skill loader 或 SKILL.md 文件。

mod todo;
mod weather;

pub use todo::{
    CancelTodoTool, CompleteTodoTool, CreateTodoTool, DeleteTodoTool, ListTodoTool, RestoreTodoTool,
};
pub use weather::WeatherTool;

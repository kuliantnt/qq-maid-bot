//! Core 业务 Tool 适配层。
//!
//! 本模块只负责把现有业务执行器包装成模型可调用 Tool。未来 Skill 层可以引用这些
//! Tool 的元数据和说明，但不应让 Tool 依赖 Skill loader 或 SKILL.md 文件。

mod radar;
mod rss;
mod search;
pub(crate) mod todo;
mod train;
mod weather;

pub use radar::{
    ClaudeModelMetric, ClaudeRadarSummary, CodexModelMetric, CodexRadarSummary, DynRadarExecutor,
    RadarExecutor, RadarIssueTarget, RadarSnapshot, RadarSourceFailure, RadarSourceKind,
    RadarTarget, build_radar_executor, radar_feedback_url, radar_site_url,
};
pub use rss::{RssManageSubscriptionsTool, RssRecentItemsTool};
pub(crate) use search::{WEB_SEARCH_QUERY_MAX_LENGTH, WEB_SEARCH_TOOL_NAME, WebSearchDeltaHandler};
pub use search::{WebSearchTool, WebSearchToolRequest};
pub(crate) use todo::SelectionScope;
pub use todo::{
    CompleteTodoTool, CreateTodoTool, DeleteTodoTool, EditTodoTool, GetTodoTool, ListTodoTool,
    MergeTodoTool, RestoreTodoTool,
};
pub(crate) use todo::{
    TodoReminderScheduler, TodoReminderSchedulerConfig, TodoReminderSentHook, cancel_reminder_task,
    cancel_reminder_task_by_id,
};
pub use train::TrainScheduleTool;
pub use weather::WeatherTool;

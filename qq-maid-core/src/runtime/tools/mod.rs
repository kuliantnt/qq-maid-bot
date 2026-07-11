//! Core 业务 Tool 适配层。
//!
//! 本模块只负责把现有业务执行器包装成模型可调用 Tool。未来 Skill 层可以引用这些
//! Tool 的元数据和说明，但不应让 Tool 依赖 Skill loader 或 SKILL.md 文件。

pub(crate) mod agent_presenters;
pub(crate) mod agent_turn;
mod radar;
pub mod rss;
pub(crate) mod search;
pub(crate) mod status;
mod status_classifier;
mod status_semantics;
pub(crate) mod todo;
pub mod train;
pub mod weather;

pub(crate) use agent_turn::{
    ToolTurnDiagnostics, ToolTurnPostprocess, agent_turn_diagnostics, postprocess_tool_turn,
    tool_turn_error_code,
};
pub use radar::{
    ClaudeModelMetric, ClaudeRadarSummary, CodexModelMetric, CodexRadarSummary, DynRadarExecutor,
    RadarExecutor, RadarIssueTarget, RadarSnapshot, RadarSourceFailure, RadarSourceKind,
    RadarTarget, build_radar_executor, radar_feedback_url, radar_site_url,
};
pub use rss::{RssManageSubscriptionsTool, RssRecentItemsTool};
pub(crate) use search::{WEB_SEARCH_QUERY_MAX_LENGTH, WEB_SEARCH_TOOL_NAME, WebSearchDeltaHandler};
pub use search::{WebSearchTool, WebSearchToolRequest};
pub(crate) use status::{StatusAudience, StatusHint, StatusPhase, status_hint_text};
pub(crate) use status_classifier::{
    InteractionDomain, InteractionDomainState, InteractionStateSnapshot, classify_status_hint,
};
pub use todo::{
    CompleteTodoTool, CreateTodoTool, DeleteTodoTool, EditTodoTool, GetTodoTool, ListTodoTool,
    ManageRecurringReminderTool, MergeTodoTool, RestoreTodoTool,
};
/// Respond 层只依赖任务存储抽象名；当前实现由 Todo 业务模块提供。
pub type TaskStore = todo::TodoStore;
/// Respond 层只依赖任务 owner 抽象名；当前实现由 Todo 业务模块提供。
pub type TaskOwner = todo::TodoOwner;
pub(crate) use todo::{
    TodoReminderScheduler, TodoReminderSchedulerConfig, TodoReminderSentHook, TodoScopedToolInputs,
    cancel_reminder_task, cancel_reminder_task_by_id,
    replace_scoped_todo_tools_from_visible_snapshot,
};
pub use train::TrainScheduleTool;
pub use weather::WeatherTool;

//! Respond Tool Runtime。
//!
//! 负责构造服务端白名单 ToolRegistry，并按聊天场景裁剪模型可见工具。
//! Tool 是否成功仍以真实工具执行结果为准，本模块不生成业务成功文案。

use std::sync::Arc;

use crate::{
    config::ResolvedAgentPolicy,
    error::LlmError,
    runtime::rss::RssFetcher,
    runtime::session::{SessionMeta, SessionRecord, SessionStore},
    runtime::tools::{
        CompleteTodoTool, CreateTodoTool, DeleteTodoTool, EditTodoTool, GetTodoTool, ListTodoTool,
        ManageRecurringReminderTool, MergeTodoTool, RestoreTodoTool, RssManageSubscriptionsTool,
        RssRecentItemsTool, TaskStore, TodoScopedToolInputs, ToolTurnPostprocess,
        TrainScheduleTool, WeatherTool, WebSearchTool, postprocess_tool_turn,
        replace_scoped_todo_tools_from_visible_snapshot,
    },
    storage::notification::NotificationOutboxStore,
};
use qq_maid_llm::tool::{DEFAULT_TOOL_TIMEOUT, ToolRegistry};

use super::{RespondExecutors, RespondRequest, RespondStores, llm_service::RespondOutput};

#[derive(Clone)]
pub(crate) struct ToolRuntime {
    registry: ToolRegistry,
    task_store: TaskStore,
    session_store: SessionStore,
    notification_store: NotificationOutboxStore,
}

impl ToolRuntime {
    pub(super) fn new(
        executors: &RespondExecutors,
        stores: &RespondStores,
        rss_fetcher: RssFetcher,
        rss_summary_max_chars: usize,
        rss_seen_retention: usize,
        tool_result_max_chars: usize,
    ) -> Self {
        let mut registry =
            ToolRegistry::new().with_limits(DEFAULT_TOOL_TIMEOUT, tool_result_max_chars);
        // Tool 只通过服务端白名单注册；Todo Tool 复用现有 store、session 快照和 pending。
        for tool in [
            Arc::new(WeatherTool::new(executors.weather_executor.clone()))
                as qq_maid_llm::tool::DynTool,
            Arc::new(TrainScheduleTool::new(executors.train_executor.clone())),
            Arc::new(RssRecentItemsTool::new(stores.rss_store.clone())),
            Arc::new(RssManageSubscriptionsTool::new(
                stores.rss_store.clone(),
                rss_fetcher,
                rss_summary_max_chars,
                rss_seen_retention,
            )),
            Arc::new(WebSearchTool::new(executors.query_executor.clone())),
            Arc::new(ListTodoTool::new(
                stores.task_store.clone(),
                stores.session_store.clone(),
            )),
            Arc::new(GetTodoTool::new(
                stores.task_store.clone(),
                stores.session_store.clone(),
            )),
            Arc::new(CreateTodoTool::new(
                stores.task_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(CompleteTodoTool::new(
                stores.task_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(EditTodoTool::new(
                stores.task_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(RestoreTodoTool::new(
                stores.task_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(DeleteTodoTool::new(
                stores.task_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(MergeTodoTool::new(
                stores.task_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(ManageRecurringReminderTool::new(
                stores.task_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
        ] {
            if let Err(err) = registry.insert(tool) {
                tracing::warn!(
                    error_code = %err.code,
                    error_stage = %err.stage,
                    "failed to register core tool"
                );
            }
        }
        Self {
            registry,
            task_store: stores.task_store.clone(),
            session_store: stores.session_store.clone(),
            notification_store: stores.notification_store.clone(),
        }
    }

    /// 按聊天场景裁剪模型可见工具。
    ///
    /// 群聊即使显式开启 Tool Loop，也只暴露查询类工具，避免自然语言普通消息绕过
    /// slash/pending 边界触发 Todo 写入或其他持久化修改。
    pub(super) fn registry_for_chat(
        &self,
        policy: &ResolvedAgentPolicy,
        req: &RespondRequest,
    ) -> Result<ToolRegistry, LlmError> {
        let tool_names = policy
            .enabled_tools
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut registry = self.registry.subset(&tool_names)?;
        self.replace_scoped_tools_from_request(&mut registry, &policy.enabled_tools, req)?;
        Ok(registry)
    }

    pub(crate) fn registry_for_tool_name(&self, tool_name: &str) -> Result<ToolRegistry, LlmError> {
        self.registry.subset(&[tool_name])
    }

    pub(crate) fn postprocess_tool_turn(
        &self,
        conversation_session: &mut SessionRecord,
        meta: &SessionMeta,
        interaction_meta: &SessionMeta,
        output: RespondOutput,
    ) -> Result<ToolTurnPostprocess, LlmError> {
        postprocess_tool_turn(
            &self.session_store,
            &self.task_store,
            conversation_session,
            meta,
            interaction_meta,
            output,
        )
    }

    fn replace_scoped_tools_from_request(
        &self,
        registry: &mut ToolRegistry,
        enabled_tools: &[String],
        req: &RespondRequest,
    ) -> Result<(), LlmError> {
        let quoted_bot_lookup = req
            .quoted
            .as_ref()
            .is_some_and(|quoted| quoted.lookup_found && quoted.from_bot == Some(true));
        replace_scoped_todo_tools_from_visible_snapshot(TodoScopedToolInputs {
            registry,
            enabled_tools,
            todo_store: &self.task_store,
            session_store: &self.session_store,
            notification_store: &self.notification_store,
            snapshot: req.visible_entity_snapshot.as_ref(),
            platform: &req.platform,
            account_id: req.account_id.as_deref(),
            scope_key: &req.scope_key,
            user_id: req.user_id.as_deref(),
            quoted_bot_lookup,
        })
    }
}

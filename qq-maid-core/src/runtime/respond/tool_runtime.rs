//! Respond Tool Runtime。
//!
//! 负责构造服务端白名单 ToolRegistry，并按聊天场景裁剪模型可见工具。
//! Tool 是否成功仍以真实工具执行结果为准，本模块不生成业务成功文案。

use std::sync::Arc;

use crate::{
    config::ResolvedAgentPolicy,
    error::LlmError,
    runtime::session::{SessionMeta, SessionRecord, SessionStore},
    runtime::tools::rss::RssFetcher,
    runtime::tools::weather::WEATHER_TOOL_NAME,
    runtime::tools::{
        CompleteTodoTool, CreateTodoTool, DeleteTodoTool, EditTodoTool, GetTodoTool,
        KnowledgeSearchTool, ListTodoTool, ManageRecurringReminderTool, MergeTodoTool,
        RestoreTodoTool, RssManageSubscriptionsTool, RssRecentItemsTool, SaveMemoryTool, TaskStore,
        TodoScopedToolInputs, ToolTurnPostprocess, TrainScheduleTool, WEB_SEARCH_TOOL_NAME,
        WeatherTool, WebSearchTimeouts, WebSearchTool, postprocess_tool_turn,
        replace_scoped_todo_tools_from_visible_snapshot, todo,
    },
    storage::notification::NotificationOutboxStore,
};
use qq_maid_llm::tool::{DEFAULT_TOOL_TIMEOUT, ToolRegistry};

use super::{
    RespondExecutors, RespondRequest, RespondStores, agent_route::AgentToolMode,
    llm_service::RespondOutput,
};

#[derive(Clone)]
pub(crate) struct ToolRuntime {
    registry: ToolRegistry,
    task_store: TaskStore,
    session_store: SessionStore,
    notification_store: NotificationOutboxStore,
    save_memory_tool: SaveMemoryTool,
    web_search_tool: WebSearchTool,
    weather_available: bool,
}

impl ToolRuntime {
    pub(super) fn new(
        executors: &RespondExecutors,
        stores: &RespondStores,
        rss_fetcher: RssFetcher,
        rss_summary_max_chars: usize,
        rss_seen_retention: usize,
        tool_result_max_chars: usize,
        web_search_timeouts: WebSearchTimeouts,
        knowledge_index: crate::runtime::tools::knowledge::KnowledgeIndex,
    ) -> Self {
        let mut registry =
            ToolRegistry::new().with_limits(DEFAULT_TOOL_TIMEOUT, tool_result_max_chars);
        let save_memory_tool =
            SaveMemoryTool::new(stores.memory_store.clone(), stores.session_store.clone());
        let web_search_tool =
            WebSearchTool::new(executors.query_executor.clone()).with_timeouts(web_search_timeouts);
        let weather_available = executors.weather_executor.is_available();
        // Tool 只通过服务端白名单注册；Todo Tool 复用现有 store、session 快照和 pending。
        let mut tools: Vec<qq_maid_llm::tool::DynTool> = vec![
            Arc::new(TrainScheduleTool::new(executors.train_executor.clone())),
            Arc::new(RssRecentItemsTool::new(stores.rss_store.clone())),
            Arc::new(RssManageSubscriptionsTool::new(
                stores.rss_store.clone(),
                rss_fetcher,
                rss_summary_max_chars,
                rss_seen_retention,
            )),
            Arc::new(web_search_tool.clone()),
            Arc::new(KnowledgeSearchTool::new(
                knowledge_index,
                tool_result_max_chars,
            )),
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
            Arc::new(save_memory_tool.clone()),
        ];
        if weather_available {
            tools.push(Arc::new(WeatherTool::new(
                executors.weather_executor.clone(),
            )));
        }
        for tool in tools {
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
            save_memory_tool,
            web_search_tool,
            weather_available,
        }
    }

    /// 按聊天场景裁剪模型可见工具。
    ///
    /// 完整 Agent 使用场景配置白名单；群聊默认关闭完整 Tool Loop 时只构造
    /// `save_memory` 子集，由 Luna 根据 Tool 描述决定是否调用。
    pub(super) fn registry_for_chat(
        &self,
        policy: &ResolvedAgentPolicy,
        req: &RespondRequest,
        mode: AgentToolMode,
    ) -> Result<(ToolRegistry, Vec<String>), LlmError> {
        let user_text = req.effective_user_text();
        let mut tool_names = match mode {
            AgentToolMode::ConfiguredWhitelist => {
                todo::tool_policy::enabled_tool_names_for_request(&policy.enabled_tools, &user_text)
            }
            AgentToolMode::MemoryOnly => policy
                .enabled_tools
                .iter()
                .map(String::as_str)
                .filter(|name| *name == crate::runtime::tools::memory::SAVE_MEMORY_TOOL_NAME)
                .collect(),
        };
        if !self.weather_available {
            tool_names.retain(|name| *name != WEATHER_TOOL_NAME);
        }
        let mut registry = self.registry.subset(&tool_names)?;
        if tool_names.contains(&WEB_SEARCH_TOOL_NAME) {
            // 自然语言搜索与 `/查` 必须复用同一份请求级场景策略；模型参数中即使
            // 伪造 model_override，也会被这个服务端实例覆盖。
            registry.replace(Arc::new(
                self.web_search_tool
                    .clone()
                    .with_model_override(policy.search_model.clone()),
            ))?;
        }
        // subset、请求级 scoped 替换和 diagnostics 必须共享同一份过滤结果，
        // 避免 scoped 阶段重新尝试替换本轮已禁止暴露的工具。
        self.replace_scoped_tools_from_request(&mut registry, &tool_names, req)?;
        let exposed_tools = tool_names.into_iter().map(str::to_owned).collect();
        Ok((registry, exposed_tools))
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
        context: crate::runtime::tools::agent_turn::ToolTurnContext,
    ) -> Result<ToolTurnPostprocess, LlmError> {
        postprocess_tool_turn(
            &self.session_store,
            &self.task_store,
            conversation_session,
            meta,
            interaction_meta,
            output,
            context,
        )
    }

    pub(crate) fn recover_output_after_agent_failure(
        &self,
        err: &LlmError,
        model: &str,
    ) -> Option<RespondOutput> {
        todo::agent_turn::fallback_output_after_agent_failure(err, model)
    }

    fn replace_scoped_tools_from_request(
        &self,
        registry: &mut ToolRegistry,
        enabled_tools: &[&str],
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
        })?;
        if enabled_tools.contains(&crate::runtime::tools::memory::SAVE_MEMORY_TOOL_NAME) {
            registry.replace(Arc::new(
                self.save_memory_tool
                    .scoped_for_request(req.effective_user_text()),
            ))?;
        }
        Ok(())
    }
}

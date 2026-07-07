//! Respond Tool Runtime。
//!
//! 负责构造服务端白名单 ToolRegistry，并按聊天场景裁剪模型可见工具。
//! Tool 是否成功仍以真实工具执行结果为准，本模块不生成业务成功文案。

use std::sync::Arc;

use crate::{
    config::ResolvedAgentPolicy,
    error::LlmError,
    runtime::tools::{
        CancelTodoTool, CompleteTodoTool, CreateTodoTool, DeleteTodoTool, EditTodoTool,
        GetTodoTool, ListTodoTool, MergeTodoTool, RestoreTodoTool, RssRecentItemsTool,
        SelectionScope, TrainScheduleTool, WeatherTool, WebSearchTool,
    },
    runtime::{session::SessionStore, todo::TodoStore},
    storage::notification::NotificationOutboxStore,
};
use qq_maid_llm::tool::{DEFAULT_TOOL_TIMEOUT, ToolRegistry};

use super::{RespondExecutors, RespondStores};

#[derive(Clone)]
pub(super) struct ToolRuntime {
    registry: ToolRegistry,
    todo_store: TodoStore,
    session_store: SessionStore,
    notification_store: NotificationOutboxStore,
}

impl ToolRuntime {
    pub(super) fn new(
        executors: &RespondExecutors,
        stores: &RespondStores,
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
            Arc::new(WebSearchTool::new(executors.query_executor.clone())),
            Arc::new(ListTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
            )),
            Arc::new(GetTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
            )),
            Arc::new(CreateTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(CompleteTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(EditTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(CancelTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(RestoreTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(DeleteTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            Arc::new(MergeTodoTool::new(
                stores.todo_store.clone(),
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
            todo_store: stores.todo_store.clone(),
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
        todo_selection_scope: Option<SelectionScope>,
    ) -> Result<ToolRegistry, LlmError> {
        let tool_names = policy
            .enabled_tools
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let mut registry = self.registry.subset(&tool_names)?;
        if let Some(scope) = todo_selection_scope {
            self.replace_scoped_todo_tools(&mut registry, &policy.enabled_tools, scope)?;
        }
        Ok(registry)
    }

    pub(super) fn registry_for_tool_name(&self, tool_name: &str) -> Result<ToolRegistry, LlmError> {
        self.registry.subset(&[tool_name])
    }

    fn replace_scoped_todo_tools(
        &self,
        registry: &mut ToolRegistry,
        enabled_tools: &[String],
        scope: SelectionScope,
    ) -> Result<(), LlmError> {
        let enabled = |name: &str| enabled_tools.iter().any(|tool| tool == name);
        if enabled("get_todo") {
            registry.replace(Arc::new(
                GetTodoTool::new(self.todo_store.clone(), self.session_store.clone())
                    .with_selection_scope(scope.clone()),
            ))?;
        }
        if enabled("complete_todos") {
            registry.replace(Arc::new(
                CompleteTodoTool::new(
                    self.todo_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope.clone()),
            ))?;
        }
        if enabled("edit_todo") {
            registry.replace(Arc::new(
                EditTodoTool::new(
                    self.todo_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope.clone()),
            ))?;
        }
        if enabled("cancel_todo") {
            registry.replace(Arc::new(
                CancelTodoTool::new(
                    self.todo_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope.clone()),
            ))?;
        }
        if enabled("restore_todos") {
            registry.replace(Arc::new(
                RestoreTodoTool::new(
                    self.todo_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope.clone()),
            ))?;
        }
        if enabled("delete_todos") {
            registry.replace(Arc::new(
                DeleteTodoTool::new(
                    self.todo_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope.clone()),
            ))?;
        }
        if enabled("merge_todos") {
            registry.replace(Arc::new(
                MergeTodoTool::new(
                    self.todo_store.clone(),
                    self.session_store.clone(),
                    self.notification_store.clone(),
                )
                .with_selection_scope(scope),
            ))?;
        }
        Ok(())
    }
}

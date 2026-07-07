//! 请求响应路由与分派。
//!
//! 本模块是 LLM 响应的入口层，负责接收外部（HTTP facade 或内部子 flow）
//! 发来的 `RespondRequest`，根据请求类型和会话状态将其分派到对应的子处理
//! 模块（聊天、翻译、待办、记忆、天气、搜索、会话管理），最终返回 `RespondResponse`。

use std::{future::Future, pin::Pin};

use crate::{
    config::AgentRuntimeConfig,
    error::LlmError,
    provider::DynLlmProvider,
    runtime::{
        display_name::DisplayNameStore,
        knowledge::KnowledgeIndex,
        memory::MemoryStore,
        prompt::PromptConfig,
        query::DynQueryExecutor,
        rss::{RssFetcher, RssStore},
        session::SessionStore,
        todo::TodoStore,
        tools::DynRadarExecutor,
        train::DynTrainExecutor,
        translation::TranslationService,
        weather::DynWeatherExecutor,
    },
    storage::notification::NotificationOutboxStore,
};
use qq_maid_llm::context_budget::ContextBudgetConfig;

mod types;
pub use types::{ChatResponse, RespondPurpose, RespondRequest, RespondResponse};

mod agent_outcome;
mod chat_flow;
mod command_dispatcher;
mod command_render;
mod common;
mod conversation_session;
mod help;
mod interaction_state;
mod llm_service;
/// Markdown 剥离工具，实现在 `qq-maid-common::markdown_strip`；
/// 这里仅保留兼容入口，避免内部 flow 与测试大面积改 import。
mod markdown_strip;
mod memory_flow;
mod pending;
mod radar_flow;
mod router;
mod rss_flow;
mod search_flow;
mod session_flow;
mod set_flow;
mod status_hint;
#[cfg(test)]
mod tests;
mod title;
mod todo_flow;
mod tool_presenters;
mod tool_projection;
mod tool_route;
mod tool_runtime;
mod train_flow;
mod translation_flow;
mod weather_flow;

use command_dispatcher::{CommandDispatcher, DispatchOutcome};
use common::session_error;
use interaction_state::{
    apply_manual_display_names, command_bypasses_pending, respond_interaction_meta, respond_meta,
    session_pending_visible_to_user,
};
pub(crate) use status_hint::{StatusAudience, StatusHint, StatusPhase, status_hint_text};

/// `RustRespondService` 需要的持久化存储集合。
///
/// 这些 store 生命周期一致，收拢后可减少构造函数参数，同时不改变各业务 flow 的边界。
#[derive(Clone)]
pub struct RespondStores {
    /// 长期记忆存储
    pub memory_store: MemoryStore,
    /// 会话记录存储
    pub session_store: SessionStore,
    /// 待办事项存储
    pub todo_store: TodoStore,
    /// 统一通知 Outbox 存储
    pub notification_store: NotificationOutboxStore,
    /// RSS 订阅存储
    pub rss_store: RssStore,
    /// 手动展示名存储，用于本地昵称兜底（#326）。
    pub display_name_store: DisplayNameStore,
}

/// 响应服务外部执行器集合。
///
/// 将查询、天气、列车等执行器收拢为一个参数对象，
/// 减少 `RustRespondService::new` 的构造函数参数数量。
#[derive(Clone)]
pub struct RespondExecutors {
    /// 联网查询执行器
    pub query_executor: DynQueryExecutor,
    /// 天气查询执行器
    pub weather_executor: DynWeatherExecutor,
    /// 列车时刻查询执行器
    pub train_executor: DynTrainExecutor,
    /// 外部雷达公开数据读取执行器
    pub radar_executor: DynRadarExecutor,
}

/// `RustRespondService` 的可选模型和输出配置。
#[derive(Clone)]
pub struct RespondServiceOptions {
    /// 标题生成专用模型（可选）
    pub title_model: Option<String>,
    /// 待办解析专用模型（可选）
    #[allow(dead_code)]
    pub todo_model: Option<String>,
    /// 记忆草稿专用模型（可选）
    pub memory_model: Option<String>,
    /// 上下文压缩专用模型（可选）
    pub compact_model: Option<String>,
    /// 翻译专用模型（可选）；未配置时沿用主 provider 模型。
    pub translation_model: Option<String>,
    /// RSS 摘要最大字符数
    pub rss_summary_max_chars: usize,
    /// RSS 去重记录保留数量
    pub rss_seen_retention: usize,
    /// 是否启用普通聊天的原生 Tool Calling 总开关。
    #[allow(dead_code)]
    pub tool_calling_enabled: bool,
    /// 是否允许群聊普通聊天进入 Tool Calling；默认关闭，避免工具调用阻塞群聊。
    pub tool_calling_group_enabled: bool,
    /// 单次 Tool Loop 最大工具调用轮数。
    pub tool_calling_max_rounds: usize,
    /// 聊天上下文预算；只由 Core 装配层读取配置后注入。
    pub context_budget: ContextBudgetConfig,
    /// 单项 Tool 输出最大字符数，单独注入 ToolRegistry，不混入上下文预算。
    pub tool_result_max_chars: usize,
    /// 私聊状态提示使用的前台称呼。
    pub status_display_name: String,
    /// 统一 Agent 场景策略。
    pub agent_config: AgentRuntimeConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RespondPlan {
    Immediate,
    StreamingChat,
    CompleteToolLoop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatToolPlan {
    Plain,
    ForceCompleteToolLoop,
}

/// Rust 原生实现的响应服务。
///
/// 聚合所有外部依赖（LLM Provider、会话存储、记忆存储、待办存储等），
/// 提供统一的 `respond` 入口点，将请求按业务语义分派到各子处理模块。
#[derive(Clone)]
pub struct RustRespondService {
    /// LLM 提供商（支持流式 / 非流式聊天）
    provider: DynLlmProvider,
    /// 联网查询执行器
    query_executor: DynQueryExecutor,
    /// 天气查询执行器
    weather_executor: DynWeatherExecutor,
    /// 列车时刻查询执行器
    train_executor: DynTrainExecutor,
    /// Codex / Claude Code Radar 公开数据读取执行器
    radar_executor: DynRadarExecutor,
    /// 长期记忆存储
    memory_store: MemoryStore,
    /// 会话记录存储
    session_store: SessionStore,
    /// 待办事项存储
    todo_store: TodoStore,
    /// 统一通知 Outbox 存储
    notification_store: NotificationOutboxStore,
    /// RSS 订阅存储
    rss_store: RssStore,
    /// 手动展示名存储，用于本地昵称兜底（#326）。
    display_name_store: DisplayNameStore,
    /// RSS / Atom 拉取解析器
    rss_fetcher: RssFetcher,
    /// 本地 Markdown 知识检索索引
    knowledge_index: KnowledgeIndex,
    /// 共享翻译执行器；命令和 RSS 共用同一套 provider 调用逻辑。
    translation_service: TranslationService,
    /// 模型原生 Tool Calling 运行时；只注册受控的 Core 业务 Tool。
    tool_runtime: tool_runtime::ToolRuntime,
    /// 系统提示词配置
    prompt_config: PromptConfig,
    /// 标题自动生成专用模型名（若指定则覆盖默认模型）
    title_model: Option<String>,
    /// 记忆草稿专用模型名
    memory_model: Option<String>,
    /// 会话上下文压缩专用模型名
    compact_model: Option<String>,
    /// RSS 摘要最大字符数
    rss_summary_max_chars: usize,
    /// 每个订阅保留的去重指纹数量
    rss_seen_retention: usize,
    /// 统一 Agent 场景策略。
    agent_config: AgentRuntimeConfig,
    /// 聊天上下文预算。
    context_budget: ContextBudgetConfig,
    /// 私聊状态提示使用的前台称呼。
    status_display_name: String,
}

impl RustRespondService {
    /// 构造 `RustRespondService`。
    ///
    /// 所有依赖均为必需注入，不存在默认值或 fallback 构造。
    pub fn new(
        provider: DynLlmProvider,
        executors: RespondExecutors,
        stores: RespondStores,
        rss_fetcher: RssFetcher,
        knowledge_index: KnowledgeIndex,
        prompt_config: PromptConfig,
        options: RespondServiceOptions,
    ) -> Self {
        let translation_service =
            TranslationService::new(provider.clone(), options.translation_model);
        let tool_runtime =
            tool_runtime::ToolRuntime::new(&executors, &stores, options.tool_result_max_chars);
        Self {
            provider,
            query_executor: executors.query_executor,
            weather_executor: executors.weather_executor,
            train_executor: executors.train_executor,
            radar_executor: executors.radar_executor,
            memory_store: stores.memory_store,
            session_store: stores.session_store,
            todo_store: stores.todo_store,
            notification_store: stores.notification_store,
            rss_store: stores.rss_store,
            display_name_store: stores.display_name_store,
            rss_fetcher,
            knowledge_index,
            translation_service,
            tool_runtime,
            prompt_config,
            title_model: options.title_model,
            memory_model: options.memory_model,
            compact_model: options.compact_model,
            rss_summary_max_chars: options.rss_summary_max_chars,
            rss_seen_retention: options.rss_seen_retention,
            agent_config: options.agent_config,
            context_budget: options.context_budget,
            status_display_name: options.status_display_name,
        }
    }

    pub(crate) fn status_display_name(&self) -> &str {
        &self.status_display_name
    }

    /// 统一的请求响应入口。
    ///
    /// 分派顺序：
    /// 1. 检查会话中是否有**待处理操作**（pending operation），若有则优先处理。
    /// 2. 解析是否为**会话管理指令**（`/new`, `/clear`, `/state` 等）。
    /// 3. 获取或创建活跃会话。
    /// 4. 检查是否为**翻译命令**。
    /// 5. 检查是否为**天气查询命令**。
    /// 6. 检查是否为**联网搜索命令**。
    /// 7. 检查是否为**待办相关操作**。
    /// 8. 检查是否为**长期记忆操作**。
    /// 9. 兜底：进入**普通聊天**处理流程。
    pub async fn respond(&self, req: RespondRequest) -> Result<RespondResponse, LlmError> {
        let plan = self.plan_core_respond(&req)?;
        self.respond_with_plan(req, plan).await
    }

    pub(crate) async fn respond_with_plan(
        &self,
        req: RespondRequest,
        plan: RespondPlan,
    ) -> Result<RespondResponse, LlmError> {
        match CommandDispatcher::new(self).dispatch(req, plan).await? {
            DispatchOutcome::Respond(response) => Ok(*response),
            DispatchOutcome::Chat(chat) => {
                self.handle_chat(
                    chat.req,
                    chat.user_text,
                    chat.meta,
                    chat.session,
                    chat.chat_plan,
                )
                .await
            }
        }
    }

    /// 仅供 Core 进程内 stream 边界使用的真流式入口。
    ///
    /// 本阶段只接通 `/查` 和普通聊天；短命令仍走完整响应路径，避免改变用户可见语义。
    pub async fn respond_stream<F>(
        &self,
        mut req: RespondRequest,
        on_delta: F,
    ) -> Result<RespondResponse, LlmError>
    where
        F: FnMut(String) -> Pin<Box<dyn Future<Output = Result<(), LlmError>> + Send>> + Send,
    {
        let user_text = req.effective_user_text();
        let meta = respond_meta(&req);
        let interaction_meta = respond_interaction_meta(&req);
        let mut active_interaction_session = self
            .session_store
            .get_active(&interaction_meta)
            .map_err(session_error)?;
        let mut active_session = self
            .session_store
            .get_active(&meta)
            .map_err(session_error)?;

        let bypass_pending_for_session_command = command_bypasses_pending(&user_text);
        if !bypass_pending_for_session_command {
            if let Some(session) = active_interaction_session
                .as_mut()
                .filter(|session| session.pending_operation.is_some())
                && let Some(response) = self
                    .handle_pending_operation(&req, &user_text, &meta, session)
                    .await?
            {
                return Ok(response);
            }
            if let Some(session) = active_session
                .as_mut()
                .filter(|session| session_pending_visible_to_user(session, meta.user_id.as_deref()))
                && let Some(response) = self
                    .handle_pending_operation(&req, &user_text, &meta, session)
                    .await?
            {
                return Ok(response);
            }
        }

        if let Some(command) = session_flow::parse_session_command(&user_text) {
            return self.handle_session_command(command, &meta).await;
        }

        let mut session = match active_session {
            Some(session) => session,
            None => self
                .session_store
                .get_or_create_active(&meta)
                .map_err(session_error)?,
        };

        if let Some(command) = search_flow::parse_web_search_command(&user_text) {
            return self
                .handle_web_search_command_stream(command, &req, &mut session, on_delta)
                .await;
        }

        // 真流式只覆盖普通聊天；搜索命令等确定性入口不提前查手动展示名。
        apply_manual_display_names(&self.display_name_store, &meta, &mut req);
        self.handle_chat_stream(req, on_delta).await
    }
}

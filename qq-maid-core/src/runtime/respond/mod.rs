//! 请求响应路由与分派。
//!
//! 本模块是 LLM 响应的入口层，负责接收外部（HTTP facade 或内部子 flow）
//! 发来的 `RespondRequest`，根据请求类型和会话状态将其分派到对应的子处理
//! 模块（聊天、翻译、待办、记忆、天气、搜索、会话管理），最终返回 `RespondResponse`。

use std::{future::Future, pin::Pin, time::Duration};

use qq_maid_llm::{
    agent_loop::ToolLoopProgressSink, context_budget::ContextBudgetConfig,
    provider::DynLlmProvider, web_search::DynWebSearchExecutor,
};

use crate::{
    config::AgentRuntimeConfig,
    error::LlmError,
    runtime::{
        display_name::DisplayNameStore,
        knowledge::KnowledgeIndex,
        prompt::PromptConfig,
        session::SessionStore,
        tools::{
            DynRadarExecutor, TaskStore,
            memory::MemoryStore,
            rss::{RssFetcher, RssStore},
            train::DynTrainExecutor,
            weather::DynWeatherExecutor,
        },
        translation::TranslationService,
    },
    storage::notification::NotificationOutboxStore,
};

mod types;
pub use types::{ChatResponse, RespondPurpose, RespondRequest, RespondResponse};

pub(crate) mod agent_outcome;
mod agent_route;
mod chat_flow;
mod command_dispatcher;
pub(crate) mod command_render;
pub(crate) mod common;
mod conversation_session;
mod help;
mod interaction_state;
pub(crate) mod llm_service;
mod memory_flow;
mod pending;
mod pending_dispatch;
mod radar_flow;
mod router;
mod rss_flow;
pub(crate) mod search_flow;
mod session_flow;
mod set_flow;
#[cfg(test)]
pub(crate) mod tests;
mod title;
mod tool_runtime;
pub(crate) mod train_flow;
mod translation_flow;
pub(crate) mod weather_flow;

pub(crate) use crate::runtime::tools::{StatusAudience, StatusHint, StatusPhase, status_hint_text};
use chat_flow::ChatFlowSinks;
use command_dispatcher::{CommandDispatcher, DispatchOutcome};
use common::session_error;
use interaction_state::{
    apply_manual_display_names, command_bypasses_pending, group_session_turn_actor,
    respond_interaction_meta, respond_meta, session_pending_visible_to_user,
};

/// `RustRespondService` 需要的持久化存储集合。
///
/// 这些 store 生命周期一致，收拢后可减少构造函数参数，同时不改变各业务 flow 的边界。
#[derive(Clone)]
pub struct RespondStores {
    /// 长期记忆存储
    pub memory_store: MemoryStore,
    /// 会话记录存储
    pub session_store: SessionStore,
    /// 任务存储；当前实现由 Todo 业务模块提供。
    pub task_store: TaskStore,
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
    pub query_executor: DynWebSearchExecutor,
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
    /// 标题生成专用模型（可选）；配置后覆盖场景 Agent 辅助路线。
    pub title_model: Option<String>,
    /// 记忆草稿专用显式覆盖模型（可选）
    pub memory_model: Option<String>,
    /// 上下文压缩专用显式覆盖模型（可选）
    pub compact_model: Option<String>,
    /// 翻译专用显式覆盖模型（可选）。
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
    /// 单次 Agent 最大工具调用轮数。
    pub tool_calling_max_rounds: usize,
    /// 聊天上下文预算；只由 Core 装配层读取配置后注入。
    pub context_budget: ContextBudgetConfig,
    /// 单项 Tool 输出最大字符数，单独注入 ToolRegistry，不混入上下文预算。
    pub tool_result_max_chars: usize,
    /// Agent `web_search` 等待首个非空搜索增量的超时。
    pub web_search_first_activity_timeout: Duration,
    /// 程序生成的用户可见文案所使用的机器人主称呼。
    pub bot_display_name: String,
    /// 统一 Agent 场景策略。
    pub agent_config: AgentRuntimeConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RespondPlan {
    Immediate,
    /// 确定性短命令的事件包装试点。
    ///
    /// 只用于已显式放行的 slash command，命令执行仍走原有确定性分派；
    /// 这里仅让 Core -> Gateway 边界统一输出 Status / Completed / Failed。
    CommandEvent,
    StreamingChat,
    AgentRuntime,
    /// 显式联网查询路径，仅用于 `/查`、`/查询`、`/search` 等专用入口。
    ///
    /// 普通自然语言搜索请求进入 AgentRuntime，由模型结合上下文决定是否调用
    /// `web_search`；显式命令继续复用原有流式查询和错误处理能力。
    WebSearch,
}

/// Router 对单个请求生成的不可变执行决策。
///
/// `RespondPlan` 只负责 Core/Gateway 输出策略；普通聊天是否进入 Tool Agent
/// 必须始终读取同一份 `respond_route`，执行阶段不得重新读取 session 或 policy 计算。
#[derive(Debug, Clone, Copy)]
pub(crate) struct PlannedRespond {
    plan: RespondPlan,
    respond_route: Option<agent_route::AgentRouteDecision>,
    status_hint: Option<StatusHint>,
}

impl PlannedRespond {
    const fn web_search() -> Self {
        Self {
            plan: RespondPlan::WebSearch,
            respond_route: None,
            status_hint: None,
        }
    }

    pub(crate) const fn command_event() -> Self {
        Self {
            plan: RespondPlan::CommandEvent,
            respond_route: Some(agent_route::AgentRouteDecision::plain_deterministic(
                "command_event_fallback",
            )),
            status_hint: None,
        }
    }

    fn chat(
        respond_route: agent_route::AgentRouteDecision,
        status_hint: Option<StatusHint>,
    ) -> Self {
        let plan = if respond_route.uses_agent_runtime() {
            RespondPlan::AgentRuntime
        } else {
            RespondPlan::StreamingChat
        };
        Self {
            plan,
            respond_route: Some(respond_route),
            status_hint,
        }
    }

    const fn immediate_chat(reason: &'static str) -> Self {
        Self {
            plan: RespondPlan::Immediate,
            respond_route: Some(agent_route::AgentRouteDecision::plain_deterministic(reason)),
            status_hint: None,
        }
    }

    pub(crate) const fn plan(self) -> RespondPlan {
        self.plan
    }

    const fn respond_route(self) -> Option<agent_route::AgentRouteDecision> {
        self.respond_route
    }

    pub(crate) fn status_hint(self) -> StatusHint {
        if !matches!(self.plan, RespondPlan::AgentRuntime) {
            return StatusHint::model();
        }
        self.status_hint.unwrap_or_else(StatusHint::model)
    }

    /// 只有路由层识别出明确工具状态提示时才提前展示 Agent 进度。
    /// 普通聊天仍进入 AgentRuntime，但应等真实工具事件后再展示处理状态。
    pub(crate) fn should_emit_eager_agent_status(self) -> bool {
        matches!(self.plan, RespondPlan::AgentRuntime) && self.status_hint.is_some()
    }

    const fn classified_status_hint(self) -> Option<StatusHint> {
        self.status_hint
    }
}

impl PartialEq<RespondPlan> for PlannedRespond {
    fn eq(&self, other: &RespondPlan) -> bool {
        self.plan == *other
    }
}

impl PartialEq<PlannedRespond> for RespondPlan {
    fn eq(&self, other: &PlannedRespond) -> bool {
        *self == other.plan
    }
}

/// Rust 原生实现的响应服务。
///
/// 聚合所有外部依赖（LLM Provider、会话存储、记忆存储、待办存储等），
/// 提供统一的 `respond` 入口点，将请求按业务语义分派到各子处理模块。
#[derive(Clone)]
pub struct RustRespondService {
    /// LLM 提供商（支持流式 / 非流式聊天）
    pub(crate) provider: DynLlmProvider,
    /// 联网查询执行器
    query_executor: DynWebSearchExecutor,
    /// 天气查询执行器
    weather_executor: DynWeatherExecutor,
    /// 列车时刻查询执行器
    train_executor: DynTrainExecutor,
    /// Codex / Claude Code Radar 公开数据读取执行器
    radar_executor: DynRadarExecutor,
    /// 长期记忆存储
    pub(crate) memory_store: MemoryStore,
    /// 会话记录存储
    pub(crate) session_store: SessionStore,
    /// 任务存储；当前实现由 Todo 业务模块提供。
    pub(crate) task_store: TaskStore,
    /// 统一通知 Outbox 存储
    pub(crate) notification_store: NotificationOutboxStore,
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
    pub(crate) tool_runtime: tool_runtime::ToolRuntime,
    /// 系统提示词配置
    prompt_config: PromptConfig,
    /// 标题自动生成专用模型名（若指定则覆盖场景 Agent 辅助路线）
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
    pub(crate) agent_config: AgentRuntimeConfig,
    /// 聊天上下文预算。
    context_budget: ContextBudgetConfig,
    /// 程序生成的用户可见文案所使用的机器人主称呼。
    bot_display_name: String,
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
            TranslationService::new(provider.clone(), options.translation_model)
                .with_agent_config(options.agent_config.clone());
        let tool_runtime = tool_runtime::ToolRuntime::new(
            &executors,
            &stores,
            rss_fetcher.clone(),
            options.rss_summary_max_chars,
            options.rss_seen_retention,
            options.tool_result_max_chars,
            options.web_search_first_activity_timeout,
        );
        Self {
            provider,
            query_executor: executors.query_executor,
            weather_executor: executors.weather_executor,
            train_executor: executors.train_executor,
            radar_executor: executors.radar_executor,
            memory_store: stores.memory_store,
            session_store: stores.session_store,
            task_store: stores.task_store,
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
            bot_display_name: options.bot_display_name,
        }
    }

    pub(crate) fn bot_display_name(&self) -> &str {
        &self.bot_display_name
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
        let planned = self.plan_core_respond(&req)?;
        self.respond_with_plan(req, planned).await
    }

    pub(crate) async fn respond_with_plan(
        &self,
        req: RespondRequest,
        planned: PlannedRespond,
    ) -> Result<RespondResponse, LlmError> {
        self.respond_with_plan_and_progress(req, planned, None, None, None)
            .await
    }

    pub(crate) async fn respond_with_plan_and_progress(
        &self,
        req: RespondRequest,
        planned: PlannedRespond,
        progress_sink: Option<ToolLoopProgressSink>,
        final_delta_sink: Option<qq_maid_llm::agent_loop::AgentTextDeltaSink>,
        run_handle: Option<qq_maid_llm::agent_loop::AgentRunHandle>,
    ) -> Result<RespondResponse, LlmError> {
        match CommandDispatcher::new(self).dispatch(req, planned).await? {
            DispatchOutcome::Respond(response) => Ok(*response),
            DispatchOutcome::Chat(chat) => {
                self.handle_chat(
                    *chat,
                    ChatFlowSinks {
                        progress_sink,
                        final_delta_sink,
                        run_handle,
                    },
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
        req: RespondRequest,
        on_delta: F,
    ) -> Result<RespondResponse, LlmError>
    where
        F: FnMut(String) -> Pin<Box<dyn Future<Output = Result<(), LlmError>> + Send>> + Send,
    {
        let planned = self.plan_core_respond(&req)?;
        if matches!(planned.plan(), RespondPlan::AgentRuntime) {
            // 直接调用方也必须遵守 Router 已确定的 Tool Agent 执行路径，不能把
            // 同一 decision 交给普通 stream_respond() 后生成错误 diagnostics。
            return self.respond_with_plan(req, planned).await;
        }
        if matches!(planned.plan(), RespondPlan::WebSearch) {
            return self.respond_web_search_stream(req, on_delta).await;
        }
        self.respond_stream_with_plan(req, planned, on_delta).await
    }

    pub(crate) async fn respond_stream_with_plan<F>(
        &self,
        mut req: RespondRequest,
        planned: PlannedRespond,
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
        let turn_actor = group_session_turn_actor(&self.display_name_store, &meta, &req);
        if let Some(session) = active_interaction_session.as_mut() {
            session.set_turn_actor(turn_actor.clone());
        }
        if let Some(session) = active_session.as_mut() {
            session.set_turn_actor(turn_actor.clone());
        }

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
        session.set_turn_actor(turn_actor);

        if let Some(command) = search_flow::parse_web_search_command(&user_text) {
            return self
                .handle_web_search_command_stream(command, &req, &mut session, on_delta)
                .await;
        }

        // 真流式只覆盖普通聊天；搜索命令只保存 actor 快照，不额外注入当前身份 prompt。
        apply_manual_display_names(&self.display_name_store, &meta, &mut req);
        let respond_route = planned.respond_route().ok_or_else(|| {
            LlmError::new(
                "respond_route_missing",
                "chat execution requires the router decision",
                "router",
            )
        })?;
        self.handle_chat_stream(req, respond_route, on_delta).await
    }

    /// `RespondPlan::WebSearch` 的流式编放入口。
    ///
    /// 当显式 `/查` 等命令被 router 判定为 WebSearch 时调用，复用
    /// `handle_web_search_command_stream` 的查询、流式输出和错误处理。会话加载与
    /// pending 优先级沿用 `respond_stream`，不重复各自重建状态机。
    pub(crate) async fn respond_web_search_stream<F>(
        &self,
        req: RespondRequest,
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
        let turn_actor = group_session_turn_actor(&self.display_name_store, &meta, &req);
        if let Some(session) = active_interaction_session.as_mut() {
            session.set_turn_actor(turn_actor.clone());
        }
        if let Some(session) = active_session.as_mut() {
            session.set_turn_actor(turn_actor.clone());
        }

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
        session.set_turn_actor(turn_actor);

        let command = search_flow::parse_web_search_command(&user_text).ok_or_else(|| {
            LlmError::new(
                "web_search_command_missing",
                "web search plan requires an explicit search command",
                "router",
            )
        })?;
        self.handle_web_search_command_stream(command, &req, &mut session, on_delta)
            .await
    }
}

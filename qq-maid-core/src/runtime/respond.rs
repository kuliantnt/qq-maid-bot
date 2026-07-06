//! 请求响应路由与分派。
//!
//! 本模块是 LLM 响应的入口层，负责接收外部（HTTP facade 或内部子 flow）
//! 发来的 `RespondRequest`，根据请求类型和会话状态将其分派到对应的子处理
//! 模块（聊天、翻译、待办、记忆、天气、搜索、会话管理），最终返回 `RespondResponse`。

use std::{future::Future, pin::Pin};

use crate::{
    config::{AgentRuntimeConfig, ChatScene, ResolvedAgentPolicy},
    error::LlmError,
    identity::{interaction_scope_key, parse_stable_scope_key},
    provider::DynLlmProvider,
    runtime::{
        knowledge::KnowledgeIndex,
        memory::MemoryStore,
        prompt::PromptConfig,
        query::DynQueryExecutor,
        rss::{RssFetcher, RssStore},
        session::{
            LAST_QUERY_TTL_SECONDS, SessionMeta, SessionRecord, SessionStore, query_is_fresh,
            valid_last_visible_todo_query,
        },
        todo::TodoStore,
        tools::{
            CancelTodoTool, CompleteTodoTool, CreateTodoTool, DeleteTodoTool, DynRadarExecutor,
            EditTodoTool, GetTodoTool, ListTodoTool, RestoreTodoTool, RssRecentItemsTool,
            TrainScheduleTool, WeatherTool,
        },
        train::DynTrainExecutor,
        translation::TranslationService,
        weather::DynWeatherExecutor,
    },
    storage::notification::NotificationOutboxStore,
};
use qq_maid_llm::{
    context_budget::ContextBudgetConfig,
    tool::{DEFAULT_TOOL_TIMEOUT, ToolRegistry},
};

mod types;
use crate::service::{CoreInboundClassification, CoreInboundKind};
pub use types::{ChatResponse, RespondPurpose, RespondRequest, RespondResponse};

mod agent_outcome;
mod chat_flow;
mod command_render;
mod common;
mod help;
mod llm_service;
/// Markdown 剥离工具，实现在 `qq-maid-common::markdown_strip`；
/// 这里仅保留兼容入口，避免内部 flow 与测试大面积改 import。
mod markdown_strip;
mod memory_flow;
mod pending;
mod radar_flow;
mod rss_flow;
mod search_flow;
mod session_flow;
mod status_hint;
#[cfg(test)]
mod tests;
mod title;
mod todo_flow;
mod tool_presenters;
mod tool_route;
mod train_flow;
mod translation_flow;
mod weather_flow;

use common::{clean_string, session_error};
pub(crate) use status_hint::{StatusAudience, StatusHint, StatusPhase, status_hint_text};
use tool_route::{ToolLoopRoute, ToolRouteContext};

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
    /// RSS / Atom 拉取解析器
    rss_fetcher: RssFetcher,
    /// 本地 Markdown 知识检索索引
    knowledge_index: KnowledgeIndex,
    /// 共享翻译执行器；命令和 RSS 共用同一套 provider 调用逻辑。
    translation_service: TranslationService,
    /// 模型原生 Tool Calling 注册表；只注册受控的 Core 业务 Tool。
    tool_registry: ToolRegistry,
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
        let mut tool_registry =
            ToolRegistry::new().with_limits(DEFAULT_TOOL_TIMEOUT, options.tool_result_max_chars);
        // Tool 只通过服务端白名单注册；Todo Tool 复用现有 store、session 快照和 pending。
        for tool in [
            std::sync::Arc::new(WeatherTool::new(executors.weather_executor.clone()))
                as qq_maid_llm::tool::DynTool,
            std::sync::Arc::new(TrainScheduleTool::new(executors.train_executor.clone())),
            std::sync::Arc::new(RssRecentItemsTool::new(stores.rss_store.clone())),
            std::sync::Arc::new(ListTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
            )),
            std::sync::Arc::new(GetTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
            )),
            std::sync::Arc::new(CreateTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            std::sync::Arc::new(CompleteTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            std::sync::Arc::new(EditTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            std::sync::Arc::new(CancelTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            std::sync::Arc::new(RestoreTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
            std::sync::Arc::new(DeleteTodoTool::new(
                stores.todo_store.clone(),
                stores.session_store.clone(),
                stores.notification_store.clone(),
            )),
        ] {
            if let Err(err) = tool_registry.insert(tool) {
                tracing::warn!(
                    error_code = %err.code,
                    error_stage = %err.stage,
                    "failed to register core tool"
                );
            }
        }
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
            rss_fetcher,
            knowledge_index,
            translation_service,
            tool_registry,
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

    pub(crate) fn status_hint_for_plan(
        &self,
        req: &RespondRequest,
        plan: RespondPlan,
    ) -> Result<StatusHint, LlmError> {
        if !matches!(plan, RespondPlan::CompleteToolLoop) {
            return Ok(StatusHint::model());
        }
        let policy = self.resolve_agent_policy(req)?;
        let meta = respond_meta(req);
        let interaction_meta = respond_interaction_meta(req);
        let active_interaction_session = self
            .session_store
            .get_active(&interaction_meta)
            .map_err(session_error)?;
        let active_conversation_session = self
            .session_store
            .get_active(&meta)
            .map_err(session_error)?;
        let route_session = route_context_session(
            req,
            active_interaction_session.as_ref(),
            active_conversation_session.as_ref(),
        );
        Ok(self
            .route_tool_loop_with_active(req, &policy, route_session)
            .status_hint
            .unwrap_or_else(StatusHint::default_tool))
    }

    /// 为响应入口计算本轮响应计划。
    ///
    /// 这里是普通消息是否进入完整 Tool Loop 的唯一决策点。
    /// pending、slash 命令和确定性 Todo 查询仍优先走 `Immediate`；
    /// 工具关闭、provider 不支持工具或群聊工具开关关闭时继续保留原流式路径。
    pub(crate) fn plan_core_respond(&self, req: &RespondRequest) -> Result<RespondPlan, LlmError> {
        let user_text = req.effective_user_text();
        let trimmed = user_text.trim();
        if trimmed.is_empty() && req.effective_input_parts().is_empty() {
            return Ok(RespondPlan::Immediate);
        }

        let meta = respond_meta(req);
        let interaction_meta = respond_interaction_meta(req);
        let active_interaction_session = self
            .session_store
            .get_active(&interaction_meta)
            .map_err(session_error)?;
        let active_conversation_session = self
            .session_store
            .get_active(&meta)
            .map_err(session_error)?;
        let route_session = route_context_session(
            req,
            active_interaction_session.as_ref(),
            active_conversation_session.as_ref(),
        );
        if pending_blocks_immediate(
            &user_text,
            active_interaction_session.as_ref(),
            active_conversation_session.as_ref(),
            meta.user_id.as_deref(),
        ) {
            return Ok(RespondPlan::Immediate);
        }

        if search_flow::parse_web_search_command(&user_text).is_some() {
            return Ok(RespondPlan::StreamingChat);
        }
        if trimmed.starts_with('/') || trimmed.starts_with('／') {
            return Ok(RespondPlan::Immediate);
        }

        // 先保护已有确定性命令和自然语言 Todo 查询，避免简单列表查询绕过
        // `handle_todo_flow()` 进入模型 Tool Loop，回归同义词和默认过滤语义。
        let classification = classify_inbound_with_active(
            &user_text,
            active_interaction_session.as_ref(),
            active_conversation_session.as_ref(),
            meta.user_id.as_deref(),
        );
        if matches!(classification.kind, CoreInboundKind::Immediate) {
            return Ok(RespondPlan::Immediate);
        }

        let policy = self.resolve_agent_policy(req)?;
        let tool_decision = self.route_tool_loop_with_active(req, &policy, route_session);
        let plan = if !req.has_non_text_input_parts()
            && matches!(tool_decision.route, ToolLoopRoute::CompleteToolLoop)
        {
            RespondPlan::CompleteToolLoop
        } else {
            RespondPlan::StreamingChat
        };
        tracing::debug!(
            respond_plan = ?plan,
            tool_loop_route = ?tool_decision.route,
            semantic_route = ?tool_decision.semantic_route,
            tool_domain = ?tool_decision.domain,
            route_reason = tool_decision.reason,
            is_group = req
                .group_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            input_chars = trimmed.chars().count(),
            enabled_tools_count = policy.enabled_tools.len(),
            "selected core respond route"
        );
        if matches!(plan, RespondPlan::CompleteToolLoop) {
            Ok(RespondPlan::CompleteToolLoop)
        } else {
            Ok(RespondPlan::StreamingChat)
        }
    }

    fn route_tool_loop_with_active(
        &self,
        req: &RespondRequest,
        policy: &ResolvedAgentPolicy,
        active_session: Option<&SessionRecord>,
    ) -> tool_route::ToolRouteDecision {
        tool_route::route_tool_loop(
            req,
            ToolRouteContext {
                scene_enabled: policy.enabled,
                tool_calling_enabled: policy.tool_calling_enabled,
                group_tool_calling_enabled: policy.group_tool_calling_enabled,
                provider_supports_tool_calling: self
                    .provider
                    .tool_calling_protocol(Some(&policy.main_model))
                    .is_some(),
                enabled_tools_available: !policy.enabled_tools.is_empty(),
                has_recent_todo_context: has_recent_todo_context(req, active_session),
            },
        )
    }

    pub(crate) fn resolve_agent_policy(
        &self,
        req: &RespondRequest,
    ) -> Result<ResolvedAgentPolicy, LlmError> {
        let scene = if req
            .group_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            ChatScene::Group
        } else {
            ChatScene::Private
        };
        self.agent_config.resolve(scene)
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
        let user_text = req.effective_user_text();
        let meta = respond_meta(&req);
        let interaction_meta = respond_interaction_meta(&req);

        // pending、Todo 可见编号和 Memory 列表序号属于群内个人交互状态；
        // 普通聊天历史仍保留在 conversation session，避免把群聊上下文强制拆成私聊。
        let mut active_interaction_session = self
            .session_store
            .get_active(&interaction_meta)
            .map_err(session_error)?;
        let mut active_session = self
            .session_store
            .get_active(&meta)
            .map_err(session_error)?;

        // 若用户输入不是跳等待办的会话指令，则先检查是否有待处理操作（pending）
        let bypass_pending_for_session_command =
            session_flow::parse_pending_bypass_session_command(&user_text).is_some();
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

        // 检查是否为会话管理指令（/new, /clear, /state 等）
        if let Some(command) = session_flow::parse_session_command(&user_text) {
            return self.handle_session_command(command, &meta).await;
        }

        // 确保存在活跃会话（无则创建）
        let mut session = match active_session {
            Some(session) => session,
            None => self
                .session_store
                .get_or_create_active(&meta)
                .map_err(session_error)?,
        };
        let force_tool_loop = matches!(plan, RespondPlan::CompleteToolLoop);

        // 检查是否为翻译指令（如 "/翻译 文本"、"/翻译日语 文本"）
        if let Some(command) = translation_flow::parse_translation_command(&user_text) {
            return self
                .handle_translation_command(command, &meta, &user_text, &mut session)
                .await;
        }

        // 检查是否为天气查询指令（如 "/北京天气" 或 "/天气北京"）
        if let Some(command) = weather_flow::parse_weather_command(&user_text) {
            return self
                .handle_weather_command(command, &user_text, &mut session)
                .await;
        }

        // 检查是否为列车时刻查询指令（如 "/火车 G1 明天"）
        if let Some(command) = train_flow::parse_train_command(&user_text) {
            return self
                .handle_train_command(command, &user_text, &mut session)
                .await;
        }

        // 检查是否为雷达看板指令（如 "/rader codex" 或 "/雷达"）
        if let Some(command) = radar_flow::parse_radar_command(&user_text) {
            return self
                .handle_radar_command(command, &user_text, &mut session)
                .await;
        }

        // 检查是否为联网搜索指令（如 "/查 关键词"）
        if let Some(command) = search_flow::parse_web_search_command(&user_text) {
            return self
                .handle_web_search_command(command, &req, &mut session)
                .await;
        }

        // 检查是否为 RSS 订阅指令（如 "/rss add ..." 或 "/订阅"）
        if let Some(response) = self
            .handle_rss_flow(&req, &user_text, &meta, &mut session)
            .await?
        {
            return Ok(response);
        }

        // CompleteToolLoop 下由 Agent 自行决定是否调用 Todo Tool；
        // slash 命令、pending 和确定性 Todo 查询已在前面保持原路径。
        if !force_tool_loop {
            // 检查是否为待办相关操作（新增、查看、完成、编辑、删除等）
            if should_try_todo_flow(&user_text) {
                let mut interaction_session = match active_interaction_session.take() {
                    Some(session) => session,
                    None => self
                        .session_store
                        .get_or_create_active(&interaction_meta)
                        .map_err(session_error)?,
                };
                if let Some(response) = self
                    .handle_todo_flow(&user_text, &meta, &mut interaction_session)
                    .await?
                {
                    return Ok(response);
                }
                active_interaction_session = Some(interaction_session);
            }
        }

        // 检查是否为长期记忆相关操作（记忆新增、查看、更新、删除等）
        if !force_tool_loop && memory_flow::parse_memory_command(&user_text).is_some() {
            let mut interaction_session = match active_interaction_session.take() {
                Some(session) => session,
                None => self
                    .session_store
                    .get_or_create_active(&interaction_meta)
                    .map_err(session_error)?,
            };
            if let Some(response) = self
                .handle_memory_flow(&req, &user_text, &meta, &mut interaction_session)
                .await?
            {
                return Ok(response);
            }
        }

        // 兜底：进入普通 LLM 聊天流程
        let chat_plan = match plan {
            RespondPlan::CompleteToolLoop => ChatToolPlan::ForceCompleteToolLoop,
            RespondPlan::Immediate | RespondPlan::StreamingChat => ChatToolPlan::Plain,
        };
        self.handle_chat(req, user_text, meta, session, chat_plan)
            .await
    }

    /// 轻量判断物理入站消息是否可以进入短窗口聚合。
    ///
    /// 这里只复用现有命令解析和真实 pending 状态，不执行命令、不创建会话、
    /// 不调用 LLM，避免 Gateway 为聚合复制一套业务关键词规则。
    pub fn classify_inbound(
        &self,
        req: RespondRequest,
    ) -> Result<CoreInboundClassification, LlmError> {
        let user_text = req.effective_user_text();
        let meta = respond_meta(&req);
        let interaction_meta = respond_interaction_meta(&req);
        let active_interaction_session = self
            .session_store
            .get_active(&interaction_meta)
            .map_err(session_error)?;
        let active_conversation_session = self
            .session_store
            .get_active(&meta)
            .map_err(session_error)?;

        if pending_blocks_immediate(
            &user_text,
            active_interaction_session.as_ref(),
            active_conversation_session.as_ref(),
            meta.user_id.as_deref(),
        ) {
            return Ok(CoreInboundClassification {
                kind: CoreInboundKind::Immediate,
            });
        }

        Ok(classify_inbound_with_active(
            &user_text,
            active_interaction_session.as_ref(),
            active_conversation_session.as_ref(),
            meta.user_id.as_deref(),
        ))
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

        let bypass_pending_for_session_command =
            session_flow::parse_pending_bypass_session_command(&user_text).is_some();
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

        self.handle_chat_stream(req, on_delta).await
    }
}

fn respond_meta(req: &RespondRequest) -> SessionMeta {
    SessionMeta::new_with_account(
        req.scope_key.clone(),
        req.user_id.clone(),
        req.group_id.clone(),
        req.guild_id.clone(),
        req.channel_id.clone(),
        clean_string(req.platform.clone()).unwrap_or_else(|| "qq".to_owned()),
        req.account_id.clone(),
    )
}

fn respond_interaction_meta(req: &RespondRequest) -> SessionMeta {
    let mut meta = respond_meta(req);
    if req
        .group_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && req
            .user_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && parse_stable_scope_key(&req.scope_key).is_some()
    {
        meta.scope_key = interaction_scope_key(req.user_id.as_deref(), &req.scope_key);
    }
    meta
}

fn pending_blocks_immediate(
    user_text: &str,
    active_interaction_session: Option<&SessionRecord>,
    active_conversation_session: Option<&SessionRecord>,
    user_id: Option<&str>,
) -> bool {
    let bypass_pending_for_session_command =
        session_flow::parse_pending_bypass_session_command(user_text).is_some();
    !bypass_pending_for_session_command
        && (active_interaction_session
            .and_then(|session| session.pending_operation.as_ref())
            .is_some()
            || active_conversation_session
                .is_some_and(|session| session_pending_visible_to_user(session, user_id)))
}

fn session_pending_visible_to_user(session: &SessionRecord, user_id: Option<&str>) -> bool {
    let Some(pending) = session.pending_operation.as_ref() else {
        return false;
    };
    match pending.initiator_user_id() {
        Some(initiator) => user_id == Some(initiator),
        None => true,
    }
}

fn should_try_todo_flow(user_text: &str) -> bool {
    todo_flow::parse_todo_command(user_text).is_some()
        || todo_flow::is_natural_todo_query_text(user_text)
        || todo_flow::is_full_todo_result_request(user_text)
}

fn has_recent_todo_context(req: &RespondRequest, active_session: Option<&SessionRecord>) -> bool {
    let Some(session) = active_session else {
        return false;
    };
    let owner = TodoStore::owner(req.user_id.as_deref(), &req.scope_key);

    let mut snapshot = session.clone();
    let has_visible_snapshot = valid_last_visible_todo_query(&mut snapshot, &owner.key)
        .is_some_and(|query| !query.result_ids.is_empty());
    if has_visible_snapshot {
        return true;
    }

    session.last_todo_action.as_ref().is_some_and(|action| {
        action.owner_key == owner.key && query_is_fresh(&action.created_at, LAST_QUERY_TTL_SECONDS)
    })
}

fn route_context_session<'a>(
    req: &RespondRequest,
    active_interaction_session: Option<&'a SessionRecord>,
    active_conversation_session: Option<&'a SessionRecord>,
) -> Option<&'a SessionRecord> {
    // 新 session 状态以 interaction scope 为准；旧 conversation 可见快照只作为路由提示
    // 兼容读取，不迁移、不回写，实际 Todo/Memory 状态仍落在 interaction session。
    if has_recent_todo_context(req, active_interaction_session) {
        return active_interaction_session;
    }
    if has_recent_todo_context(req, active_conversation_session) {
        return active_conversation_session;
    }
    active_interaction_session.or(active_conversation_session)
}

fn classify_inbound_with_active(
    user_text: &str,
    active_interaction_session: Option<&SessionRecord>,
    active_conversation_session: Option<&SessionRecord>,
    user_id: Option<&str>,
) -> CoreInboundClassification {
    if pending_blocks_immediate(
        user_text,
        active_interaction_session,
        active_conversation_session,
        user_id,
    ) {
        return CoreInboundClassification {
            kind: CoreInboundKind::Immediate,
        };
    }

    let is_command = session_flow::parse_session_command(user_text).is_some()
        || translation_flow::parse_translation_command(user_text).is_some()
        || weather_flow::parse_weather_command(user_text).is_some()
        || train_flow::parse_train_command(user_text).is_some()
        || search_flow::parse_web_search_command(user_text).is_some()
        || rss_flow::parse_rss_command(user_text).is_some()
        || todo_flow::parse_todo_command(user_text).is_some()
        || todo_flow::is_natural_todo_query_text(user_text)
        || memory_flow::parse_memory_command(user_text).is_some();

    CoreInboundClassification {
        kind: if is_command {
            CoreInboundKind::Immediate
        } else {
            CoreInboundKind::NormalChat
        },
    }
}

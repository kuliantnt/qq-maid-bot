//! Respond 路由决策服务。
//!
//! 这里是普通消息进入 Immediate / StreamingChat / AgentRuntime 的唯一决策边界。
//! 它只读取现有 session 状态和 agent policy，不执行命令、不创建会话、不调用 LLM。

use crate::{
    config::{ChatScene, ResolvedAgentPolicy},
    error::LlmError,
    runtime::command::is_slash_command_candidate,
    runtime::tools::classify_status_hint,
    service::{CoreInboundClassification, CoreInboundKind},
};

use super::{
    PlannedRespond, RespondPlan, RespondRequest, RustRespondService,
    agent_route::{self, AgentRouteContext, RespondRoute},
    common::session_error,
    interaction_state::{
        classify_inbound_with_active, interaction_snapshot, pending_blocks_immediate,
        respond_interaction_meta, respond_meta, route_context_session,
    },
    search_flow, session_flow,
};

pub(super) struct RespondRouter<'a> {
    service: &'a RustRespondService,
}

impl<'a> RespondRouter<'a> {
    pub(super) fn new(service: &'a RustRespondService) -> Self {
        Self { service }
    }

    pub(super) fn plan(&self, req: &RespondRequest) -> Result<PlannedRespond, LlmError> {
        let user_text = req.effective_user_text();
        let trimmed = user_text.trim();
        if trimmed.is_empty() && req.effective_input_parts().is_empty() {
            return Ok(PlannedRespond::immediate_chat("deterministic_or_empty"));
        }

        let meta = respond_meta(req);
        let interaction_meta = respond_interaction_meta(req);
        let active_interaction_session = self
            .service
            .session_store
            .get_active(&interaction_meta)
            .map_err(session_error)?;
        let active_conversation_session = self
            .service
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
            return Ok(PlannedRespond::immediate_chat("pending_handler_fallback"));
        }

        if search_flow::parse_web_search_command(&user_text).is_some() {
            // 显式 `/查` 入口统一走 WebSearch，复用 `/查` 的流式查询能力，
            // 避免被通用 slash 命令截走而走非流式完整等待路径。
            return Ok(PlannedRespond::web_search());
        }
        if is_event_wrapped_command(trimmed) {
            return Ok(PlannedRespond::command_event());
        }
        if is_slash_command_candidate(trimmed) {
            return Ok(PlannedRespond::immediate_chat(
                "deterministic_slash_fallback",
            ));
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
            return Ok(PlannedRespond::immediate_chat(
                "deterministic_handler_fallback",
            ));
        }

        let policy = self.resolve_agent_policy(req)?;
        let agent_decision = self.route_agent_runtime(req, &policy);
        let plan = if !req.has_non_text_input_parts()
            && matches!(agent_decision.route, RespondRoute::AgentRuntime)
        {
            RespondPlan::AgentRuntime
        } else {
            RespondPlan::StreamingChat
        };
        // 状态语义在能力路由完成后独立计算，只供展示和 diagnostics 使用。
        // Todo domain 的上下文选择封装在业务状态分类器中，respond 不解释具体 domain。
        let interaction_state = interaction_snapshot(req, route_session);
        let status_hint = match agent_decision.tool_mode() {
            // Memory-only 模式先由 Luna 判断是否真实调用，避免天气、Todo 等状态提示
            // 与本轮唯一可见工具不一致；真实 Tool 进度仍由 Agent 事件产生。
            Some(agent_route::AgentToolMode::MemoryOnly) | None => None,
            Some(agent_route::AgentToolMode::ConfiguredWhitelist) => {
                classify_status_hint(trimmed, &interaction_state)
            }
        };
        tracing::debug!(
            respond_plan = ?plan,
            tool_loop_route = ?agent_decision.route,
            status_subject = ?status_hint.map(|hint| hint.subject.as_str()),
            route_reason = agent_decision.reason,
            is_group = req
                .group_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            input_chars = trimmed.chars().count(),
            enabled_tools_count = policy.enabled_tools.len(),
            "selected core respond route"
        );
        Ok(PlannedRespond::chat(agent_decision, status_hint))
    }

    pub(super) fn classify_inbound(
        &self,
        req: RespondRequest,
    ) -> Result<CoreInboundClassification, LlmError> {
        let user_text = req.effective_user_text();
        let meta = respond_meta(&req);
        let interaction_meta = respond_interaction_meta(&req);
        let active_interaction_session = self
            .service
            .session_store
            .get_active(&interaction_meta)
            .map_err(session_error)?;
        let active_conversation_session = self
            .service
            .session_store
            .get_active(&meta)
            .map_err(session_error)?;

        // Gateway 只提交候选，Core 负责后续注册表与权限判断；所有斜杠候选都应绕过
        // Gateway 普通聊天冷却，确保未知命令也能在 Core 静默收口。
        if is_slash_command_candidate(&user_text) {
            return Ok(CoreInboundClassification {
                kind: CoreInboundKind::Immediate,
            });
        }

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

    pub(super) fn resolve_agent_policy(
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
        self.service.agent_config.resolve(scene)
    }

    fn route_agent_runtime(
        &self,
        req: &RespondRequest,
        policy: &ResolvedAgentPolicy,
    ) -> agent_route::AgentRouteDecision {
        agent_route::route_agent_runtime(
            req,
            AgentRouteContext {
                scene_enabled: policy.enabled,
                tool_calling_enabled: policy.tool_calling_enabled,
                group_tool_calling_enabled: policy.group_tool_calling_enabled,
                provider_supports_tool_calling: self
                    .service
                    .provider
                    .tool_calling_protocol(Some(&policy.main_model))
                    .is_some(),
                enabled_tools_available: !policy.enabled_tools.is_empty(),
                memory_tool_available: policy
                    .enabled_tools
                    .iter()
                    .any(|name| name == crate::runtime::tools::memory::SAVE_MEMORY_TOOL_NAME),
            },
        )
    }
}

impl RustRespondService {
    pub(crate) fn plan_core_respond(
        &self,
        req: &RespondRequest,
    ) -> Result<PlannedRespond, LlmError> {
        RespondRouter::new(self).plan(req)
    }

    pub(crate) fn resolve_agent_policy(
        &self,
        req: &RespondRequest,
    ) -> Result<ResolvedAgentPolicy, LlmError> {
        RespondRouter::new(self).resolve_agent_policy(req)
    }

    pub fn classify_inbound(
        &self,
        req: RespondRequest,
    ) -> Result<CoreInboundClassification, LlmError> {
        RespondRouter::new(self).classify_inbound(req)
    }
}

fn is_event_wrapped_command(text: &str) -> bool {
    session_flow::parse_session_command(text)
        .is_some_and(|command| command.action.as_str() == "help")
}

//! Respond 路由决策服务。
//!
//! 这里是普通消息进入 Immediate / StreamingChat / AgentChat 的唯一决策边界。
//! 它只读取现有 session 状态和 agent policy，不执行命令、不创建会话、不调用 LLM。

use crate::{
    config::{ChatScene, ResolvedAgentPolicy},
    error::LlmError,
    runtime::session::SessionRecord,
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
        if trimmed.starts_with('/') || trimmed.starts_with('／') {
            return Ok(PlannedRespond::immediate_chat(
                "deterministic_slash_fallback",
            ));
        }

        // 在进入 Agent Chat 路由前，先拦截“明确对机器人发起的搜索意图”。
        // 群聊未 @ 机器人时 Gateway 已在入站阶段过滤，Core 侧不再二次判定；
        // 但为安全起见，群聊仍要求存在 directed_to_bot 信号才自动联网查询，
        // 避免出现“查/搜索”等词就触发。私聊天然视为明确发起。
        if agent_route::has_search_intent(trimmed, &trimmed.to_ascii_lowercase())
            && directed_to_bot(req)
        {
            return Ok(PlannedRespond::web_search());
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
        let agent_decision = self.route_agent_chat_with_active(req, &policy, route_session);
        let plan = if !req.has_non_text_input_parts()
            && matches!(agent_decision.route, RespondRoute::AgentChat)
        {
            RespondPlan::AgentChat
        } else {
            RespondPlan::StreamingChat
        };
        tracing::debug!(
            respond_plan = ?plan,
            tool_loop_route = ?agent_decision.route,
            semantic_route = ?agent_decision.semantic_route,
            tool_domain = ?agent_decision.domain,
            route_reason = agent_decision.reason,
            is_group = req
                .group_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            input_chars = trimmed.chars().count(),
            enabled_tools_count = policy.enabled_tools.len(),
            "selected core respond route"
        );
        Ok(PlannedRespond::chat(agent_decision))
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

    fn route_agent_chat_with_active(
        &self,
        req: &RespondRequest,
        policy: &ResolvedAgentPolicy,
        active_session: Option<&SessionRecord>,
    ) -> agent_route::AgentRouteDecision {
        agent_route::route_agent_chat(
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
                interaction_state: interaction_snapshot(req, active_session),
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

/// 判断消息是否明确对机器人发起，用于普通聊天搜索意图是否进入 WebSearch。
///
/// 复用 Gateway 已归一化的 `message_context.mentions[].is_self` 信号，不重复解析
/// 群聊 @ 文本。群聊只有存在 @ 当前机器人的 mention 才算明确发起；私聊天然为真。
/// Gateway 入站过滤已保证群聊无 @ 机器人消息不会进入 Core respond，本判断主要作为
/// Core 内部安全边界，避免直接构造 `RespondRequest` 的调用方误触发联网查询。
fn directed_to_bot(req: &RespondRequest) -> bool {
    let is_group = req
        .group_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if !is_group {
        return true;
    }
    req.message_context
        .as_ref()
        .map(|context| context.mentions.iter().any(|mention| mention.is_self))
        .unwrap_or(false)
}

fn is_event_wrapped_command(text: &str) -> bool {
    session_flow::parse_session_command(text)
        .is_some_and(|command| command.action.as_str() == "help")
}

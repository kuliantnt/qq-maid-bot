//! Respond 路由决策服务。
//!
//! 这里是普通消息进入 Immediate / StreamingChat / CompleteToolLoop 的唯一决策边界。
//! 它只读取现有 session 状态和 agent policy，不执行命令、不创建会话、不调用 LLM。

use crate::{
    config::{ChatScene, ResolvedAgentPolicy},
    error::LlmError,
    runtime::session::SessionRecord,
    service::{CoreInboundClassification, CoreInboundKind},
};

use super::{
    RespondPlan, RespondRequest, RustRespondService,
    common::session_error,
    interaction_state::{
        classify_inbound_with_active, interaction_snapshot, pending_blocks_immediate,
        respond_interaction_meta, respond_meta, route_context_session,
    },
    search_flow,
    status_hint::StatusHint,
    tool_route::{self, ToolLoopRoute, ToolRouteContext},
};

pub(super) struct RespondRouter<'a> {
    service: &'a RustRespondService,
}

impl<'a> RespondRouter<'a> {
    pub(super) fn new(service: &'a RustRespondService) -> Self {
        Self { service }
    }

    pub(super) fn status_hint_for_plan(
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
        Ok(self
            .route_tool_loop_with_active(req, &policy, route_session)
            .status_hint
            .unwrap_or_else(StatusHint::default_tool))
    }

    pub(super) fn plan(&self, req: &RespondRequest) -> Result<RespondPlan, LlmError> {
        let user_text = req.effective_user_text();
        let trimmed = user_text.trim();
        if trimmed.is_empty() && req.effective_input_parts().is_empty() {
            return Ok(RespondPlan::Immediate);
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
    pub(crate) fn status_hint_for_plan(
        &self,
        req: &RespondRequest,
        plan: RespondPlan,
    ) -> Result<StatusHint, LlmError> {
        RespondRouter::new(self).status_hint_for_plan(req, plan)
    }

    pub(crate) fn plan_core_respond(&self, req: &RespondRequest) -> Result<RespondPlan, LlmError> {
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

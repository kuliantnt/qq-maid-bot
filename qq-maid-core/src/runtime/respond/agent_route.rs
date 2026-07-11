//! 普通消息 Agent 能力前置路由。
//!
//! 代码侧只决定当前请求是否允许向模型暴露工具。通过场景、Provider 能力、
//! 群聊开关和白名单约束后，普通纯文本消息统一进入具备原生 Tool Calling 的
//! 模型流程。本模块不读取业务交互状态，也不生成状态提示。

use super::RespondRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RespondRoute {
    StandardChat,
    AgentRuntime,
}

impl RespondRoute {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::StandardChat => "standard_chat",
            Self::AgentRuntime => "agent_runtime",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AgentRouteDecision {
    pub route: RespondRoute,
    pub reason: &'static str,
}

impl AgentRouteDecision {
    /// 为确定性分派准备普通聊天兜底。Router 必须在执行前确定 reason，
    /// dispatcher 不得在 handler 未消费时临时补造路由信息。
    pub(super) const fn plain_deterministic(reason: &'static str) -> Self {
        Self {
            route: RespondRoute::StandardChat,
            reason,
        }
    }

    pub(super) const fn uses_agent_runtime(self) -> bool {
        matches!(self.route, RespondRoute::AgentRuntime)
    }
}

#[derive(Debug, Clone)]
pub(super) struct AgentRouteContext {
    pub scene_enabled: bool,
    pub tool_calling_enabled: bool,
    pub group_tool_calling_enabled: bool,
    pub provider_supports_tool_calling: bool,
    pub enabled_tools_available: bool,
}

pub(super) fn route_agent_runtime(
    req: &RespondRequest,
    ctx: AgentRouteContext,
) -> AgentRouteDecision {
    let is_group = req
        .group_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if !ctx.scene_enabled {
        return decision(RespondRoute::StandardChat, "agent_unavailable");
    }
    // 群聊开关是独立的安全边界，diagnostics 应保留该原因，不能被通用
    // tool_calling_enabled guard 折叠为无法定位配置项的 unavailable。
    if is_group && !ctx.group_tool_calling_enabled {
        return decision(RespondRoute::StandardChat, "group_agent_disabled");
    }
    if !ctx.tool_calling_enabled
        || !ctx.provider_supports_tool_calling
        || !ctx.enabled_tools_available
    {
        return decision(RespondRoute::StandardChat, "agent_unavailable");
    }
    if req.has_non_text_input_parts() {
        return decision(RespondRoute::StandardChat, "multimodal_standard_chat");
    }
    let text = req.effective_user_text();
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.starts_with('／') {
        return decision(RespondRoute::StandardChat, "deterministic_or_empty");
    }

    decision(RespondRoute::AgentRuntime, "agent_runtime_available")
}

fn decision(route: RespondRoute, reason: &'static str) -> AgentRouteDecision {
    AgentRouteDecision { route, reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(text: &str) -> RespondRequest {
        RespondRequest {
            content: text.to_owned(),
            scope_key: "private:u1".to_owned(),
            user_id: Some("u1".to_owned()),
            platform: "qq_official".to_owned(),
            ..Default::default()
        }
    }

    fn context() -> AgentRouteContext {
        AgentRouteContext {
            scene_enabled: true,
            tool_calling_enabled: true,
            group_tool_calling_enabled: false,
            provider_supports_tool_calling: true,
            enabled_tools_available: true,
        }
    }

    #[test]
    fn private_standard_messages_enter_tool_capable_agent() {
        for input in [
            "晚上好",
            "下午好呀",
            "我晚上有点累",
            "写一段长文本测试流式",
            "讲个故事",
            "解释一下 Rust 所有权",
            "T3 架构怎么设计",
            "天气",
            "今天天气真好",
            "聊聊天气",
        ] {
            let decision = route_agent_runtime(&request(input), context());
            assert_eq!(decision.route, RespondRoute::AgentRuntime, "{input}");
        }
    }

    #[test]
    fn disabled_or_group_request_keeps_standard_route() {
        let mut group = request("杭州明天要带伞吗");
        group.group_id = Some("g1".to_owned());
        assert_eq!(
            route_agent_runtime(&group, context()).route,
            RespondRoute::StandardChat
        );
        assert_eq!(
            route_agent_runtime(
                &request("杭州明天要带伞吗"),
                AgentRouteContext {
                    tool_calling_enabled: false,
                    ..context()
                },
            )
            .route,
            RespondRoute::StandardChat
        );
    }

    #[test]
    fn group_request_uses_tool_loop_when_group_switch_enabled() {
        let mut group = request("杭州明天要带伞吗");
        group.group_id = Some("g1".to_owned());
        assert_eq!(
            route_agent_runtime(
                &group,
                AgentRouteContext {
                    group_tool_calling_enabled: true,
                    ..context()
                },
            )
            .route,
            RespondRoute::AgentRuntime
        );
    }

    #[test]
    fn availability_guards_keep_plain_route() {
        for ctx in [
            AgentRouteContext {
                scene_enabled: false,
                ..context()
            },
            AgentRouteContext {
                enabled_tools_available: false,
                ..context()
            },
            AgentRouteContext {
                provider_supports_tool_calling: false,
                ..context()
            },
        ] {
            assert_eq!(
                route_agent_runtime(&request("杭州明天要带伞吗"), ctx).route,
                RespondRoute::StandardChat
            );
        }
    }
}

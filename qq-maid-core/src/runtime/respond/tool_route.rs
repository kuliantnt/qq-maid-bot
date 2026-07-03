//! 普通私聊 Tool Loop 前置路由。
//!
//! 私聊普通消息不再按 Todo/Weather 关键词猜测意图；只要工具能力可用，就进入
//! 具备受控工具的 Agent 路径，由模型决定是否调用工具。slash 命令、确定性
//! Todo 查询仍在更外层保持原有路径；群聊 Tool Loop 需要额外显式开关。

use super::RespondRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolLoopRoute {
    PlainChat,
    CompleteToolLoop,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ToolRouteContext {
    pub tool_calling_enabled: bool,
    pub group_tool_calling_enabled: bool,
    pub provider_supports_tool_calling: bool,
}

pub(super) fn route_tool_loop(req: &RespondRequest, ctx: ToolRouteContext) -> ToolLoopRoute {
    if !ctx.tool_calling_enabled || !ctx.provider_supports_tool_calling {
        return ToolLoopRoute::PlainChat;
    }
    let text = req.effective_user_text();
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.starts_with('／') {
        return ToolLoopRoute::PlainChat;
    }
    if req
        .group_id
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        && !ctx.group_tool_calling_enabled
    {
        return ToolLoopRoute::PlainChat;
    }

    ToolLoopRoute::CompleteToolLoop
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

    fn context() -> ToolRouteContext {
        ToolRouteContext {
            tool_calling_enabled: true,
            group_tool_calling_enabled: false,
            provider_supports_tool_calling: true,
        }
    }

    #[test]
    fn private_plain_message_uses_tool_loop_when_tool_calling_enabled() {
        assert_eq!(
            route_tool_loop(&request("晚上好"), context()),
            ToolLoopRoute::CompleteToolLoop
        );
        assert_eq!(
            route_tool_loop(&request("删除第二条"), context()),
            ToolLoopRoute::CompleteToolLoop
        );
        assert_eq!(
            route_tool_loop(&request("新增待办，明天接老公"), context()),
            ToolLoopRoute::CompleteToolLoop
        );
    }

    #[test]
    fn disabled_or_group_request_keeps_plain_route() {
        let mut group = request("杭州明天要带伞吗");
        group.group_id = Some("g1".to_owned());
        assert_eq!(route_tool_loop(&group, context()), ToolLoopRoute::PlainChat);
        assert_eq!(
            route_tool_loop(
                &request("杭州明天要带伞吗"),
                ToolRouteContext {
                    tool_calling_enabled: false,
                    group_tool_calling_enabled: false,
                    provider_supports_tool_calling: true,
                },
            ),
            ToolLoopRoute::PlainChat
        );
    }

    #[test]
    fn group_request_uses_tool_loop_when_group_switch_enabled() {
        let mut group = request("杭州明天要带伞吗");
        group.group_id = Some("g1".to_owned());
        assert_eq!(
            route_tool_loop(
                &group,
                ToolRouteContext {
                    group_tool_calling_enabled: true,
                    ..context()
                },
            ),
            ToolLoopRoute::CompleteToolLoop
        );
    }
}

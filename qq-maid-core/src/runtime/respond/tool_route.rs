//! 普通消息 Tool Loop 前置路由。
//!
//! 本模块只保留通用入口、公共类型和跨域调度胶水。具体工具域的自然语言
//! 路由规则优先由工具域提供；当前 Todo/Reminder 规则已下沉到
//! `runtime::tools::todo::route`，其他工具域规则由
//! `respond::tool_route_domains` 调度到对应 domain provider，避免本入口重新膨胀成业务判断集合。

use crate::runtime::tools::todo::route::{self as todo_route, TodoRouteAction, TodoRouteKind};

use super::{
    RespondRequest,
    interaction_state::{InteractionDomain, InteractionStateSnapshot},
    plain_chat_route,
    status_hint::{StatusAction, StatusHint, StatusSubject},
    tool_route_domains,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RespondRoute {
    PlainChat,
    ToolAgent,
}

impl RespondRoute {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::PlainChat => "plain_chat",
            Self::ToolAgent => "tool_agent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SemanticRoute {
    PlainChat,
    ToolLoop,
    Deterministic,
    Ambiguous,
}

impl SemanticRoute {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::PlainChat => "plain_chat",
            Self::ToolLoop => "tool_loop",
            Self::Deterministic => "deterministic",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolDomain {
    Todo,
    Weather,
    Train,
    Rss,
    Search,
    Memory,
    Unknown,
}

impl ToolDomain {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Weather => "weather",
            Self::Train => "train",
            Self::Rss => "rss",
            Self::Search => "search",
            Self::Memory => "memory",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ToolRouteDecision {
    pub route: RespondRoute,
    pub semantic_route: SemanticRoute,
    pub domain: ToolDomain,
    pub reason: &'static str,
    pub status_hint: Option<StatusHint>,
}

impl ToolRouteDecision {
    /// 为确定性分派准备普通聊天兜底。Router 必须在执行前确定 reason，
    /// dispatcher 不得在 handler 未消费时临时补造路由信息。
    pub(super) const fn plain_deterministic(reason: &'static str) -> Self {
        Self {
            route: RespondRoute::PlainChat,
            semantic_route: SemanticRoute::Deterministic,
            domain: ToolDomain::Unknown,
            reason,
            status_hint: None,
        }
    }

    pub(super) const fn uses_tool_agent(self) -> bool {
        matches!(self.route, RespondRoute::ToolAgent)
    }

    pub(super) fn domains(self) -> Vec<&'static str> {
        if matches!(self.domain, ToolDomain::Unknown) {
            Vec::new()
        } else {
            vec![self.domain.as_str()]
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ToolRouteContext {
    pub scene_enabled: bool,
    pub tool_calling_enabled: bool,
    pub group_tool_calling_enabled: bool,
    pub provider_supports_tool_calling: bool,
    pub enabled_tools_available: bool,
    /// 当前请求可见的通用交互状态快照，由路由层按 domain 消费。
    pub interaction_state: InteractionStateSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SemanticAssessment {
    pub semantic_route: SemanticRoute,
    pub domain: ToolDomain,
    pub reason: &'static str,
}

pub(super) fn route_tool_loop(req: &RespondRequest, ctx: ToolRouteContext) -> ToolRouteDecision {
    let is_group = req
        .group_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if !ctx.scene_enabled {
        return decision(
            RespondRoute::PlainChat,
            SemanticRoute::PlainChat,
            ToolDomain::Unknown,
            "tool_loop_unavailable",
            None,
        );
    }
    // 群聊开关是独立的安全边界，diagnostics 应保留该原因，不能被通用
    // tool_calling_enabled guard 折叠为无法定位配置项的 unavailable。
    if is_group && !ctx.group_tool_calling_enabled {
        return decision(
            RespondRoute::PlainChat,
            SemanticRoute::PlainChat,
            ToolDomain::Unknown,
            "group_tool_loop_disabled",
            None,
        );
    }
    if !ctx.tool_calling_enabled
        || !ctx.provider_supports_tool_calling
        || !ctx.enabled_tools_available
    {
        return decision(
            RespondRoute::PlainChat,
            SemanticRoute::PlainChat,
            ToolDomain::Unknown,
            "tool_loop_unavailable",
            None,
        );
    }
    if req.has_non_text_input_parts() {
        return decision(
            RespondRoute::PlainChat,
            SemanticRoute::PlainChat,
            ToolDomain::Unknown,
            "multimodal_plain_chat",
            None,
        );
    }
    let text = req.effective_user_text();
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.starts_with('／') {
        return decision(
            RespondRoute::PlainChat,
            SemanticRoute::Deterministic,
            ToolDomain::Unknown,
            "deterministic_or_empty",
            None,
        );
    }
    let has_recent_todo_context = ctx
        .interaction_state
        .has_recent_context(InteractionDomain::Todo);
    let assessment = classify_semantic_route(trimmed, has_recent_todo_context);
    let status_hint = classify_status_hint(trimmed, has_recent_todo_context);
    match assessment.semantic_route {
        SemanticRoute::PlainChat => decision(
            RespondRoute::PlainChat,
            SemanticRoute::PlainChat,
            assessment.domain,
            assessment.reason,
            None,
        ),
        SemanticRoute::ToolLoop => decision(
            RespondRoute::ToolAgent,
            SemanticRoute::ToolLoop,
            assessment.domain,
            assessment.reason,
            status_hint,
        ),
        SemanticRoute::Deterministic => decision(
            RespondRoute::PlainChat,
            SemanticRoute::Deterministic,
            assessment.domain,
            "deterministic_or_empty",
            None,
        ),
        SemanticRoute::Ambiguous if is_group => decision(
            RespondRoute::PlainChat,
            SemanticRoute::Ambiguous,
            assessment.domain,
            "semantic_ambiguous_group_plain",
            None,
        ),
        SemanticRoute::Ambiguous => decision(
            RespondRoute::PlainChat,
            SemanticRoute::Ambiguous,
            assessment.domain,
            assessment.reason,
            None,
        ),
    }
}

pub(super) fn has_search_intent(text: &str, lower: &str) -> bool {
    tool_route_domains::has_search_intent(text, lower)
}

fn decision(
    route: RespondRoute,
    semantic_route: SemanticRoute,
    domain: ToolDomain,
    reason: &'static str,
    status_hint: Option<StatusHint>,
) -> ToolRouteDecision {
    ToolRouteDecision {
        route,
        semantic_route,
        domain,
        reason,
        status_hint,
    }
}

pub(super) fn assessment(
    semantic_route: SemanticRoute,
    domain: ToolDomain,
    reason: &'static str,
) -> SemanticAssessment {
    SemanticAssessment {
        semantic_route,
        domain,
        reason,
    }
}

fn classify_semantic_route(text: &str, has_recent_todo_context: bool) -> SemanticAssessment {
    let lower = text.to_ascii_lowercase();
    if text.starts_with('/') || text.starts_with('／') {
        return assessment(
            SemanticRoute::Deterministic,
            ToolDomain::Unknown,
            "deterministic_or_empty",
        );
    }

    let plain_chat_candidate = plain_chat_route::has_plain_chat_intent(text, &lower);
    let todo_intent = todo_route::classify_todo_route(
        text,
        &lower,
        has_recent_todo_context,
        plain_chat_candidate,
    );
    if todo_intent.routes_to_tool_loop() {
        return assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Todo,
            todo_intent.reason,
        );
    }
    if matches!(todo_intent.kind, TodoRouteKind::NumberContextMissing) {
        return assessment(
            SemanticRoute::PlainChat,
            ToolDomain::Todo,
            todo_intent.reason,
        );
    }

    if let Some(assessment) = tool_route_domains::classify_non_todo_route(text, &lower) {
        return assessment;
    }
    if tool_route_domains::mentions_inert_weather_topic(text) {
        return assessment(
            SemanticRoute::PlainChat,
            ToolDomain::Unknown,
            "semantic_plain_chat",
        );
    }
    if plain_chat_candidate {
        return assessment(
            SemanticRoute::PlainChat,
            ToolDomain::Unknown,
            "semantic_plain_chat",
        );
    }
    if matches!(todo_intent.kind, TodoRouteKind::ContextReferenceMissing) {
        return assessment(
            SemanticRoute::PlainChat,
            ToolDomain::Todo,
            todo_intent.reason,
        );
    }
    if plain_chat_route::has_ambiguous_toolish_intent(text) {
        return assessment(
            SemanticRoute::Ambiguous,
            ToolDomain::Unknown,
            "semantic_ambiguous_plain",
        );
    }

    assessment(
        SemanticRoute::Ambiguous,
        ToolDomain::Unknown,
        "semantic_ambiguous_plain",
    )
}

fn classify_status_hint(text: &str, has_recent_todo_context: bool) -> Option<StatusHint> {
    let lower = text.to_ascii_lowercase();
    let plain_chat_candidate = plain_chat_route::has_plain_chat_intent(text, &lower);
    let todo_intent = todo_route::classify_todo_route(
        text,
        &lower,
        has_recent_todo_context,
        plain_chat_candidate,
    );
    if todo_intent.routes_to_tool_loop() {
        let action = if todo_route::routes_as_todo_write_status(text, plain_chat_candidate) {
            StatusAction::Write
        } else {
            match todo_route::todo_route_action(text) {
                TodoRouteAction::Confirm => StatusAction::Confirm,
                TodoRouteAction::Write => StatusAction::Write,
                TodoRouteAction::Query => StatusAction::Query,
                TodoRouteAction::Process => StatusAction::Process,
            }
        };
        return Some(StatusHint::new(StatusSubject::Todo, action));
    }
    tool_route_domains::classify_non_todo_status_hint(text, &lower)
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
            scene_enabled: true,
            tool_calling_enabled: true,
            group_tool_calling_enabled: false,
            provider_supports_tool_calling: true,
            enabled_tools_available: true,
            interaction_state: InteractionStateSnapshot::default(),
        }
    }

    fn context_with_recent_todo() -> ToolRouteContext {
        ToolRouteContext {
            interaction_state: InteractionStateSnapshot::with_recent_todo_context_for_test(),
            ..context()
        }
    }

    #[test]
    fn private_plain_messages_keep_streaming_chat() {
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
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, RespondRoute::PlainChat, "{input}");
            assert_eq!(decision.semantic_route, SemanticRoute::PlainChat, "{input}");
            assert_eq!(decision.status_hint, None, "{input}");
        }
    }

    #[test]
    fn local_text_processing_requests_do_not_count_as_search_intent() {
        let codex_output = "\
Codex 分析结果：
- route 命中了 WebSearch
- 查询工具返回：查询内容太长
- tool_route 里出现 search 关键词
人话说这个";
        let log_output = "\
2026-07-08 ERROR query failed
WebSearch tool timeout
查询内容太长，请压缩到 200 字以内再试。
总结这段";

        for input in [codex_output, log_output] {
            let lower = input.to_ascii_lowercase();
            assert!(!has_search_intent(input, &lower), "{input}");
            let decision = route_tool_loop(&request(input), context());
            assert_ne!(decision.domain, ToolDomain::Search, "{input}");
            assert_ne!(decision.semantic_route, SemanticRoute::ToolLoop, "{input}");
        }
    }

    #[test]
    fn explicit_search_requests_keep_search_intent() {
        for input in [
            "查一下今天 AI 新闻",
            "联网查询一下 Rust async sink 是什么",
            "搜索一下这个报错",
            "查 GitHub 上有没有相关 issue",
            "最新的 Rust 版本是什么",
        ] {
            let lower = input.to_ascii_lowercase();
            assert!(has_search_intent(input, &lower), "{input}");
        }
    }

    #[test]
    fn private_tool_intent_uses_tool_loop_when_tool_calling_enabled() {
        for input in [
            "新增待办，明天接老公",
            "编辑第三条，其他不动",
            "清除第三条详情",
            "第三条不要详情了",
            "把第二条的备注删掉",
            "第三条和第四条详情都不需要",
            "记一下我喜欢少糖",
            "别忘了买菜",
            "提醒我续费",
            "下午检查发布清单",
            "周四项目 A 完成初稿",
            "杭州明天要带伞吗",
            "查一下 G1 时刻",
            "查看上次 codex 发布的 rss",
        ] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, RespondRoute::ToolAgent, "{input}");
            assert_eq!(decision.semantic_route, SemanticRoute::ToolLoop, "{input}");
            assert!(decision.status_hint.is_some(), "{input}");
        }
    }

    #[test]
    fn tool_intent_carries_existing_status_hints() {
        let cases = [
            (
                "杭州明天要带伞吗",
                StatusHint::new(StatusSubject::Weather, StatusAction::Query),
            ),
            (
                "新增待办，明天接老公",
                StatusHint::new(StatusSubject::Todo, StatusAction::Write),
            ),
            (
                "下午检查发布清单",
                StatusHint::new(StatusSubject::Todo, StatusAction::Write),
            ),
            (
                "完成第一条",
                StatusHint::new(StatusSubject::Todo, StatusAction::Confirm),
            ),
            (
                "查看待办有哪些",
                StatusHint::new(StatusSubject::Todo, StatusAction::Query),
            ),
            (
                "查一下 G1 时刻",
                StatusHint::new(StatusSubject::Train, StatusAction::Query),
            ),
        ];

        for (input, expected_hint) in cases {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, RespondRoute::ToolAgent, "{input}");
            assert_eq!(decision.status_hint, Some(expected_hint), "{input}");
        }
    }

    #[test]
    fn todo_reference_routes_by_strength_and_recent_context() {
        for input in [
            "完成第一条",
            "处理第一项",
            "取消它",
            "删掉这个",
            "这些完成了",
        ] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, RespondRoute::ToolAgent, "{input}");
            assert_eq!(decision.domain, ToolDomain::Todo, "{input}");
        }

        for input in ["这个改一下", "都删除了吧"] {
            let without_context = route_tool_loop(&request(input), context());
            assert_eq!(without_context.route, RespondRoute::PlainChat, "{input}");
            assert_eq!(
                without_context.reason, "todo_reference_context_missing",
                "{input}"
            );

            let with_context = route_tool_loop(&request(input), context_with_recent_todo());
            assert_eq!(with_context.route, RespondRoute::ToolAgent, "{input}");
            assert_eq!(with_context.domain, ToolDomain::Todo, "{input}");
        }
    }

    #[test]
    fn bare_number_todo_operations_require_recent_context() {
        for input in ["7删除", "删除7", "把7合并到6", "6和7合并"] {
            let without_context = route_tool_loop(&request(input), context());
            assert_eq!(without_context.route, RespondRoute::PlainChat, "{input}");
            assert_eq!(
                without_context.reason, "todo_number_context_missing",
                "{input}"
            );

            let with_context = route_tool_loop(&request(input), context_with_recent_todo());
            assert_eq!(with_context.route, RespondRoute::ToolAgent, "{input}");
            assert_eq!(with_context.domain, ToolDomain::Todo, "{input}");
        }
    }

    #[test]
    fn ambiguous_private_defaults_to_plain_chat() {
        for input in ["安排一下", "帮我处理一下", "刚刚没看到，再来一条"] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, RespondRoute::PlainChat, "{input}");
            assert_eq!(decision.semantic_route, SemanticRoute::Ambiguous, "{input}");
            assert_eq!(decision.status_hint, None, "{input}");
        }
    }

    #[test]
    fn disabled_or_group_request_keeps_plain_route() {
        let mut group = request("杭州明天要带伞吗");
        group.group_id = Some("g1".to_owned());
        assert_eq!(
            route_tool_loop(&group, context()).route,
            RespondRoute::PlainChat
        );
        assert_eq!(
            route_tool_loop(
                &request("杭州明天要带伞吗"),
                ToolRouteContext {
                    tool_calling_enabled: false,
                    ..context()
                },
            )
            .route,
            RespondRoute::PlainChat
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
            )
            .route,
            RespondRoute::ToolAgent
        );
    }

    #[test]
    fn availability_guards_keep_plain_route() {
        for ctx in [
            ToolRouteContext {
                scene_enabled: false,
                ..context()
            },
            ToolRouteContext {
                enabled_tools_available: false,
                ..context()
            },
            ToolRouteContext {
                provider_supports_tool_calling: false,
                ..context()
            },
        ] {
            assert_eq!(
                route_tool_loop(&request("杭州明天要带伞吗"), ctx).route,
                RespondRoute::PlainChat
            );
        }
    }
}

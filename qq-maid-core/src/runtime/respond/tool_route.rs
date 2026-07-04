//! 普通消息 Tool Loop 前置路由。
//!
//! slash 命令、pending 和确定性 Todo 查询仍在更外层保持原有路径。这里仅判断
//! 普通聊天是否需要进入受控工具 Agent：明显闲聊、创作、解释和流式测试保留
//! 原生聊天路径；明确工具任务才进入 Tool Loop。不确定的私聊仍保守交给 Agent，
//! 群聊不确定则默认保持普通聊天，避免群聊闲聊频繁阻塞在工具循环。

use super::{
    RespondRequest,
    status_hint::{StatusAction, StatusHint, StatusSubject},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolLoopRoute {
    PlainChat,
    CompleteToolLoop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SemanticRoute {
    PlainChat,
    ToolLoop,
    Deterministic,
    Ambiguous,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ToolRouteDecision {
    pub route: ToolLoopRoute,
    pub semantic_route: SemanticRoute,
    pub reason: &'static str,
    pub status_hint: Option<StatusHint>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ToolRouteContext {
    pub scene_enabled: bool,
    pub tool_calling_enabled: bool,
    pub group_tool_calling_enabled: bool,
    pub provider_supports_tool_calling: bool,
    pub enabled_tools_available: bool,
}

const TODO_OBJECT_MARKERS: &[&str] = &["待办", "代办", "任务", "提醒", "事项"];
const TODO_WRITE_MARKERS: &[&str] = &[
    "新增",
    "添加",
    "加个",
    "加一",
    "创建",
    "记一下",
    "记录",
    "提醒我",
    "别忘",
    "编辑",
    "修改",
    "改成",
];
const TODO_CONFIRM_MARKERS: &[&str] = &["完成", "做完", "恢复", "取消", "删除", "删掉", "移除"];
const TODO_QUERY_MARKERS: &[&str] = &["查看", "看一下", "列出", "有哪些"];

pub(super) fn route_tool_loop(req: &RespondRequest, ctx: ToolRouteContext) -> ToolRouteDecision {
    if !ctx.scene_enabled
        || !ctx.tool_calling_enabled
        || !ctx.provider_supports_tool_calling
        || !ctx.enabled_tools_available
    {
        return decision(
            ToolLoopRoute::PlainChat,
            SemanticRoute::PlainChat,
            "tool_loop_unavailable",
            None,
        );
    }
    let text = req.effective_user_text();
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') || trimmed.starts_with('／') {
        return decision(
            ToolLoopRoute::PlainChat,
            SemanticRoute::Deterministic,
            "deterministic_or_empty",
            None,
        );
    }
    let is_group = req
        .group_id
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if is_group && !ctx.group_tool_calling_enabled {
        return decision(
            ToolLoopRoute::PlainChat,
            SemanticRoute::PlainChat,
            "group_tool_loop_disabled",
            None,
        );
    }

    let semantic = classify_semantic_route(trimmed);
    let status_hint = classify_status_hint(trimmed);
    match semantic {
        SemanticRoute::PlainChat => decision(
            ToolLoopRoute::PlainChat,
            SemanticRoute::PlainChat,
            "semantic_plain_chat",
            None,
        ),
        SemanticRoute::ToolLoop => decision(
            ToolLoopRoute::CompleteToolLoop,
            SemanticRoute::ToolLoop,
            "semantic_tool_intent",
            status_hint,
        ),
        SemanticRoute::Deterministic => decision(
            ToolLoopRoute::PlainChat,
            SemanticRoute::Deterministic,
            "deterministic_or_empty",
            None,
        ),
        SemanticRoute::Ambiguous if is_group => decision(
            ToolLoopRoute::PlainChat,
            SemanticRoute::Ambiguous,
            "semantic_ambiguous_group_plain",
            None,
        ),
        SemanticRoute::Ambiguous => decision(
            ToolLoopRoute::CompleteToolLoop,
            SemanticRoute::Ambiguous,
            "semantic_ambiguous_private_tool_loop",
            Some(StatusHint::default_tool()),
        ),
    }
}

fn decision(
    route: ToolLoopRoute,
    semantic_route: SemanticRoute,
    reason: &'static str,
    status_hint: Option<StatusHint>,
) -> ToolRouteDecision {
    ToolRouteDecision {
        route,
        semantic_route,
        reason,
        status_hint,
    }
}

fn classify_semantic_route(text: &str) -> SemanticRoute {
    let lower = text.to_ascii_lowercase();
    if text.starts_with('/') || text.starts_with('／') {
        return SemanticRoute::Deterministic;
    }

    // 明确工具意图优先，避免“写一个待办”“讲一下今天待办”被创作/解释词误判为闲聊。
    if has_todo_intent(text, &lower)
        || has_memory_intent(text, &lower)
        || has_weather_intent(text, &lower)
        || has_train_intent(text, &lower)
        || has_rss_intent(text, &lower)
    {
        return SemanticRoute::ToolLoop;
    }

    if mentions_inert_weather_topic(text) {
        return SemanticRoute::PlainChat;
    }

    if has_plain_chat_intent(text, &lower) {
        return SemanticRoute::PlainChat;
    }

    if has_ambiguous_toolish_intent(text) {
        return SemanticRoute::Ambiguous;
    }

    SemanticRoute::Ambiguous
}

fn classify_status_hint(text: &str) -> Option<StatusHint> {
    let lower = text.to_ascii_lowercase();
    if has_todo_intent(text, &lower) {
        return Some(StatusHint::new(
            StatusSubject::Todo,
            todo_status_action(text),
        ));
    }
    if has_memory_intent(text, &lower) {
        return Some(StatusHint::new(StatusSubject::Record, StatusAction::Read));
    }
    if has_weather_intent(text, &lower) {
        return Some(StatusHint::new(StatusSubject::Weather, StatusAction::Query));
    }
    if has_train_intent(text, &lower) {
        return Some(StatusHint::new(StatusSubject::Train, StatusAction::Query));
    }
    if has_rss_intent(text, &lower) {
        return Some(StatusHint::new(StatusSubject::Rss, StatusAction::Query));
    }
    None
}

fn todo_status_action(text: &str) -> StatusAction {
    if contains_any(text, TODO_CONFIRM_MARKERS) {
        return StatusAction::Confirm;
    }
    if contains_any(text, TODO_WRITE_MARKERS) {
        return StatusAction::Write;
    }
    if contains_any(text, TODO_QUERY_MARKERS) {
        return StatusAction::Query;
    }
    StatusAction::Process
}

fn has_todo_intent(text: &str, lower: &str) -> bool {
    let has_todo_object = contains_any(text, TODO_OBJECT_MARKERS) || lower.contains("todo");
    let has_todo_action = contains_any(text, TODO_WRITE_MARKERS)
        || contains_any(text, TODO_CONFIRM_MARKERS)
        || contains_any(text, TODO_QUERY_MARKERS);
    if has_todo_object && has_todo_action {
        return true;
    }

    (contains_any(text, TODO_CONFIRM_MARKERS) || contains_any(text, &["编辑", "修改", "改成"]))
        && (has_ordinal_reference(text) || contains_any(text, &["它", "这个", "那个", "刚才那条"]))
}

fn has_memory_intent(text: &str, lower: &str) -> bool {
    lower.contains("memory")
        || contains_any(text, &["记忆"])
        || contains_any(text, &["记一下", "记住", "帮我记", "记录一下", "保存一下"])
}

fn has_weather_intent(text: &str, _lower: &str) -> bool {
    if contains_any(
        text,
        &[
            "下雨",
            "有雨",
            "带伞",
            "冷吗",
            "热吗",
            "穿什么",
            "几度",
            "预报",
            "预警",
            "台风",
        ],
    ) {
        return true;
    }
    if looks_like_city_weather_query(text) {
        return true;
    }
    contains_any(text, &["天气", "气温", "温度"])
        && contains_any(
            text,
            &[
                "查",
                "查询",
                "看看",
                "看下",
                "看一下",
                "怎么样",
                "如何",
                "多少",
                "会不会",
                "有没有",
            ],
        )
}

fn mentions_inert_weather_topic(text: &str) -> bool {
    contains_any(text, &["天气", "气温", "温度"]) && !has_weather_intent(text, "")
}

fn looks_like_city_weather_query(text: &str) -> bool {
    let compact = text.split_whitespace().collect::<String>();
    let Some(city) = compact.strip_suffix("天气") else {
        return false;
    };
    !city.is_empty()
        && city.chars().count() <= 12
        && !contains_any(
            city,
            &[
                "聊聊", "讨论", "关于", "这个", "那个", "一说", "说到", "如果", "因为",
            ],
        )
}

fn has_train_intent(text: &str, _lower: &str) -> bool {
    contains_any(
        text,
        &["火车", "列车", "车次", "高铁", "动车", "时刻", "站台"],
    ) || has_train_code(text)
}

fn has_rss_intent(text: &str, lower: &str) -> bool {
    lower.contains("rss") || contains_any(text, &["订阅更新", "最近订阅", "订阅记录"])
}

fn has_plain_chat_intent(text: &str, lower: &str) -> bool {
    let compact = text.split_whitespace().collect::<String>();
    matches!(
        compact.as_str(),
        "你好" | "您好" | "晚上好" | "早上好" | "中午好" | "下午好" | "你在吗" | "在吗"
    ) || matches!(lower.trim(), "hi" | "hello" | "hey")
        || contains_any(text, &["陪我聊", "聊会", "闲聊", "说说话", "聊聊天"])
        || contains_any(
            text,
            &[
                "写一段",
                "写一篇",
                "写首",
                "生成一段",
                "输出一段",
                "试试输出",
                "长文本",
                "流式",
                "讲个故事",
                "讲故事",
                "小说",
                "文案",
            ],
        )
        || contains_any(
            text,
            &[
                "解释一下",
                "讲解",
                "介绍一下",
                "分析一下",
                "聊聊",
                "为什么",
                "怎么理解",
                "怎么设计",
                "怎么选",
                "架构",
                "模型",
                "版本说明",
                "消息发送失败",
                "流式还有问题",
                "排障",
            ],
        )
}

fn has_ambiguous_toolish_intent(text: &str) -> bool {
    contains_any(
        text,
        &["安排一下", "处理一下", "帮我处理", "别忘了", "回头提醒"],
    )
}

fn has_ordinal_reference(text: &str) -> bool {
    contains_any(
        text,
        &[
            "第一", "第二", "第三", "第四", "第五", "第六", "第七", "第八", "第九", "第十", "第 1",
            "第 2", "第 3", "第 4", "第 5", "第 6", "第 7", "第 8", "第 9",
        ],
    )
}

fn has_train_code(text: &str) -> bool {
    let chars = text.chars().collect::<Vec<_>>();
    for start in 0..chars.len() {
        let ch = chars[start];
        if !matches!(
            ch,
            'G' | 'D' | 'C' | 'K' | 'Z' | 'T' | 'g' | 'd' | 'c' | 'k' | 'z' | 't'
        ) || !is_train_code_boundary(chars.get(start.wrapping_sub(1)).copied())
        {
            continue;
        }

        let mut end = start + 1;
        while end < chars.len() && chars[end].is_ascii_digit() && end - start <= 5 {
            end += 1;
        }
        let digit_count = end - start - 1;
        // 单数字车次在技术语境中误伤很高，当前只保留常见的 G1 这类高铁短码。
        let allow_single_digit = matches!(ch, 'G' | 'g');
        let valid_digit_count =
            (2..=5).contains(&digit_count) || digit_count == 1 && allow_single_digit;
        if valid_digit_count && is_train_code_boundary(chars.get(end).copied()) {
            return true;
        }
    }
    false
}

fn is_train_code_boundary(ch: Option<char>) -> bool {
    match ch {
        None => true,
        Some(ch) => ch.is_whitespace() || ch.is_ascii_punctuation() || is_cjk_punctuation(ch),
    }
}

fn is_cjk_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '，' | '。'
            | '、'
            | '：'
            | '；'
            | '？'
            | '！'
            | '（'
            | '）'
            | '【'
            | '】'
            | '《'
            | '》'
            | '“'
            | '”'
            | '‘'
            | '’'
    )
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
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
        }
    }

    #[test]
    fn private_plain_messages_keep_streaming_chat() {
        for input in [
            "晚上好",
            "你在吗",
            "能试试输出一段长文本，我试试流式输出",
            "写一段长文本测试流式",
            "讲个故事",
            "解释一下 Rust 所有权",
            "C2C 流式还有问题",
            "C2C 消息发送失败",
            "T3 架构怎么设计",
            "D1 版本说明",
            "T3架构怎么设计",
            "D1版本说明",
            "GPT-5 怎么选模型",
            "天气",
            "今天天气真好",
            "天气怎么设计",
            "聊聊天气",
            "温度参数怎么设计",
        ] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, ToolLoopRoute::PlainChat, "{input}");
            assert_eq!(decision.semantic_route, SemanticRoute::PlainChat, "{input}");
            assert_eq!(decision.status_hint, None, "{input}");
        }
    }

    #[test]
    fn private_tool_intent_uses_tool_loop_when_tool_calling_enabled() {
        for input in [
            "删除第二条",
            "新增待办，明天接老公",
            "编辑第三条，其他不动",
            "记一下我喜欢少糖",
            "杭州天气",
            "杭州天气如何",
            "杭州明天要带伞吗",
            "明天会不会下雨",
            "查一下 G1 时刻",
            "G1 明天几点",
            "高铁 G1 时刻",
            "明天有没有g1，我想看看，如果有车，我要加个待办，是上海到北京么",
            "明天上海到北京有高铁吗，有的话提醒我",
            "如果明天下雨，帮我加个带伞的待办",
            "查看上次 codex 发布的 rss",
        ] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, ToolLoopRoute::CompleteToolLoop, "{input}");
            assert_eq!(decision.semantic_route, SemanticRoute::ToolLoop, "{input}");
            assert!(decision.status_hint.is_some(), "{input}");
        }
    }

    #[test]
    fn private_tool_intent_carries_status_hint_without_changing_route() {
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
                "完成第一条",
                StatusHint::new(StatusSubject::Todo, StatusAction::Confirm),
            ),
            (
                "查看待办有哪些",
                StatusHint::new(StatusSubject::Todo, StatusAction::Query),
            ),
            (
                "查看上次 codex 发布的 rss",
                StatusHint::new(StatusSubject::Rss, StatusAction::Query),
            ),
            (
                "查一下 G1 时刻",
                StatusHint::new(StatusSubject::Train, StatusAction::Query),
            ),
        ];

        for (input, expected_hint) in cases {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, ToolLoopRoute::CompleteToolLoop, "{input}");
            assert_eq!(decision.status_hint, Some(expected_hint), "{input}");
        }
    }

    #[test]
    fn ambiguous_private_defaults_to_tool_loop() {
        let decision = route_tool_loop(&request("明天别忘了"), context());
        assert_eq!(decision.route, ToolLoopRoute::CompleteToolLoop);
        assert_eq!(decision.semantic_route, SemanticRoute::Ambiguous);
        assert_eq!(decision.status_hint, Some(StatusHint::default_tool()));
    }

    #[test]
    fn disabled_or_group_request_keeps_plain_route() {
        let mut group = request("杭州明天要带伞吗");
        group.group_id = Some("g1".to_owned());
        assert_eq!(
            route_tool_loop(&group, context()).route,
            ToolLoopRoute::PlainChat
        );
        assert_eq!(
            route_tool_loop(
                &request("杭州明天要带伞吗"),
                ToolRouteContext {
                    scene_enabled: true,
                    tool_calling_enabled: false,
                    group_tool_calling_enabled: false,
                    provider_supports_tool_calling: true,
                    enabled_tools_available: true,
                },
            )
            .route,
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
            )
            .route,
            ToolLoopRoute::CompleteToolLoop
        );
    }

    #[test]
    fn group_plain_and_ambiguous_keep_plain_route_even_when_group_switch_enabled() {
        for input in ["晚上好", "写一段长文本测试流式", "那个帮我处理一下"] {
            assert_eq!(
                route_tool_loop(
                    &{
                        let mut group = request(input);
                        group.group_id = Some("g1".to_owned());
                        group
                    },
                    ToolRouteContext {
                        group_tool_calling_enabled: true,
                        ..context()
                    },
                )
                .route,
                ToolLoopRoute::PlainChat,
                "{input}"
            );
        }
    }

    #[test]
    fn disabled_scene_keeps_plain_route_even_when_tools_supported() {
        assert_eq!(
            route_tool_loop(
                &request("晚上好"),
                ToolRouteContext {
                    scene_enabled: false,
                    ..context()
                },
            )
            .route,
            ToolLoopRoute::PlainChat
        );
    }

    #[test]
    fn empty_enabled_tools_keep_plain_route() {
        assert_eq!(
            route_tool_loop(
                &request("杭州明天要带伞吗"),
                ToolRouteContext {
                    enabled_tools_available: false,
                    ..context()
                },
            )
            .route,
            ToolLoopRoute::PlainChat
        );
    }
}

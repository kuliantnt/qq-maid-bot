//! 普通消息 Tool Loop 前置路由。
//!
//! slash 命令、pending 和确定性 Todo 查询仍在更外层保持原有路径。这里仅判断
//! 普通聊天是否需要进入受控工具 Agent：明显闲聊、创作、解释和流式测试保留
//! 原生聊天路径；明确工具任务进入 Tool Loop；依赖 Todo 最近可见快照/最近操作
//! 的上下文续指由调用方传入 session 信号后再进入 Todo Tool Loop。

use qq_maid_common::time_context;

use super::{
    RespondRequest,
    interaction_state::{InteractionDomain, InteractionStateSnapshot},
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

#[derive(Debug, Clone, Copy)]
pub(super) struct ToolRouteDecision {
    pub route: ToolLoopRoute,
    pub semantic_route: SemanticRoute,
    pub domain: ToolDomain,
    pub reason: &'static str,
    pub status_hint: Option<StatusHint>,
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
const TODO_QUERY_MARKERS: &[&str] = &["查看", "看一下", "列出", "有哪些", "检查"];
const REMINDER_ACTION_MARKERS: &[&str] = &[
    "提醒我",
    "提醒一下",
    "提醒下",
    "帮我提醒",
    "回头提醒",
    "别忘",
    "别忘了",
];

pub(super) fn route_tool_loop(req: &RespondRequest, ctx: ToolRouteContext) -> ToolRouteDecision {
    if !ctx.scene_enabled
        || !ctx.tool_calling_enabled
        || !ctx.provider_supports_tool_calling
        || !ctx.enabled_tools_available
    {
        return decision(
            ToolLoopRoute::PlainChat,
            SemanticRoute::PlainChat,
            ToolDomain::Unknown,
            "tool_loop_unavailable",
            None,
        );
    }
    if req.has_non_text_input_parts() {
        return decision(
            ToolLoopRoute::PlainChat,
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
            ToolLoopRoute::PlainChat,
            SemanticRoute::Deterministic,
            ToolDomain::Unknown,
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
            ToolDomain::Unknown,
            "group_tool_loop_disabled",
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
            ToolLoopRoute::PlainChat,
            SemanticRoute::PlainChat,
            assessment.domain,
            assessment.reason,
            None,
        ),
        SemanticRoute::ToolLoop => decision(
            ToolLoopRoute::CompleteToolLoop,
            SemanticRoute::ToolLoop,
            assessment.domain,
            assessment.reason,
            status_hint,
        ),
        SemanticRoute::Deterministic => decision(
            ToolLoopRoute::PlainChat,
            SemanticRoute::Deterministic,
            assessment.domain,
            "deterministic_or_empty",
            None,
        ),
        SemanticRoute::Ambiguous if is_group => decision(
            ToolLoopRoute::PlainChat,
            SemanticRoute::Ambiguous,
            assessment.domain,
            "semantic_ambiguous_group_plain",
            None,
        ),
        SemanticRoute::Ambiguous => decision(
            ToolLoopRoute::PlainChat,
            SemanticRoute::Ambiguous,
            assessment.domain,
            assessment.reason,
            None,
        ),
    }
}

fn decision(
    route: ToolLoopRoute,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SemanticAssessment {
    semantic_route: SemanticRoute,
    domain: ToolDomain,
    reason: &'static str,
}

fn assessment(
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

    // 硬规则负责确定性工具请求；session/context 规则只处理 Todo 快照续指。
    // 剩余模糊场景后续可替换为轻量模型/主模型输出结构化路由，不继续扩散关键词表。
    if has_todo_intent(text, &lower) || has_reminder_intent(text) || has_scheduled_task_intent(text)
    {
        return assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Todo,
            "semantic_tool_intent",
        );
    }
    if is_strong_todo_reference_operation(text) {
        return assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Todo,
            "todo_reference_strong",
        );
    }
    if is_weak_todo_context_reference(text) && has_recent_todo_context {
        return assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Todo,
            "todo_reference_context",
        );
    }
    if is_bare_number_todo_operation(text) {
        if has_recent_todo_context {
            return assessment(
                SemanticRoute::ToolLoop,
                ToolDomain::Todo,
                "todo_number_context",
            );
        }
        return assessment(
            SemanticRoute::PlainChat,
            ToolDomain::Todo,
            "todo_number_context_missing",
        );
    }
    if has_memory_intent(text, &lower) {
        return assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Memory,
            "semantic_tool_intent",
        );
    }
    if has_weather_intent(text, &lower) {
        return assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Weather,
            "semantic_tool_intent",
        );
    }
    if has_train_intent(text, &lower) {
        return assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Train,
            "semantic_tool_intent",
        );
    }
    if has_rss_intent(text, &lower) {
        return assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Rss,
            "semantic_tool_intent",
        );
    }
    if has_search_intent(text, &lower) {
        return assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Search,
            "semantic_tool_intent",
        );
    }

    if mentions_inert_weather_topic(text) {
        return assessment(
            SemanticRoute::PlainChat,
            ToolDomain::Unknown,
            "semantic_plain_chat",
        );
    }

    if has_plain_chat_intent(text, &lower) {
        return assessment(
            SemanticRoute::PlainChat,
            ToolDomain::Unknown,
            "semantic_plain_chat",
        );
    }

    if is_weak_todo_context_reference(text) {
        return assessment(
            SemanticRoute::PlainChat,
            ToolDomain::Todo,
            "todo_reference_context_missing",
        );
    }

    if has_ambiguous_toolish_intent(text) {
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
    if has_reminder_intent(text) || has_scheduled_task_intent(text) {
        return Some(StatusHint::new(StatusSubject::Todo, StatusAction::Write));
    }
    if has_todo_intent(text, &lower)
        || is_strong_todo_reference_operation(text)
        || (has_recent_todo_context && is_weak_todo_context_reference(text))
        || (has_recent_todo_context && is_bare_number_todo_operation(text))
    {
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
    if has_search_intent(text, &lower) {
        return Some(StatusHint::new(StatusSubject::Tool, StatusAction::Query));
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
    if has_reminder_action(text) && !has_reminder_intent(text) {
        return false;
    }

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

fn has_reminder_intent(text: &str) -> bool {
    has_reminder_action(text)
        && (looks_like_temporal_expression(text) || has_reminder_payload(text))
}

fn has_reminder_action(text: &str) -> bool {
    contains_any(text, REMINDER_ACTION_MARKERS)
}

fn has_scheduled_task_intent(text: &str) -> bool {
    if has_plain_chat_intent(text, &text.to_ascii_lowercase())
        || !looks_like_temporal_expression(text)
    {
        return false;
    }
    let compact = text.split_whitespace().collect::<String>();
    let action_markers = [
        "盯一下",
        "盯下",
        "看一下",
        "看下",
        "检查",
        "跟进",
        "开会",
        "提交",
        "出一版",
        "复盘",
        "续费",
        "验收",
        "整理",
        "发送",
        "发给",
        "发一下",
        "发布",
        "发版",
        "交水电费",
        "交作业",
        "买菜",
        "买东西",
        "买药",
        "完成初稿",
        "完成草稿",
    ];
    contains_any(&compact, &action_markers)
}

fn looks_like_temporal_expression(text: &str) -> bool {
    // 路由层只判断“是否存在时间线索”，不消费推断出的日期，也不改变 Todo Tool
    // 内部基于模型/时间上下文生成的最终 due_date/due_at。
    let ctx = time_context::request_time_context();
    let compact = text.split_whitespace().collect::<String>();
    if time_context::infer_due_date_from_text(text, &ctx).is_some()
        || compact != text && time_context::infer_due_date_from_text(&compact, &ctx).is_some()
    {
        return true;
    }
    contains_any(
        text,
        &[
            "今晚",
            "明早",
            "明晚",
            "早上",
            "上午",
            "中午",
            "下午",
            "晚上",
            "凌晨",
            "傍晚",
            "回头",
            "月末",
            "下个月",
        ],
    )
}

fn has_reminder_payload(text: &str) -> bool {
    let mut payload = text.to_owned();
    for marker in REMINDER_ACTION_MARKERS {
        payload = payload.replace(marker, "");
    }
    // 只清掉明显的提示/时间壳，剩余内容仍交给 Todo Tool 解析和澄清。
    for filler in [
        "帮我",
        "请",
        "麻烦",
        "一下",
        "一下子",
        "到时候",
        "记得",
        "记着",
        "回头",
        "今天",
        "明天",
        "后天",
        "今晚",
        "明早",
        "明晚",
        "早上",
        "上午",
        "中午",
        "下午",
        "晚上",
        "凌晨",
        "傍晚",
        "月底",
        "月末",
        "下个月初",
        "下个月",
        "周一",
        "周二",
        "周三",
        "周四",
        "周五",
        "周六",
        "周日",
        "星期一",
        "星期二",
        "星期三",
        "星期四",
        "星期五",
        "星期六",
        "星期日",
    ] {
        payload = payload.replace(filler, "");
    }
    let meaningful = payload.trim_matches(|ch: char| {
        ch.is_whitespace() || ch.is_ascii_punctuation() || is_cjk_punctuation(ch)
    });
    meaningful.chars().count() >= 2
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

pub(super) fn has_search_intent(text: &str, lower: &str) -> bool {
    if has_local_text_processing_intent(text, lower) {
        return false;
    }

    lower.contains("search")
        || has_explicit_search_phrase(text)
        || contains_any(
            text,
            &[
                "联网",
                "上网查",
                "网上查",
                "网络查询",
                "搜索",
                "搜一下",
                "网上有没有",
                "查 GitHub",
                "查 github",
                "查资料",
                "查新闻",
                "最新的",
                "最新消息",
                "最新进展",
            ],
        )
}

fn has_explicit_search_phrase(text: &str) -> bool {
    contains_any(text, &["查一下", "查下", "查查", "查询一下"])
        && contains_any(
            text,
            &[
                "新闻",
                "资料",
                "网上",
                "网络",
                "互联网",
                "GitHub",
                "github",
                "最新",
                "进展",
                "有没有",
            ],
        )
}

fn has_local_text_processing_intent(text: &str, _lower: &str) -> bool {
    let Some(instruction) = local_text_processing_instruction(text) else {
        return false;
    };
    let instruction_lower = instruction.to_ascii_lowercase();
    if has_explicit_online_search_marker(instruction, &instruction_lower) {
        return false;
    }

    // 长粘贴内容里的“查询 / Search / Tool”等词只描述待处理文本，
    // 路由以末尾短指令为准，避免文本整理请求误入 WebSearch。
    contains_any(
        instruction,
        &[
            "人话说这个",
            "说人话",
            "人话说",
            "总结这段",
            "总结一下",
            "总结下",
            "整理一下",
            "整理下",
            "改写一下",
            "改写下",
            "润色一下",
            "润色下",
            "压缩成",
            "压缩到",
            "解释一下",
            "解释下",
            "翻译一下",
            "翻译下",
            "这段是什么意思",
            "是什么意思",
            "说简单点",
            "简单点",
            "整理成 issue",
            "整理成任务书",
            "整理成 Codex prompt",
            "整理成 prompt",
            "上面这段",
            "这段话",
            "这段文本",
            "这段内容",
            "哪里不通顺",
            "不通顺",
            "语病",
            "病句",
        ],
    )
}

fn local_text_processing_instruction(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= 80 {
        return Some(trimmed);
    }

    trimmed
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty() && line.chars().count() <= 80)
}

fn has_explicit_online_search_marker(text: &str, _lower: &str) -> bool {
    contains_any(
        text,
        &[
            "联网",
            "上网查",
            "网上查",
            "网上有没有",
            "网络查询",
            "搜索",
            "搜一下",
            "查 GitHub",
            "查 github",
            "查资料",
            "查新闻",
            "最新消息",
            "最新进展",
        ],
    )
}

fn has_plain_chat_intent(text: &str, lower: &str) -> bool {
    let compact = text.split_whitespace().collect::<String>();
    is_plain_greeting(&compact)
        || matches!(lower.trim(), "hi" | "hello" | "hey")
        || contains_any(
            text,
            &[
                "陪我聊",
                "聊会",
                "闲聊",
                "说说话",
                "聊聊天",
                "有点烦",
                "有点累",
                "不开心",
                "你下午在吗",
                "你晚上在吗",
            ],
        )
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

fn is_plain_greeting(compact: &str) -> bool {
    matches!(compact, "你好" | "您好" | "你在吗" | "在吗")
        || ["晚上好", "早上好", "上午好", "中午好", "下午好"]
            .iter()
            .any(|greeting| {
                compact == *greeting
                    || compact.strip_prefix(greeting).is_some_and(|suffix| {
                        matches!(suffix, "呀" | "啊" | "哦" | "喔" | "哈" | "～" | "~")
                    })
            })
}

fn is_strong_todo_reference_operation(text: &str) -> bool {
    let has_reference = has_ordinal_reference(text) || has_context_pronoun_reference(text);
    if !has_reference {
        return false;
    }

    let has_lifecycle_action = contains_any(text, TODO_CONFIRM_MARKERS);
    let has_numbered_edit_or_process =
        has_ordinal_reference(text) && contains_any(text, &["处理", "改一下", "修改", "编辑"]);
    has_lifecycle_action || has_numbered_edit_or_process
}

fn is_weak_todo_context_reference(text: &str) -> bool {
    (has_context_pronoun_reference(text)
        && contains_any(text, &["处理", "改一下", "修改", "编辑", "改成"]))
        || is_bulk_todo_context_reference(text)
}

fn is_bulk_todo_context_reference(text: &str) -> bool {
    contains_any(text, &["都", "全部", "全"]) && contains_any(text, TODO_CONFIRM_MARKERS)
}

fn is_bare_number_todo_operation(text: &str) -> bool {
    let compact = text.split_whitespace().collect::<String>();
    has_ascii_digit(&compact)
        && (contains_any(&compact, TODO_CONFIRM_MARKERS)
            || contains_any(&compact, &["删", "清掉", "作废", "合并"]))
}

fn has_ascii_digit(text: &str) -> bool {
    text.bytes().any(|byte| byte.is_ascii_digit())
}

fn has_ordinal_reference(text: &str) -> bool {
    contains_any(
        text,
        &[
            "第一", "第二", "第三", "第四", "第五", "第六", "第七", "第八", "第九", "第十", "第 1",
            "第 2", "第 3", "第 4", "第 5", "第 6", "第 7", "第 8", "第 9", "第1", "第2", "第3",
            "第4", "第5", "第6", "第7", "第8", "第9",
        ],
    )
}

fn has_context_pronoun_reference(text: &str) -> bool {
    contains_any(
        text,
        &[
            "它",
            "这个",
            "那个",
            "这条",
            "那条",
            "这些",
            "它们",
            "刚才那条",
            "刚刚那条",
            "刚才那个",
            "刚刚那个",
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
            "早上好",
            "我晚上有点累",
            "你下午在吗",
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
        let issue_output = "\
执行报告：
1. 查询 router
2. WebSearch 工具进入错误分支
3. 输出太长
帮我整理成 issue";

        for input in [codex_output, log_output, issue_output] {
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
            "帮我看看网上有没有 rust async sink 相关资料",
            "查 GitHub 上有没有相关 issue",
            "最新的 Rust 版本是什么",
        ] {
            let lower = input.to_ascii_lowercase();
            assert!(has_search_intent(input, &lower), "{input}");
        }
    }

    #[test]
    fn scheduled_task_negative_phrases_do_not_enter_tool_loop() {
        for input in [
            "下午发烧了怎么办",
            "今天买的东西怎么保存",
            "周四确认这个方案为什么不行",
            "晚上发朋友圈文案怎么写",
        ] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, ToolLoopRoute::PlainChat, "{input}");
            assert_ne!(decision.semantic_route, SemanticRoute::ToolLoop, "{input}");
            assert_eq!(decision.status_hint, None, "{input}");
        }
    }

    #[test]
    fn private_tool_intent_uses_tool_loop_when_tool_calling_enabled() {
        for input in [
            "删除第二条",
            "处理第一项",
            "处理第一条",
            "把第一条改一下",
            "新增待办，明天接老公",
            "编辑第三条，其他不动",
            "记一下我喜欢少糖",
            "别忘了买菜",
            "别忘了交水电费",
            "提醒我续费",
            "帮我提醒一下检查日志",
            "回头提醒我检查日志",
            "明天别忘了买菜",
            "晚上提醒我提交周报",
            "下午检查发布清单",
            "明天上午整理方案",
            "周四项目 A 完成初稿",
            "周四下午项目 A 完成初稿",
            "下周五提醒我验收",
            "周五别忘了开会",
            "月底提醒我续费",
            "月末提醒我续费",
            "下个月初提醒我看账单",
            "7 月 10 号提醒我验收",
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
    fn ambiguous_private_defaults_to_plain_chat() {
        for input in ["安排一下", "帮我处理一下", "刚刚没看到，再来一条"] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, ToolLoopRoute::PlainChat, "{input}");
            assert_eq!(decision.semantic_route, SemanticRoute::Ambiguous, "{input}");
            assert_eq!(decision.reason, "semantic_ambiguous_plain", "{input}");
            assert_eq!(decision.status_hint, None, "{input}");
        }
    }

    #[test]
    fn private_todo_reference_routes_by_strength_and_recent_context() {
        for input in [
            "完成第一条",
            "处理第一项",
            "取消它",
            "删掉这个",
            "这些完成了",
        ] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, ToolLoopRoute::CompleteToolLoop, "{input}");
            assert_eq!(decision.semantic_route, SemanticRoute::ToolLoop, "{input}");
            assert_eq!(decision.domain, ToolDomain::Todo, "{input}");
        }

        for input in ["这个改一下", "都删除了吧"] {
            let weak_without_context = route_tool_loop(&request(input), context());
            assert_eq!(
                weak_without_context.route,
                ToolLoopRoute::PlainChat,
                "{input}"
            );
            assert_eq!(
                weak_without_context.semantic_route,
                SemanticRoute::PlainChat,
                "{input}"
            );
            assert_eq!(
                weak_without_context.reason, "todo_reference_context_missing",
                "{input}"
            );
        }

        for input in ["这个改一下", "都删除了吧"] {
            let weak_with_context = route_tool_loop(&request(input), context_with_recent_todo());
            assert_eq!(
                weak_with_context.route,
                ToolLoopRoute::CompleteToolLoop,
                "{input}"
            );
            assert_eq!(
                weak_with_context.semantic_route,
                SemanticRoute::ToolLoop,
                "{input}"
            );
            assert_eq!(weak_with_context.domain, ToolDomain::Todo, "{input}");
        }
    }

    #[test]
    fn bare_number_todo_operations_require_recent_context() {
        for input in [
            "7删除",
            "删除7",
            "7取消",
            "取消7",
            "7完成",
            "把7合并到6",
            "6和7合并",
        ] {
            let without_context = route_tool_loop(&request(input), context());
            assert_eq!(without_context.route, ToolLoopRoute::PlainChat, "{input}");
            assert_eq!(
                without_context.reason, "todo_number_context_missing",
                "{input}"
            );

            let with_context = route_tool_loop(&request(input), context_with_recent_todo());
            assert_eq!(
                with_context.route,
                ToolLoopRoute::CompleteToolLoop,
                "{input}"
            );
            assert_eq!(
                with_context.semantic_route,
                SemanticRoute::ToolLoop,
                "{input}"
            );
            assert_eq!(with_context.domain, ToolDomain::Todo, "{input}");
        }
    }

    #[test]
    fn bare_numbers_without_todo_action_do_not_route_to_tools() {
        for input in [
            "407 笑死我了",
            "T3 架构怎么设计",
            "D1 版本说明",
            "2026 年计划",
        ] {
            let decision = route_tool_loop(&request(input), context_with_recent_todo());
            assert_eq!(decision.route, ToolLoopRoute::PlainChat, "{input}");
            assert_ne!(decision.domain, ToolDomain::Todo, "{input}");
        }
    }

    #[test]
    fn plain_text_generation_without_tool_affordance_keeps_plain_chat() {
        for input in [
            "能不能给我发一条，三行的信息",
            "刚刚没看到，再来一条",
            "帮我写个文案",
            "解释一下这个问题",
            "我好烦，陪我聊会",
        ] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, ToolLoopRoute::PlainChat, "{input}");
            assert_ne!(decision.reason, "semantic_tool_intent", "{input}");
            assert_ne!(decision.semantic_route, SemanticRoute::ToolLoop, "{input}");
            assert_eq!(decision.status_hint, None, "{input}");
        }

        for input in ["能不能给我发一条，三行的信息", "刚刚没看到，再来一条"]
        {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.semantic_route, SemanticRoute::Ambiguous, "{input}");
        }
    }

    #[test]
    fn enabled_tools_alone_do_not_force_tool_loop() {
        let decision = route_tool_loop(&request("安排一下"), context());
        assert_eq!(decision.route, ToolLoopRoute::PlainChat);
        assert_eq!(decision.semantic_route, SemanticRoute::Ambiguous);
    }

    #[test]
    fn provider_tool_support_alone_do_not_force_tool_loop() {
        let decision = route_tool_loop(&request("普通说两句"), context());
        assert_eq!(decision.route, ToolLoopRoute::PlainChat);
        assert_eq!(decision.semantic_route, SemanticRoute::Ambiguous);
    }

    #[test]
    fn reminder_without_date_but_with_payload_routes_to_todo_write_tool_loop() {
        for input in [
            "别忘了买菜",
            "别忘了交水电费",
            "提醒我续费",
            "帮我提醒一下检查日志",
            "回头提醒我检查日志",
        ] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, ToolLoopRoute::CompleteToolLoop, "{input}");
            assert_eq!(decision.semantic_route, SemanticRoute::ToolLoop, "{input}");
            assert_eq!(decision.domain, ToolDomain::Todo, "{input}");
            assert_eq!(
                decision.status_hint,
                Some(StatusHint::new(StatusSubject::Todo, StatusAction::Write)),
                "{input}"
            );
        }
    }

    #[test]
    fn reminder_with_date_still_routes_to_todo_write_tool_loop() {
        for input in [
            "明天别忘了",
            "明天别忘了买菜",
            "下周五提醒我验收",
            "月底提醒我续费",
        ] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, ToolLoopRoute::CompleteToolLoop, "{input}");
            assert_eq!(decision.semantic_route, SemanticRoute::ToolLoop, "{input}");
            assert_eq!(decision.domain, ToolDomain::Todo, "{input}");
            assert_eq!(
                decision.status_hint,
                Some(StatusHint::new(StatusSubject::Todo, StatusAction::Write)),
                "{input}"
            );
        }
    }

    #[test]
    fn reminder_action_without_date_or_payload_stays_ambiguous_plain() {
        for input in ["别忘了", "提醒我"] {
            let decision = route_tool_loop(&request(input), context());
            assert_eq!(decision.route, ToolLoopRoute::PlainChat, "{input}");
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
                    interaction_state: InteractionStateSnapshot::default(),
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

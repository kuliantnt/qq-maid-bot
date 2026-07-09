//! 非 Todo 工具的普通消息轻量意图判定。
//!
//! 这些规则目前仍是 respond 路由胶水的一部分：它们只决定是否进入受控 Tool Loop
//! 或显式 WebSearch，不执行具体工具业务。后续某个工具域出现更多上下文/状态规则时，
//! 再按 Todo 的方式下沉到对应 `runtime/tools/<domain>/`。

use super::{
    status_hint::{StatusAction, StatusHint, StatusSubject},
    tool_route::{SemanticAssessment, SemanticRoute, ToolDomain, assessment},
};

pub(super) fn classify_non_todo_route(text: &str, lower: &str) -> Option<SemanticAssessment> {
    if has_memory_intent(text, lower) {
        return Some(assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Memory,
            "semantic_tool_intent",
        ));
    }
    if has_weather_intent(text, lower) {
        return Some(assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Weather,
            "semantic_tool_intent",
        ));
    }
    if has_train_intent(text, lower) {
        return Some(assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Train,
            "semantic_tool_intent",
        ));
    }
    if has_rss_intent(text, lower) {
        return Some(assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Rss,
            "semantic_tool_intent",
        ));
    }
    if has_search_intent(text, lower) {
        return Some(assessment(
            SemanticRoute::ToolLoop,
            ToolDomain::Search,
            "semantic_tool_intent",
        ));
    }
    None
}

pub(super) fn classify_non_todo_status_hint(text: &str, lower: &str) -> Option<StatusHint> {
    if has_memory_intent(text, lower) {
        return Some(StatusHint::new(StatusSubject::Record, StatusAction::Read));
    }
    if has_weather_intent(text, lower) {
        return Some(StatusHint::new(StatusSubject::Weather, StatusAction::Query));
    }
    if has_train_intent(text, lower) {
        return Some(StatusHint::new(StatusSubject::Train, StatusAction::Query));
    }
    if has_rss_intent(text, lower) {
        return Some(StatusHint::new(StatusSubject::Rss, StatusAction::Query));
    }
    if has_search_intent(text, lower) {
        return Some(StatusHint::new(StatusSubject::Tool, StatusAction::Query));
    }
    None
}

pub(super) fn mentions_inert_weather_topic(text: &str) -> bool {
    contains_any(text, &["天气", "气温", "温度"]) && !has_weather_intent(text, "")
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

pub(super) fn has_plain_chat_intent(text: &str, lower: &str) -> bool {
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

pub(super) fn has_ambiguous_toolish_intent(text: &str) -> bool {
    contains_any(
        text,
        &["安排一下", "处理一下", "帮我处理", "别忘了", "回头提醒"],
    )
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

//! 普通聊天与通用弱意图路由 helper。
//!
//! 这里保留不属于具体工具域的判断：闲聊/创作/解释类请求、长文本本地整理请求，
//! 以及无法归属具体工具的弱工具感表达。具体工具关键词应放在对应 domain route。

pub(super) fn has_non_tool_status_context(text: &str, lower: &str) -> bool {
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

pub(super) fn has_local_text_processing_intent(text: &str) -> bool {
    let Some(instruction) = local_text_processing_instruction(text) else {
        return false;
    };

    // 长粘贴内容里的“查询 / Search / Tool”等词只描述待处理文本；搜索域会先
    // 尊重这里识别出的末尾整理指令，避免正文里的明确句式误触发搜索预提示。
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

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

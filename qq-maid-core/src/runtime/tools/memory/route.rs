//! Memory 自然语言写入意图与服务端范围推断。

use super::MemoryKind;

const NEGATED_WRITE_MARKERS: &[&str] = &[
    "不要记住",
    "别记住",
    "不用记住",
    "不需要记住",
    "不要保存",
    "别保存",
    "不要写入记忆",
];

/// 明确否定写入时拒绝执行，避免模型误调用产生副作用。
///
/// 正向自然语言能力由 Luna 根据 Tool 描述判断，不能由固定短语列表定义。
pub(crate) fn is_memory_write_explicitly_negated(text: &str) -> bool {
    let normalized = text.trim().to_ascii_lowercase();
    NEGATED_WRITE_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
}

/// 群聊自然语言写入的保守定域规则；无法可靠判断时必须澄清。
pub(crate) fn infer_group_memory_kind(text: &str) -> Option<MemoryKind> {
    let compact = text.split_whitespace().collect::<String>();
    let profile_context = ["在这个群", "在本群", "这个群里", "本群里", "群里"]
        .iter()
        .any(|marker| compact.contains(marker));
    let profile_subject = [
        "叫我",
        "称呼我",
        "不要叫我",
        "我的昵称",
        "我的身份",
        "我的角色",
        "我的人设",
        "我是",
    ]
    .iter()
    .any(|marker| compact.contains(marker));
    if profile_context && profile_subject {
        return Some(MemoryKind::GroupProfile);
    }
    if [
        "群规",
        "群公告",
        "共同约定",
        "这个群每",
        "本群每",
        "这个群的",
        "本群的",
        "我们约定",
        "项目状态",
        "群项目",
    ]
    .iter()
    .any(|marker| compact.contains(marker))
    {
        return Some(MemoryKind::Group);
    }
    if [
        "我喜欢",
        "我不喜欢",
        "我希望你",
        "以后回复我",
        "我的偏好",
        "个人偏好",
        "只在私聊",
    ]
    .iter()
    .any(|marker| compact.contains(marker))
    {
        return Some(MemoryKind::Personal);
    }
    None
}

/// 只为用户可见状态识别明确的记忆写入请求；真实保存仍以 `save_memory` Tool 为准。
pub(crate) fn has_memory_status_intent(text: &str) -> bool {
    !is_memory_write_explicitly_negated(text)
        && ["记一下", "记住", "帮我记", "记录一下", "保存一下"]
            .iter()
            .any(|needle| text.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_negation_is_only_a_safety_fallback() {
        for text in ["不要记住这句话", "别保存这个 token"] {
            assert!(is_memory_write_explicitly_negated(text), "{text}");
        }
        for text in [
            "记住我喜欢简短回复",
            "把这个作为我的长期偏好保存下来",
            "我最近在学 Rust",
        ] {
            assert!(!is_memory_write_explicitly_negated(text), "{text}");
        }
    }

    #[test]
    fn memory_status_requires_an_explicit_write_request() {
        for text in [
            "记住我喜欢简短回复",
            "帮我记一下这个偏好",
            "保存一下这条信息",
        ] {
            assert!(has_memory_status_intent(text), "{text}");
        }
        for text in [
            "Memory 怎么设计",
            "解释一下记忆系统",
            "这段内容是什么",
            "不要记住这句话",
        ] {
            assert!(!has_memory_status_intent(text), "{text}");
        }
    }
}

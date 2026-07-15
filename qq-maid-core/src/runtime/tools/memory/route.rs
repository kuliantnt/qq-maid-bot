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

const GROUP_MEMORY_COMMAND_ONLY_MARKERS: &[&str] =
    &["群里记一下", "记到这个群", "作为群记忆", "这是本群规则"];

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
    if contains_group_memory_command_only_marker(&compact) {
        return Some(MemoryKind::Group);
    }
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

/// 群公共记忆只允许通过 `/memory` 确定性命令维护。
///
/// 这里仅识别普通聊天原文；slash 命令继续交给既有命令管理流程。
pub(crate) fn is_group_memory_command_only_intent(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.starts_with('/') || trimmed.starts_with('／') {
        return false;
    }
    let compact = trimmed.split_whitespace().collect::<String>();
    contains_group_memory_command_only_marker(&compact)
        || (has_memory_intent(trimmed, &trimmed.to_ascii_lowercase())
            && infer_group_memory_kind(trimmed) == Some(MemoryKind::Group))
}

fn contains_group_memory_command_only_marker(compact: &str) -> bool {
    GROUP_MEMORY_COMMAND_ONLY_MARKERS
        .iter()
        .any(|marker| compact.contains(marker))
}

pub(crate) fn has_memory_intent(text: &str, lower: &str) -> bool {
    lower.contains("memory")
        || ["记忆", "记一下", "记住", "帮我记", "记录一下", "保存一下"]
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
    fn group_memory_write_intent_is_command_only() {
        for text in [
            "群里记一下，周五开会",
            "记到这个群：周五开会",
            "把周五开会作为群记忆",
            "这是本群规则：不要刷屏",
            "记住这个群每周五开周会",
        ] {
            assert!(is_group_memory_command_only_intent(text), "{text}");
        }
        assert!(!is_group_memory_command_only_intent(
            "/memory group add 周五开会"
        ));
        assert!(!is_group_memory_command_only_intent("以后在这个群叫我棒冰"));
    }
}

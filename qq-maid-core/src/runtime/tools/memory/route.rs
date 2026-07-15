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

/// 仅识别用户明确要求写入长期记忆的表达。
///
/// 该判断只控制 Tool 是否可见；模型仍需遵守 Tool 描述，执行时也会再次校验，
/// 最终范围、权限和写入结果统一由服务端 Memory 领域决定。
pub(crate) fn has_explicit_memory_write_intent(text: &str) -> bool {
    let normalized = text.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || NEGATED_WRITE_MARKERS
            .iter()
            .any(|marker| normalized.contains(marker))
    {
        return false;
    }

    [
        "记住",
        "帮我记",
        "请记",
        "记一下",
        "记录一下",
        "保存到记忆",
        "写入记忆",
        "加入记忆",
        "在这个群叫我",
        "在本群叫我",
        "这个群里叫我",
        "本群里叫我",
        "群里叫我",
        "以后叫我",
        "remember ",
        "remember that",
        "remember me as",
    ]
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
    fn explicit_write_intent_rejects_plain_statements_and_negation() {
        for text in ["我最近在学 Rust", "我今天去了杭州", "你还记得我吗"] {
            assert!(!has_explicit_memory_write_intent(text), "{text}");
        }
        for text in ["不要记住这句话", "别保存这个 token"] {
            assert!(!has_explicit_memory_write_intent(text), "{text}");
        }
        for text in [
            "记住我喜欢简短回复",
            "在这个群叫我棒冰",
            "请记一下我常用 Rust",
        ] {
            assert!(has_explicit_memory_write_intent(text), "{text}");
        }
    }
}

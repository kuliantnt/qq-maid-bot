use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

/// 匹配用户消息中成员编号自称的正则。
///
/// 匹配模式如 "我是407"、"编号 123 来了" 等。
static MEMBER_MENTION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[，,。.!！?？\s：:])(?:我是|这里是|这边是|我这边是|我是编号|编号是)?\s*([1-9]\d{2})(?:\s*(?:来了|在|报到|上线))?(?:$|[，,。.!！?？\s])",
    )
    .unwrap()
});

/// 从文本中匹配到的成员编号及对应信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberIdMatch {
    /// 成员编号（三位数字）
    pub member_id: String,
    /// 成员昵称
    pub name: Option<String>,
    /// 成员描述/设定
    pub profile: Option<String>,
}

/// 成员映射类型：(成员编号, 名称, 描述) 的三元组列表。
pub type MemberMapping = Vec<(String, String, String)>;

/// 将 JSON 格式的成员编号映射归一化为标准的三元组列表。
///
/// 支持两种 JSON 格式：
/// - 字符串值：`"407": "名称：描述"`
/// - 对象值：`"407": {"name": "名称", "profile": "描述"}`
pub fn normalize_member_mapping(value: &Value) -> MemberMapping {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut mapping = Vec::new();
    for (member_id, raw) in object {
        if !is_member_id(member_id) {
            continue;
        }
        if let Some(text) = raw.as_str() {
            let (name, profile) = split_member_text(text);
            if !name.is_empty() {
                mapping.push((member_id.clone(), name, profile));
            }
            continue;
        }
        if let Some(item) = raw.as_object() {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            let profile = item
                .get("profile")
                .or_else(|| item.get("description"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_owned();
            if !name.is_empty() {
                mapping.push((member_id.clone(), name, profile));
            }
        }
    }
    mapping.sort_by(|left, right| left.0.cmp(&right.0));
    mapping
}

/// 根据成员映射生成系统提示文本，告知 LLM 成员编号对应的身份信息。
pub fn build_member_id_mapping_prompt(mapping: &MemberMapping) -> Option<String> {
    let rows = mapping
        .iter()
        .filter(|(_, name, _)| !name.trim().is_empty())
        .map(|(member_id, name, profile)| {
            let description = if profile.trim().is_empty() {
                String::new()
            } else {
                format!("：{}", profile.trim())
            };
            format!("- {member_id} = {}{description}", name.trim())
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    Some(format!(
        "成员编号映射来自外部配置文件。当当前用户消息出现成员编号或明确自称时，优先使用这些配置判断当前说话者；不要重新发明编号含义，也不要仅凭上一轮前台默认延续：\n{}",
        rows.join("\n")
    ))
}

/// 从文本中查找所有成员编号提及，并匹配映射中的身份信息。
///
/// 使用正则匹配三位数编号，去重后返回匹配结果。
pub fn find_member_id_mentions(text: &str, mapping: &MemberMapping) -> Vec<MemberIdMatch> {
    let mut seen = Vec::<String>::new();
    let mut matches = Vec::new();
    for capture in MEMBER_MENTION_PATTERN.captures_iter(text.trim()) {
        let Some(member_id) = capture.get(1).map(|item| item.as_str().to_owned()) else {
            continue;
        };
        if seen.contains(&member_id) {
            continue;
        }
        seen.push(member_id.clone());
        let member = mapping.iter().find(|(id, _, _)| id == &member_id);
        matches.push(MemberIdMatch {
            member_id,
            name: member.map(|(_, name, _)| name.clone()),
            profile: member.map(|(_, _, profile)| profile.clone()),
        });
    }
    matches
}

/// 生成未知成员编号的回复，如果存在相似编号则给出提示建议。
pub fn unknown_member_id_reply(member_id: &str, mapping: &MemberMapping) -> String {
    let suggestion = suggest_member_id(member_id, mapping)
        .map(|(id, name)| format!("你是想说 {id} {name}，还是"))
        .unwrap_or_default();
    format!("当前编号映射里没有 {member_id}。是不是写错了？{suggestion}需要补充一个新成员？")
}

/// 根据匹配到的成员编号列表，构建本轮对话的身份上下文提示。
pub fn build_member_identity_context(matches: &[MemberIdMatch]) -> Option<String> {
    let rows = matches
        .iter()
        .filter_map(|item| {
            let name = item.name.as_deref()?;
            let description = item
                .profile
                .as_deref()
                .filter(|profile| !profile.trim().is_empty())
                .map(|profile| format!("：{}", profile.trim()))
                .unwrap_or_default();
            Some(format!(
                "- {} = {}{description}",
                item.member_id,
                name.trim()
            ))
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }
    Some(format!(
        "本轮用户消息命中了已知成员编号。判断当前说话者时，请优先按以下身份理解；如命中多个编号，可以理解为多人同时前台，不要重新发明编号含义：\n{}",
        rows.join("\n")
    ))
}

/// 根据后缀（末两位）匹配相似成员编号，用于纠错提示。
fn suggest_member_id(member_id: &str, mapping: &MemberMapping) -> Option<(String, String)> {
    let suffix = member_id.get(member_id.len().saturating_sub(2)..)?;
    mapping
        .iter()
        .find(|(candidate_id, _, _)| candidate_id != member_id && candidate_id.ends_with(suffix))
        .map(|(id, name, _)| (id.clone(), name.clone()))
}

/// 以中文冒号分割成员信息文本，返回 (名称, 描述)。
fn split_member_text(text: &str) -> (String, String) {
    let mut parts = text.splitn(2, '：');
    let name = parts.next().unwrap_or("").trim().to_owned();
    let profile = parts.next().unwrap_or("").trim().to_owned();
    (name, profile)
}

/// 判断字符串是否为合法成员编号（三位数字，首位非零）。
fn is_member_id(value: &str) -> bool {
    value.len() == 3
        && value
            .chars()
            .next()
            .is_some_and(|ch| matches!(ch, '1'..='9'))
        && value.chars().all(|ch| ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_mentions_use_external_mapping() {
        let mapping = normalize_member_mapping(&serde_json::json!({
            "407": {"name": "测试成员", "profile": "示例成员设定"},
            "507": {"name": "另一个", "profile": ""}
        }));

        let matches = find_member_id_mentions("我是407", &mapping);

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name.as_deref(), Some("测试成员"));
        assert!(unknown_member_id_reply("507", &mapping).contains("507"));
    }
}

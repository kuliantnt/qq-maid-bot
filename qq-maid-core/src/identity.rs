//! 跨平台身份与业务隔离键 helper。
//!
//! 本模块只负责生成和解析业务隔离键，不负责平台投递目标。几个术语需要分清：
//!
//! - conversation scope：消息发生的对话空间，用于 session、ref_index 等会话状态隔离；
//! - actor scope：对话空间内的实际操作者，用于权限、审计和群聊内个人交互归属；
//! - interaction scope：一次个人交互状态的隔离键，例如 pending 和 visible snapshot；
//! - owner scope：Todo / Memory 等业务数据归属；
//! - delivery target：平台真实投递目标，必须由 ReplyTarget / DeliveryTarget 等发送链路携带。
//!
//! 这里生成的 key 可包含平台和账号命名空间，但仍然是业务 key。发送阶段不得把它们当作
//! raw openid / group_id 反解析出平台投递目标。

const UNKNOWN_ACCOUNT: &str = "-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedScopeKey<'a> {
    pub platform: &'a str,
    pub account_id: &'a str,
    pub target_type: &'a str,
    pub raw_target_id: &'a str,
}

/// 构造 conversation scope。
///
/// 该 key 描述“消息发生在哪个对话空间”。私聊和群聊即使 actor 相同，也必须生成不同
/// conversation scope，避免 session / pending / visible snapshot / ref_index 串用。
pub fn conversation_scope_key(
    platform: &str,
    account_id: Option<&str>,
    target_type: &str,
    raw_target_id: &str,
) -> String {
    let platform = clean_segment(platform).unwrap_or("unknown");
    let account = account_id
        .and_then(clean_segment)
        .unwrap_or(UNKNOWN_ACCOUNT);
    let target_type = clean_segment(target_type).unwrap_or("unknown");
    let target = raw_target_id.trim();
    format!("platform:{platform}:account:{account}:{target_type}:{target}")
}

/// 兼容旧命名的 stable scope 构造入口。
///
/// 新代码优先使用 [`conversation_scope_key`] 表达语义；保留该函数是为了稳定既有调用点
/// 和测试中的历史术语。
pub fn stable_scope_key(
    platform: &str,
    account_id: Option<&str>,
    target_type: &str,
    raw_target_id: &str,
) -> String {
    conversation_scope_key(platform, account_id, target_type, raw_target_id)
}

/// 构造 actor scope。
///
/// actor scope 描述“当前对话空间里的谁”，不是独立会话空间，也不是发送目标。
pub fn actor_scope_key(user_id: Option<&str>, scope_key: &str) -> Option<String> {
    let scope_key = clean_string(scope_key).unwrap_or_else(|| "unknown".to_owned());
    user_id
        .and_then(clean_string)
        .map(|user_id| format!("{scope_key}:actor:{user_id}"))
}

/// 构造 interaction scope。
///
/// 群聊多人场景下，pending、可见编号快照和“刚刚那条”等个人交互状态应优先使用
/// conversation + actor；缺少 actor 时才退回 conversation scope，保持旧入口兼容。
pub fn interaction_scope_key(user_id: Option<&str>, scope_key: &str) -> String {
    actor_scope_key(user_id, scope_key)
        .unwrap_or_else(|| clean_string(scope_key).unwrap_or_else(|| "unknown".to_owned()))
}

/// 构造 owner scope。
///
/// Todo / Memory 等业务数据归属可以复用 interaction scope 的隔离方式，但仍不等同于
/// delivery target，不能从它反推出平台发送参数。
pub fn owner_scope_key(user_id: Option<&str>, scope_key: &str) -> String {
    interaction_scope_key(user_id, scope_key)
}

/// 兼容旧命名的 owner key 构造入口。
///
/// 新代码优先使用 [`owner_scope_key`] 或 [`interaction_scope_key`] 表达真实用途。
pub fn actor_owner_key(user_id: Option<&str>, scope_key: &str) -> String {
    owner_scope_key(user_id, scope_key)
}

pub fn parse_stable_scope_key(value: &str) -> Option<ParsedScopeKey<'_>> {
    let mut parts = value.splitn(6, ':');
    match (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) {
        (
            Some("platform"),
            Some(platform),
            Some("account"),
            Some(account_id),
            Some(target_type),
            Some(raw_target_id),
        ) if !platform.trim().is_empty()
            && !account_id.trim().is_empty()
            && !target_type.trim().is_empty()
            && !raw_target_id.trim().is_empty() =>
        {
            Some(ParsedScopeKey {
                platform,
                account_id,
                target_type,
                raw_target_id,
            })
        }
        _ => None,
    }
}

pub fn private_raw_target_from_scope_key(value: &str) -> Option<String> {
    raw_target_from_scope_key(value, "private")
}

pub fn group_raw_target_from_scope_key(value: &str) -> Option<String> {
    raw_target_from_scope_key(value, "group")
}

pub fn raw_target_from_scope_key(value: &str, expected_type: &str) -> Option<String> {
    if let Some(parsed) = parse_stable_scope_key(value)
        && parsed.target_type == expected_type
    {
        return clean_string(parsed.raw_target_id);
    }
    value
        .strip_prefix(&format!("{expected_type}:"))
        .and_then(clean_string)
}

pub fn scope_target_type(value: &str) -> Option<&str> {
    if let Some(parsed) = parse_stable_scope_key(value) {
        return Some(parsed.target_type);
    }
    if value.starts_with("group:") {
        Some("group")
    } else if value.starts_with("private:") || value.starts_with("service_account:") {
        Some("private")
    } else {
        None
    }
}

fn clean_segment(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() { None } else { Some(value) }
}

fn clean_string(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_scope_key_preserves_raw_target_as_business_key_payload() {
        let key = conversation_scope_key("qq_official", Some("app-1"), "private", "openid-1");

        assert_eq!(key, "platform:qq_official:account:app-1:private:openid-1");
        assert_eq!(
            stable_scope_key("qq_official", Some("app-1"), "private", "openid-1"),
            key
        );
        assert_eq!(
            private_raw_target_from_scope_key(&key).as_deref(),
            Some("openid-1")
        );
    }

    #[test]
    fn actor_interaction_and_owner_scopes_keep_actor_and_conversation_separate() {
        let conversation = "platform:qq_official:account:app-1:group:group-1";

        assert_eq!(
            actor_scope_key(Some("member-1"), conversation).as_deref(),
            Some("platform:qq_official:account:app-1:group:group-1:actor:member-1")
        );
        assert_eq!(actor_scope_key(None, conversation), None);
        assert_eq!(
            interaction_scope_key(Some("member-1"), conversation),
            "platform:qq_official:account:app-1:group:group-1:actor:member-1"
        );
        assert_eq!(
            owner_scope_key(Some("member-1"), conversation),
            "platform:qq_official:account:app-1:group:group-1:actor:member-1"
        );
        assert_eq!(interaction_scope_key(None, "group:g1"), "group:g1");
        assert_eq!(actor_owner_key(None, "group:g1"), "group:g1");
    }
}

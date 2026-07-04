//! 跨平台身份与业务隔离键 helper。
//!
//! 这里的 key 只用于 session、pending、Memory、Todo 等业务状态隔离；
//! 平台真实发送目标仍由各业务的 target/raw id 字段保存，发送阶段不得反解析这些 key。

const UNKNOWN_ACCOUNT: &str = "-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedScopeKey<'a> {
    pub platform: &'a str,
    pub account_id: &'a str,
    pub target_type: &'a str,
    pub raw_target_id: &'a str,
}

pub fn stable_scope_key(
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

pub fn actor_owner_key(user_id: Option<&str>, scope_key: &str) -> String {
    let scope_key = clean_string(scope_key).unwrap_or_else(|| "unknown".to_owned());
    match user_id.and_then(clean_string) {
        Some(user_id) => format!("{scope_key}:actor:{user_id}"),
        None => scope_key,
    }
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
        let key = stable_scope_key("qq_official", Some("app-1"), "private", "openid-1");

        assert_eq!(key, "platform:qq_official:account:app-1:private:openid-1");
        assert_eq!(
            private_raw_target_from_scope_key(&key).as_deref(),
            Some("openid-1")
        );
    }

    #[test]
    fn actor_owner_key_keeps_actor_and_conversation_scope_separate() {
        assert_eq!(
            actor_owner_key(
                Some("member-1"),
                "platform:qq_official:account:app-1:group:group-1"
            ),
            "platform:qq_official:account:app-1:group:group-1:actor:member-1"
        );
        assert_eq!(actor_owner_key(None, "group:g1"), "group:g1");
    }
}

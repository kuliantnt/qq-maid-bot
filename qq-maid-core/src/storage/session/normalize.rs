//! Session 规范化与 ID 生成 helper。
//!
//! 这里集中维护会话记录补全、scope 推断、标题规范化与 session_id 生成等纯函数，
//! 不触碰 SQLite / 持久化数据格式。`DEFAULT_SESSION_TITLE` 等公开常量仍保留在 mod.rs，
//! 这里通过 `super::` 引用以保持单一来源。

use uuid::Uuid;

use super::{DEFAULT_SESSION_TITLE, SessionRecord};

/// 规范化会话记录，补全缺失字段。
pub(super) fn normalize_session(session: &mut SessionRecord) {
    if session.session_id.trim().is_empty() {
        session.session_id = build_session_id(&session.scope_key);
    }
    if session.scope_key.trim().is_empty() {
        session.scope_key = "unknown".to_owned();
    }
    if session.scope.trim().is_empty() {
        session.scope = infer_scope(
            &session.scope_key,
            session.group_id.as_deref(),
            session.guild_id.as_deref(),
        );
    }
    if session.created_at.trim().is_empty() {
        session.created_at = super::now_iso_cn();
    }
    if session.updated_at.trim().is_empty() {
        session.updated_at = session.created_at.clone();
    }
    if session.platform.trim().is_empty() {
        session.platform = "qq".to_owned();
    }
    if session.title.trim().is_empty() {
        session.title = DEFAULT_SESSION_TITLE.to_owned();
    }
}

/// 根据 scope_key 前缀推断会话作用域类型。
pub(super) fn infer_scope(
    scope_key: &str,
    group_id: Option<&str>,
    guild_id: Option<&str>,
) -> String {
    if scope_key.starts_with("guild:") || guild_id.is_some() {
        "guild_channel".to_owned()
    } else if scope_key.starts_with("group:") || group_id.is_some() {
        "group".to_owned()
    } else {
        "private".to_owned()
    }
}

/// 初始化会话状态，根据标题设置当前话题、活跃场景和预期模式。
pub(super) fn initial_session_state(title: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut state = serde_json::Map::new();
    if title.trim().is_empty() || title.trim() == DEFAULT_SESSION_TITLE {
        return state;
    }
    state.insert(
        "current_topic".to_owned(),
        serde_json::Value::String(title.trim().to_owned()),
    );
    state.insert(
        "active_scene".to_owned(),
        serde_json::Value::String("默认会话".to_owned()),
    );
    state.insert(
        "expected_mode".to_owned(),
        serde_json::Value::String("陪聊 + 轻量整理".to_owned()),
    );
    state
}

/// 规范化会话标题，空值时使用默认标题。
pub(super) fn normalize_session_title(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        DEFAULT_SESSION_TITLE.to_owned()
    } else {
        title.to_owned()
    }
}

/// 生成会话 ID：时间戳 + 作用域键 + UUID 片段组合。
pub(super) fn build_session_id(scope_key: &str) -> String {
    let timestamp = super::now_iso_cn()
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .take(14)
        .collect::<String>();
    format!(
        "{}-{}-{}",
        timestamp,
        safe_id_part(scope_key, "unknown"),
        &Uuid::new_v4().to_string()[..6]
    )
}

/// 将字符串转为安全的 ID 片段：仅保留字母数字、下划线、点、连字符。
pub(super) fn safe_id_part(value: &str, fallback: &str) -> String {
    let safe = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches(&['-', '.', '_'][..])
        .to_owned();
    if safe.is_empty() {
        fallback.to_owned()
    } else {
        safe
    }
}

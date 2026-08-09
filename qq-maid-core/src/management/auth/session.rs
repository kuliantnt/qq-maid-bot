//! 管理员会话的创建、容量和过期管理。
//!
//! 登录与初始化流程仍在父模块按事务顺序编排；本模块只封装会话记录和容量不变量。

use std::collections::HashMap;

use super::{
    AdminAuth, AdminAuthError, AdminSession, IssuedSession, MAX_ADMIN_SESSIONS,
    MAX_PREAUTH_SESSIONS, MAX_SESSIONS, SESSION_ABSOLUTE_TTL, SESSION_IDLE_TTL, SessionKind,
    SessionRecord, credentials, unix_seconds,
};

pub(super) fn new_admin_session(
    id: i64,
    username: &str,
) -> ([u8; 32], SessionRecord, IssuedSession) {
    let now = unix_seconds();
    let (cookie_value, cookie_hash) = credentials::random_token();
    let (csrf_token, csrf_hash) = credentials::random_token();
    let absolute_expires_at = now + SESSION_ABSOLUTE_TTL.as_secs() as i64;
    let record = SessionRecord {
        kind: SessionKind::Admin {
            id,
            username: username.to_owned(),
        },
        csrf_token: csrf_token.clone(),
        csrf_hash,
        created_at: now,
        last_seen_at: now,
        absolute_expires_at,
    };
    let issued = IssuedSession {
        cookie_value,
        session: AdminSession {
            username: username.to_owned(),
            capabilities: admin_capabilities(),
            csrf_token,
            expires_at: absolute_expires_at,
        },
    };
    (cookie_hash, record, issued)
}

pub(super) fn insert_session_locked(
    sessions: &mut HashMap<[u8; 32], SessionRecord>,
    token_hash: [u8; 32],
    record: SessionRecord,
) -> Result<(), AdminAuthError> {
    match &record.kind {
        SessionKind::PreAuth => {
            if session_count(sessions, SessionKindFilter::PreAuth) >= MAX_PREAUTH_SESSIONS {
                // 匿名容量满时只回收最旧 PreAuth，绝不让匿名洪泛淘汰 Admin。
                let oldest = oldest_session(sessions, SessionKindFilter::PreAuth)
                    .ok_or_else(session_capacity_reached)?;
                sessions.remove(&oldest);
            }
        }
        SessionKind::Admin { .. } => {
            if session_count(sessions, SessionKindFilter::Admin) >= MAX_ADMIN_SESSIONS {
                // 有效 Admin 达到独立上限时拒绝新会话，不隐式登出其他管理员浏览器。
                return Err(session_capacity_reached());
            }
        }
    }
    sessions.insert(token_hash, record);
    debug_assert!(sessions.len() <= MAX_SESSIONS);
    Ok(())
}

impl AdminAuth {
    pub(super) fn remove_session(&self, cookie_value: &str) -> Result<(), AdminAuthError> {
        self.sessions
            .lock()
            .map_err(session_lock_error)?
            .remove(&credentials::token_hash(cookie_value));
        Ok(())
    }
}

pub(super) fn prune_sessions(sessions: &mut HashMap<[u8; 32], SessionRecord>, now: i64) {
    sessions.retain(|_, value| {
        now <= value.absolute_expires_at
            && now - value.last_seen_at
                <= match value.kind {
                    SessionKind::PreAuth => super::PREAUTH_TTL.as_secs() as i64,
                    SessionKind::Admin { .. } => SESSION_IDLE_TTL.as_secs() as i64,
                }
    });
}

#[derive(Clone, Copy)]
pub(super) enum SessionKindFilter {
    PreAuth,
    Admin,
}

pub(super) fn session_matches(kind: &SessionKind, filter: SessionKindFilter) -> bool {
    matches!(
        (kind, filter),
        (SessionKind::PreAuth, SessionKindFilter::PreAuth)
            | (SessionKind::Admin { .. }, SessionKindFilter::Admin)
    )
}

pub(super) fn session_count(
    sessions: &HashMap<[u8; 32], SessionRecord>,
    filter: SessionKindFilter,
) -> usize {
    sessions
        .values()
        .filter(|record| session_matches(&record.kind, filter))
        .count()
}

pub(super) fn oldest_session(
    sessions: &HashMap<[u8; 32], SessionRecord>,
    filter: SessionKindFilter,
) -> Option<[u8; 32]> {
    sessions
        .iter()
        .filter(|(_, record)| session_matches(&record.kind, filter))
        .min_by_key(|(_, record)| record.created_at)
        .map(|(key, _)| *key)
}

pub(super) fn unauthenticated() -> AdminAuthError {
    AdminAuthError::new(
        "unauthenticated",
        "administrator session is missing or expired",
    )
}

pub(super) fn session_capacity_reached() -> AdminAuthError {
    AdminAuthError::new(
        "session_capacity_reached",
        "administrator session capacity has been reached; retry later",
    )
}

pub(super) fn session_lock_error<T>(_: std::sync::PoisonError<T>) -> AdminAuthError {
    AdminAuthError::storage("administrator session lock is poisoned")
}

pub(super) fn admin_capabilities() -> Vec<String> {
    [
        "console.config.read",
        "console.config.write",
        "console.audit.write",
        "console.process.restart",
        "memory.admin",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

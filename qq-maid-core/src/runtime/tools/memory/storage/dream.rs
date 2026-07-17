// Portions of this file are adapted from xai-org/grok-build's xai-grok-memory
// Dream implementation. Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// Modified for qq-maid-bot's SQLite target leases and atomic Memory checkpoints.

//! Session Dream 的检查点、短租约与原子 Memory 写入。
//!
//! 模型调用不属于 storage 事务：`claim_dream` 只在短事务内取得租约，随后读取输入；
//! `complete_dream` 再用独立短事务校验租约、写入候选并推进实际输入边界。

use std::collections::HashSet;

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use uuid::Uuid;

use crate::runtime::session::{SessionMessage, SessionTurnActor};

use super::{
    MemoryCategory, MemoryError, MemoryKind, MemorySourceType, MemoryStatus, MemoryStore,
    MemoryTarget, MemoryVisibility, PersistMemoryRequest, insert_record_unlocked,
    v3::{build_v3_record, ensure_profile_enabled_unlocked},
};

const DREAM_LEASE_SECONDS: i64 = 600;

#[derive(Debug, Clone)]
pub(crate) struct DreamContext {
    pub actor_scope_id: String,
    pub target: MemoryTarget,
    pub conversation_scope_key: String,
    pub actor_ref: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DreamLimits {
    pub min_interval_seconds: u64,
    pub min_new_sessions: usize,
    pub max_sessions: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct DreamSession {
    pub session_id: String,
    pub updated_at: String,
    pub checkpoint_message_id: i64,
    pub summary: Option<String>,
    pub user_messages: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DreamClaim {
    pub token: String,
    pub sessions: Vec<DreamSession>,
    pub has_more_sessions: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DreamCandidate {
    pub content: String,
    pub category: MemoryCategory,
    pub attribute_key: Option<String>,
}

pub(crate) struct DreamCompletion<'a> {
    pub checkpoint_message_id: i64,
    pub checkpoint_updated_at: &'a str,
    pub checkpoint_session_id: &'a str,
    pub candidates: &'a [DreamCandidate],
    pub input_count: usize,
    pub truncated: bool,
    pub now_epoch: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DreamFinalizeStats {
    pub inserted_count: usize,
    pub duplicate_count: usize,
    pub conflict_count: usize,
}

#[derive(Debug)]
struct StoredDreamState {
    conversation_scope_key: String,
    actor_ref: String,
    last_processed_message_id: i64,
    last_run_at_epoch: i64,
    truncated: bool,
    lock_until_epoch: i64,
}

#[derive(Debug)]
struct SessionHeader {
    session_id: String,
    updated_at: String,
    checkpoint_message_id: i64,
    summary: String,
    extra_json: String,
}

impl MemoryStore {
    /// 抢占一个 target 的 Dream 批次。事务在返回前已经提交，模型调用必须发生在返回后。
    pub(crate) fn claim_dream(
        &self,
        context: &DreamContext,
        limits: DreamLimits,
        now_epoch: i64,
    ) -> Result<Option<DreamClaim>, MemoryError> {
        let target = context.target.clean()?;
        let actor_ref = context.actor_ref.as_deref().unwrap_or("").trim();
        let scope_key = context.conversation_scope_key.trim();
        if scope_key.is_empty() || context.actor_scope_id.trim().is_empty() {
            return Err(MemoryError::bad_request("invalid Dream context"));
        }

        let mut conn = self.connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MemoryError::from_sql)?;
        let state = load_state(&tx, &target)?;
        if target.memory_kind() == MemoryKind::GroupProfile && !profile_enabled(&tx, &target)? {
            return Ok(None);
        }
        if let Some(state) = &state {
            if state.conversation_scope_key != scope_key || state.actor_ref != actor_ref {
                return Err(MemoryError::bad_request(
                    "Dream checkpoint context mismatch",
                ));
            }
            if state.lock_until_epoch > now_epoch {
                return Ok(None);
            }
            let next_due = state
                .last_run_at_epoch
                .saturating_add(limits.min_interval_seconds.min(i64::MAX as u64) as i64);
            if state.last_run_at_epoch > 0 && next_due > now_epoch {
                return Ok(None);
            }
        }

        let last_message_id = state
            .as_ref()
            .map(|state| state.last_processed_message_id)
            .unwrap_or(0);
        let mut headers = select_session_headers(
            &tx,
            scope_key,
            actor_ref,
            last_message_id,
            limits.max_sessions.saturating_add(1),
        )?;
        let has_more_sessions = headers.len() > limits.max_sessions;
        headers.truncate(limits.max_sessions);
        let prior_truncated = state.as_ref().is_some_and(|state| state.truncated);
        if headers.is_empty()
            || (!prior_truncated && headers.len() < limits.min_new_sessions.max(1))
        {
            return Ok(None);
        }

        let token = Uuid::new_v4().to_string();
        let lock_until_epoch = now_epoch.saturating_add(DREAM_LEASE_SECONDS);
        tx.execute(
            "INSERT INTO memory_dream_state (
                scope_type, scope_id, memory_kind, subject_key,
                conversation_scope_key, actor_ref, lock_token, lock_until_epoch
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(scope_type, scope_id, memory_kind, subject_key) DO UPDATE SET
                lock_token = excluded.lock_token,
                lock_until_epoch = excluded.lock_until_epoch",
            params![
                target.scope_type().as_str(),
                target.scope_id(),
                target.memory_kind().as_str(),
                target.subject_id().unwrap_or(""),
                scope_key,
                actor_ref,
                token,
                lock_until_epoch,
            ],
        )
        .map_err(MemoryError::from_sql)?;
        tx.commit().map_err(MemoryError::from_sql)?;

        // 输入读取发生在租约事务外。Session 正文不会进入日志或 Dream 状态表。
        let sessions = match load_dream_sessions(&conn, &headers, context.actor_ref.as_deref()) {
            Ok(sessions) => sessions,
            Err(error) => {
                drop(conn);
                let _ = self.release_dream(context, &token);
                return Err(error);
            }
        };
        Ok(Some(DreamClaim {
            token,
            sessions,
            has_more_sessions,
        }))
    }

    /// 失败时只释放当前租约，不修改检查点和上次成功时间。
    pub(crate) fn release_dream(
        &self,
        context: &DreamContext,
        token: &str,
    ) -> Result<(), MemoryError> {
        let target = context.target.clean()?;
        self.connection()?
            .execute(
                "UPDATE memory_dream_state
                 SET lock_token = NULL, lock_until_epoch = 0, last_status = 'failed'
                 WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
                   AND subject_key = ?4 AND lock_token = ?5",
                params![
                    target.scope_type().as_str(),
                    target.scope_id(),
                    target.memory_kind().as_str(),
                    target.subject_id().unwrap_or(""),
                    token,
                ],
            )
            .map_err(MemoryError::from_sql)?;
        Ok(())
    }

    /// 在同一短事务内写入安全候选并推进本轮实际输入边界。
    ///
    /// `candidates` 为空仍是成功批次（包括 NO_REPLY 或候选全部被安全过滤）。
    pub(crate) fn complete_dream(
        &self,
        context: &DreamContext,
        token: &str,
        completion: DreamCompletion<'_>,
    ) -> Result<DreamFinalizeStats, MemoryError> {
        let target = context.target.clean()?;
        if completion.checkpoint_updated_at.trim().is_empty()
            || completion.checkpoint_session_id.trim().is_empty()
        {
            return Err(MemoryError::bad_request("Dream checkpoint is empty"));
        }
        let mut conn = self.connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MemoryError::from_sql)?;
        let owns_lock = tx
            .query_row(
                "SELECT 1 FROM memory_dream_state
                 WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
                   AND subject_key = ?4 AND conversation_scope_key = ?5
                   AND actor_ref = ?6 AND lock_token = ?7",
                params![
                    target.scope_type().as_str(),
                    target.scope_id(),
                    target.memory_kind().as_str(),
                    target.subject_id().unwrap_or(""),
                    context.conversation_scope_key,
                    context.actor_ref.as_deref().unwrap_or(""),
                    token,
                ],
                |_| Ok(()),
            )
            .optional()
            .map_err(MemoryError::from_sql)?
            .is_some();
        if !owns_lock {
            return Err(MemoryError::changed("Dream lease changed before commit"));
        }
        if target.memory_kind() == MemoryKind::GroupProfile && !profile_enabled(&tx, &target)? {
            return Err(MemoryError::forbidden(
                "group profile was opted out before Dream commit",
            ));
        }

        let mut stats = DreamFinalizeStats::default();
        let mut seen_batch = HashSet::new();
        for candidate in completion.candidates {
            let normalized = candidate.content.trim();
            if !seen_batch.insert(normalized.to_owned())
                || exact_content_exists(&tx, &target, normalized)?
            {
                stats.duplicate_count += 1;
                continue;
            }
            if let Some(key) = candidate.attribute_key.as_deref()
                && user_confirmed_key_conflicts(&tx, &target, key, normalized)?
            {
                stats.conflict_count += 1;
                continue;
            }
            let record = build_v3_record(PersistMemoryRequest {
                target: target.clone(),
                created_by_user_id: context.actor_scope_id.clone(),
                content: normalized.to_owned(),
                source_text: String::new(),
                category: candidate.category,
                legacy_scope: match target.memory_kind() {
                    MemoryKind::Personal => "private",
                    MemoryKind::GroupProfile => "group_profile",
                    _ => "private",
                }
                .to_owned(),
                visibility: match target.memory_kind() {
                    MemoryKind::Personal => MemoryVisibility::Private,
                    MemoryKind::GroupProfile => MemoryVisibility::GroupMembers,
                    _ => MemoryVisibility::Private,
                },
                source_type: MemorySourceType::SystemDerived,
                source_ref: None,
                confirmed_at: None,
                pinned: false,
                attribute_key: candidate.attribute_key.clone(),
                relation_subject_id: None,
                relation_object_id: None,
            })?;
            ensure_profile_enabled_unlocked(&tx, &record)?;
            insert_record_unlocked(&tx, &record)?;
            stats.inserted_count += 1;
        }

        tx.execute(
            "UPDATE memory_dream_state SET
                last_processed_updated_at = ?1,
                last_processed_session_id = ?2,
                last_processed_message_id = ?3,
                last_run_at_epoch = ?4,
                last_status = 'success',
                input_count = ?5,
                output_count = ?6,
                duplicate_count = ?7,
                conflict_count = ?8,
                truncated = ?9,
                lock_token = NULL,
                lock_until_epoch = 0
             WHERE scope_type = ?10 AND scope_id = ?11 AND memory_kind = ?12
               AND subject_key = ?13 AND lock_token = ?14",
            params![
                completion.checkpoint_updated_at,
                completion.checkpoint_session_id,
                completion.checkpoint_message_id,
                completion.now_epoch,
                completion.input_count as i64,
                stats.inserted_count as i64,
                stats.duplicate_count as i64,
                stats.conflict_count as i64,
                completion.truncated,
                target.scope_type().as_str(),
                target.scope_id(),
                target.memory_kind().as_str(),
                target.subject_id().unwrap_or(""),
                token,
            ],
        )
        .map_err(MemoryError::from_sql)?;
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok(stats)
    }
}

fn load_state(
    conn: &rusqlite::Connection,
    target: &MemoryTarget,
) -> Result<Option<StoredDreamState>, MemoryError> {
    conn.query_row(
        "SELECT conversation_scope_key, actor_ref, last_processed_message_id,
                last_run_at_epoch, truncated, lock_until_epoch
         FROM memory_dream_state
         WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3 AND subject_key = ?4",
        params![
            target.scope_type().as_str(),
            target.scope_id(),
            target.memory_kind().as_str(),
            target.subject_id().unwrap_or(""),
        ],
        |row| {
            Ok(StoredDreamState {
                conversation_scope_key: row.get(0)?,
                actor_ref: row.get(1)?,
                last_processed_message_id: row.get(2)?,
                last_run_at_epoch: row.get(3)?,
                truncated: row.get(4)?,
                lock_until_epoch: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(MemoryError::from_sql)
}

fn select_session_headers(
    conn: &rusqlite::Connection,
    scope_key: &str,
    actor_ref: &str,
    last_message_id: i64,
    limit: usize,
) -> Result<Vec<SessionHeader>, MemoryError> {
    let actor_pattern = format!("%\"actor_ref\":\"{actor_ref}\"%");
    let (sql, values): (&str, Vec<rusqlite::types::Value>) = if actor_ref.is_empty() {
        (
            "SELECT s.session_id, s.updated_at, s.summary, s.extra_json, MAX(sm.id)
             FROM sessions s
             JOIN session_messages sm ON sm.session_id = s.session_id
             WHERE s.scope_key = ?1 AND sm.role = 'user'
             GROUP BY s.session_id
             HAVING MAX(sm.id) > ?2
             ORDER BY MAX(sm.id) ASC, s.session_id ASC LIMIT ?3",
            vec![
                scope_key.to_owned().into(),
                last_message_id.into(),
                (limit as i64).into(),
            ],
        )
    } else {
        (
            "SELECT s.session_id, s.updated_at, s.summary, s.extra_json, MAX(sm.id)
             FROM sessions s
             JOIN session_messages sm ON sm.session_id = s.session_id
             WHERE s.scope_key = ?1 AND sm.role = 'user' AND sm.turn_actor_json LIKE ?2
             GROUP BY s.session_id
             HAVING MAX(sm.id) > ?3
             ORDER BY MAX(sm.id) ASC, s.session_id ASC LIMIT ?4",
            vec![
                scope_key.to_owned().into(),
                actor_pattern.into(),
                last_message_id.into(),
                (limit as i64).into(),
            ],
        )
    };
    let mut stmt = conn.prepare(sql).map_err(MemoryError::from_sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values), |row| {
            Ok(SessionHeader {
                session_id: row.get(0)?,
                updated_at: row.get(1)?,
                summary: row.get(2)?,
                extra_json: row.get(3)?,
                checkpoint_message_id: row.get(4)?,
            })
        })
        .map_err(MemoryError::from_sql)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from_sql)
}

fn load_dream_sessions(
    conn: &rusqlite::Connection,
    headers: &[SessionHeader],
    actor_ref: Option<&str>,
) -> Result<Vec<DreamSession>, MemoryError> {
    let mut result = Vec::with_capacity(headers.len());
    for header in headers {
        let mut messages = archived_user_messages(&header.extra_json, actor_ref);
        let mut stmt = conn
            .prepare(
                "SELECT content, turn_actor_json FROM session_messages
                 WHERE session_id = ?1 AND role = 'user'
                 ORDER BY message_index ASC, id ASC",
            )
            .map_err(MemoryError::from_sql)?;
        let rows = stmt
            .query_map(params![header.session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(MemoryError::from_sql)?;
        for row in rows {
            let (content, turn_actor_json) = row.map_err(MemoryError::from_sql)?;
            if actor_matches(turn_actor_json.as_deref(), actor_ref) {
                messages.push(content);
            }
        }
        result.push(DreamSession {
            session_id: header.session_id.clone(),
            updated_at: header.updated_at.clone(),
            checkpoint_message_id: header.checkpoint_message_id,
            summary: actor_ref
                .is_none()
                .then(|| header.summary.trim().to_owned())
                .filter(|s| !s.is_empty()),
            user_messages: messages,
        });
    }
    Ok(result)
}

fn archived_user_messages(extra_json: &str, actor_ref: Option<&str>) -> Vec<String> {
    let Ok(extra) = serde_json::from_str::<Value>(extra_json) else {
        return Vec::new();
    };
    extra
        .get("archived_history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|archive| archive.get("history").and_then(Value::as_array))
        .flatten()
        .filter_map(|message| serde_json::from_value::<SessionMessage>(message.clone()).ok())
        .filter(|message| message.role == "user")
        .filter(|message| {
            actor_ref.is_none()
                || message
                    .turn_actor
                    .as_ref()
                    .and_then(|actor| actor.actor_ref.as_deref())
                    == actor_ref
        })
        .map(|message| message.content)
        .collect()
}

fn actor_matches(raw: Option<&str>, actor_ref: Option<&str>) -> bool {
    let Some(expected) = actor_ref else {
        return true;
    };
    raw.and_then(|raw| serde_json::from_str::<SessionTurnActor>(raw).ok())
        .and_then(|actor| actor.actor_ref)
        .as_deref()
        == Some(expected)
}

fn exact_content_exists(
    conn: &rusqlite::Connection,
    target: &MemoryTarget,
    content: &str,
) -> Result<bool, MemoryError> {
    target_exists_query(
        conn,
        target,
        "content = ?5",
        params![
            target.scope_type().as_str(),
            target.scope_id(),
            target.memory_kind().as_str(),
            target.subject_id().unwrap_or(""),
            content,
        ],
    )
}

fn user_confirmed_key_conflicts(
    conn: &rusqlite::Connection,
    target: &MemoryTarget,
    attribute_key: &str,
    content: &str,
) -> Result<bool, MemoryError> {
    conn.query_row(
        "SELECT 1 FROM memories
         WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
           AND COALESCE(subject_id, '') = ?4 AND status = ?5
           AND source_type = ?6 AND attribute_key = ?7 AND content <> ?8
         LIMIT 1",
        params![
            target.scope_type().as_str(),
            target.scope_id(),
            target.memory_kind().as_str(),
            target.subject_id().unwrap_or(""),
            MemoryStatus::Active.as_str(),
            MemorySourceType::UserConfirmed.as_str(),
            attribute_key,
            content,
        ],
        |_| Ok(()),
    )
    .optional()
    .map(|value| value.is_some())
    .map_err(MemoryError::from_sql)
}

fn profile_enabled(
    conn: &rusqlite::Connection,
    target: &MemoryTarget,
) -> Result<bool, MemoryError> {
    conn.query_row(
        "SELECT profile_enabled FROM memory_profile_preferences
         WHERE group_scope_id = ?1 AND subject_id = ?2",
        params![target.scope_id(), target.subject_id().unwrap_or("")],
        |row| row.get::<_, bool>(0),
    )
    .optional()
    .map(|enabled| enabled.unwrap_or(true))
    .map_err(MemoryError::from_sql)
}

fn target_exists_query<P: rusqlite::Params>(
    conn: &rusqlite::Connection,
    target: &MemoryTarget,
    predicate: &str,
    params: P,
) -> Result<bool, MemoryError> {
    let sql = format!(
        "SELECT 1 FROM memories
         WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
           AND COALESCE(subject_id, '') = ?4 AND status = 'active' AND {predicate}
         LIMIT 1"
    );
    let _ = target;
    conn.query_row(&sql, params, |_| Ok(()))
        .optional()
        .map(|value| value.is_some())
        .map_err(MemoryError::from_sql)
}

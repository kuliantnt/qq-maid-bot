// Portions of this file are adapted from xai-org/grok-build's xai-grok-memory
// Dream implementation. Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// Modified for qq-maid-bot's SQLite target leases and atomic Memory checkpoints.

//! Session Dream 的检查点、短租约与原子 Memory 写入。
//!
//! 模型调用不属于 storage 事务：`claim_dream` 在短事务内读取稳定消息边界并取得租约；
//! `complete_dream` 再用独立短事务校验租约、写入候选并推进实际输入边界。

use std::collections::HashSet;

use qq_maid_common::time_context::local_date_from_timestamp;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use tracing::debug;
use uuid::Uuid;

use crate::runtime::session::{SessionMessage, SessionTurnActor};

use super::{
    MemoryCategory, MemoryError, MemoryKind, MemorySourceType, MemoryStatus, MemoryStore,
    MemoryTarget, MemoryVisibility, PersistMemoryRequest, insert_record_unlocked,
    v3::{build_v3_record, ensure_profile_enabled_unlocked},
};

const DREAM_LEASE_SECONDS: i64 = 600;
const DREAM_SESSION_PATH_MIN_MESSAGES: usize = 30;
const DREAM_ACTIVE_DATE_PATH_MIN_DATES: usize = 3;
const DREAM_ACTIVE_DATE_PATH_MIN_MESSAGES: usize = 50;
const DREAM_INTERVAL_PATH_MIN_SECONDS: u64 = 7 * 24 * 60 * 60;
const DREAM_INTERVAL_PATH_MIN_MESSAGES: usize = 60;

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
    pub max_sessions: usize,
    pub trigger_policy: DreamTriggerPolicy,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DreamTriggerPolicy {
    min_new_sessions: usize,
    min_session_path_messages: usize,
    min_active_dates: usize,
    min_active_date_path_messages: usize,
    min_checkpoint_interval_seconds: u64,
    min_interval_path_messages: usize,
}

impl DreamTriggerPolicy {
    pub(crate) const fn production(min_new_sessions: usize) -> Self {
        Self {
            min_new_sessions,
            min_session_path_messages: DREAM_SESSION_PATH_MIN_MESSAGES,
            min_active_dates: DREAM_ACTIVE_DATE_PATH_MIN_DATES,
            min_active_date_path_messages: DREAM_ACTIVE_DATE_PATH_MIN_MESSAGES,
            min_checkpoint_interval_seconds: DREAM_INTERVAL_PATH_MIN_SECONDS,
            min_interval_path_messages: DREAM_INTERVAL_PATH_MIN_MESSAGES,
        }
    }

    #[cfg(test)]
    pub(crate) const fn permissive() -> Self {
        Self {
            min_new_sessions: 1,
            min_session_path_messages: 1,
            min_active_dates: usize::MAX,
            min_active_date_path_messages: usize::MAX,
            min_checkpoint_interval_seconds: u64::MAX,
            min_interval_path_messages: usize::MAX,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DreamMessage {
    pub message_id: i64,
    pub session_id: String,
    pub updated_at: String,
    pub timestamp: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DreamClaim {
    pub token: String,
    pub messages: Vec<DreamMessage>,
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
    extra_json: String,
}

#[derive(Debug, Clone, Copy)]
struct DreamTriggerStats {
    message_count: usize,
    session_count: usize,
    active_date_count: usize,
    checkpoint_interval_seconds: u64,
    has_successful_checkpoint: bool,
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
            .filter(|state| state.last_run_at_epoch > 0)
            .map(|state| state.last_processed_message_id)
            .unwrap_or(i64::MIN);
        let mut headers = select_session_headers(&tx, scope_key)?;
        backfill_legacy_archived_message_ids(&tx, &mut headers)?;
        let messages =
            load_dream_messages(&tx, &headers, context.actor_ref.as_deref(), last_message_id)?;
        // 统计只基于检查点后的稳定 user 消息；assistant、重复 ID 和其他群成员已在读取层排除。
        let trigger_stats = dream_trigger_stats(&messages, state.as_ref(), now_epoch);
        // 三路门槛只控制首次批次。成功批次若因字符或 Session 上限截断，truncated 会把
        // 连续尾部标记为待续批；后续调度仍经过冷却和租约检查，但不重新累计消息门槛。
        let pending_continuation = state.as_ref().is_some_and(|state| state.truncated);
        let initial_trigger_matches = dream_trigger_matches(trigger_stats, limits.trigger_policy);
        if messages.is_empty() || (!pending_continuation && !initial_trigger_matches) {
            let reason = if messages.is_empty() {
                "no_pending_messages".to_owned()
            } else {
                dream_trigger_miss_reason(trigger_stats, limits.trigger_policy)
            };
            debug!(
                message_count = trigger_stats.message_count,
                session_count = trigger_stats.session_count,
                active_date_count = trigger_stats.active_date_count,
                checkpoint_interval_seconds = trigger_stats.checkpoint_interval_seconds,
                has_successful_checkpoint = trigger_stats.has_successful_checkpoint,
                pending_continuation,
                reason = %reason,
                "memory Dream trigger skipped"
            );
            return Ok(None);
        }

        let mut selected_sessions = HashSet::new();
        let mut selected_messages = Vec::new();
        let mut has_more_sessions = false;
        for message in messages {
            if selected_sessions.contains(message.session_id.as_str()) {
                selected_messages.push(message);
            } else if selected_sessions.len() < limits.max_sessions.max(1) {
                selected_sessions.insert(message.session_id.clone());
                selected_messages.push(message);
            } else {
                has_more_sessions = true;
                // 检查点只能覆盖稳定消息流的连续前缀，不能越过未选择 Session 的消息。
                break;
            }
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

        // Session 正文只在领取阶段读取，不进入日志或 Dream 状态表。
        Ok(Some(DreamClaim {
            token,
            messages: selected_messages,
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
) -> Result<Vec<SessionHeader>, MemoryError> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, updated_at, extra_json
             FROM sessions
             WHERE scope_key = ?1
             ORDER BY created_at ASC, session_id ASC",
        )
        .map_err(MemoryError::from_sql)?;
    let rows = stmt
        .query_map(params![scope_key], |row| {
            Ok(SessionHeader {
                session_id: row.get(0)?,
                updated_at: row.get(1)?,
                extra_json: row.get(2)?,
            })
        })
        .map_err(MemoryError::from_sql)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from_sql)
}

/// 旧版归档 JSON 没有稳定消息 ID。首次 Dream 时为这些消息写入持久化负数 ID，
/// 使其稳定排在现有 SQLite 正数行 ID 之前，后续普通保存和再次读取都复用该边界。
fn backfill_legacy_archived_message_ids(
    conn: &rusqlite::Connection,
    headers: &mut [SessionHeader],
) -> Result<(), MemoryError> {
    let mut used_ids = HashSet::new();
    for header in headers.iter() {
        let Ok(extra) = serde_json::from_str::<Value>(&header.extra_json) else {
            continue;
        };
        for message in archived_message_values(&extra) {
            if let Some(message_id) = message.get("message_id").and_then(Value::as_i64) {
                used_ids.insert(message_id);
            }
        }
    }

    let mut next_id = i64::MIN.saturating_add(1);
    for header in headers.iter_mut() {
        let Ok(mut extra) = serde_json::from_str::<Value>(&header.extra_json) else {
            continue;
        };
        let mut changed = false;
        let Some(archives) = extra
            .get_mut("archived_history")
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        for archive in archives {
            let Some(messages) = archive.get_mut("history").and_then(Value::as_array_mut) else {
                continue;
            };
            for message in messages {
                let Some(object) = message.as_object_mut() else {
                    continue;
                };
                if object.get("message_id").and_then(Value::as_i64).is_some() {
                    continue;
                }
                while used_ids.contains(&next_id) {
                    next_id = next_id.saturating_add(1);
                }
                object.insert("message_id".to_owned(), Value::from(next_id));
                used_ids.insert(next_id);
                next_id = next_id.saturating_add(1);
                changed = true;
            }
        }
        if changed {
            let extra_json = serde_json::to_string(&extra)
                .map_err(|_| MemoryError::bad_request("encode Session archive failed"))?;
            conn.execute(
                "UPDATE sessions SET extra_json = ?1 WHERE session_id = ?2",
                params![extra_json, header.session_id],
            )
            .map_err(MemoryError::from_sql)?;
            header.extra_json = extra_json;
        }
    }
    Ok(())
}

fn archived_message_values(extra: &Value) -> Vec<&Value> {
    extra
        .get("archived_history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|archive| archive.get("history").and_then(Value::as_array))
        .flatten()
        .collect()
}

fn load_dream_messages(
    conn: &rusqlite::Connection,
    headers: &[SessionHeader],
    actor_ref: Option<&str>,
    last_message_id: i64,
) -> Result<Vec<DreamMessage>, MemoryError> {
    let mut result = Vec::new();
    let mut seen_message_ids = HashSet::new();
    for header in headers {
        for message in archived_user_messages(&header.extra_json, actor_ref) {
            let Some(message_id) = message.message_id.filter(|id| *id > last_message_id) else {
                continue;
            };
            if seen_message_ids.insert(message_id) {
                result.push(DreamMessage {
                    message_id,
                    session_id: header.session_id.clone(),
                    updated_at: header.updated_at.clone(),
                    timestamp: message.ts,
                    content: message.content,
                });
            }
        }
        let mut stmt = conn
            .prepare(
                "SELECT id, content, ts, turn_actor_json FROM session_messages
                 WHERE session_id = ?1 AND role = 'user' AND id > ?2
                 ORDER BY id ASC",
            )
            .map_err(MemoryError::from_sql)?;
        let rows = stmt
            .query_map(params![header.session_id, last_message_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(MemoryError::from_sql)?;
        for row in rows {
            let (message_id, content, timestamp, turn_actor_json) =
                row.map_err(MemoryError::from_sql)?;
            if actor_matches(turn_actor_json.as_deref(), actor_ref)
                && seen_message_ids.insert(message_id)
            {
                result.push(DreamMessage {
                    message_id,
                    session_id: header.session_id.clone(),
                    updated_at: header.updated_at.clone(),
                    timestamp,
                    content,
                });
            }
        }
    }
    result.sort_by_key(|message| message.message_id);
    Ok(result)
}

fn dream_trigger_stats(
    messages: &[DreamMessage],
    state: Option<&StoredDreamState>,
    now_epoch: i64,
) -> DreamTriggerStats {
    let session_count = messages
        .iter()
        .map(|message| message.session_id.as_str())
        .collect::<HashSet<_>>()
        .len();
    // Session 消息时间戳可能携带 UTC 或其他偏移；统一按项目北京时间策略换算自然日。
    let active_date_count = messages
        .iter()
        .filter_map(|message| local_date_from_timestamp(&message.timestamp))
        .collect::<HashSet<_>>()
        .len();
    let last_run_at_epoch = state.map_or(0, |state| state.last_run_at_epoch);
    let has_successful_checkpoint = last_run_at_epoch > 0;
    let checkpoint_interval_seconds = if has_successful_checkpoint {
        now_epoch.saturating_sub(last_run_at_epoch).max(0) as u64
    } else {
        0
    };
    DreamTriggerStats {
        message_count: messages.len(),
        session_count,
        active_date_count,
        checkpoint_interval_seconds,
        has_successful_checkpoint,
    }
}

fn dream_trigger_matches(stats: DreamTriggerStats, policy: DreamTriggerPolicy) -> bool {
    let session_path = stats.session_count >= policy.min_new_sessions.max(1)
        && stats.message_count >= policy.min_session_path_messages.max(1);
    let active_date_path = stats.active_date_count >= policy.min_active_dates.max(1)
        && stats.message_count >= policy.min_active_date_path_messages.max(1);
    let interval_path = stats.has_successful_checkpoint
        && stats.checkpoint_interval_seconds >= policy.min_checkpoint_interval_seconds
        && stats.message_count >= policy.min_interval_path_messages.max(1);
    session_path || active_date_path || interval_path
}

fn dream_trigger_miss_reason(stats: DreamTriggerStats, policy: DreamTriggerPolicy) -> String {
    let session_reason = format!(
        "session_path(messages={}/{},sessions={}/{})",
        stats.message_count,
        policy.min_session_path_messages.max(1),
        stats.session_count,
        policy.min_new_sessions.max(1)
    );
    let active_date_reason = format!(
        "active_date_path(messages={}/{},dates={}/{})",
        stats.message_count,
        policy.min_active_date_path_messages.max(1),
        stats.active_date_count,
        policy.min_active_dates.max(1)
    );
    let interval_reason = if stats.has_successful_checkpoint {
        format!(
            "interval_path(messages={}/{},seconds={}/{})",
            stats.message_count,
            policy.min_interval_path_messages.max(1),
            stats.checkpoint_interval_seconds,
            policy.min_checkpoint_interval_seconds
        )
    } else {
        format!(
            "interval_path(messages={}/{},checkpoint=missing)",
            stats.message_count,
            policy.min_interval_path_messages.max(1)
        )
    };
    format!("{session_reason};{active_date_reason};{interval_reason}")
}

fn archived_user_messages(extra_json: &str, actor_ref: Option<&str>) -> Vec<SessionMessage> {
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

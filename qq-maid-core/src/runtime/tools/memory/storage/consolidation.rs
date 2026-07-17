//! Memory 后台整理的 SQLite 领取、去重与检查点事务。
//!
//! 这里只处理“内容与语义键完全相同”的确定性重复项。模糊相似与事实冲突继续保留，
//! 交给后续显式确认或更高层策略处理，避免后台任务静默改写用户事实。

use std::collections::HashMap;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::{
    MemoryError, MemoryKind, MemoryRecord, MemoryScopeType, MemorySourceType, MemoryStore,
    MemoryTarget, row::memory_from_row,
};

const MAX_TARGETS_PER_RUN: usize = 10;

#[derive(Debug, Clone, Copy)]
pub(crate) struct ConsolidationLimits {
    pub min_interval_seconds: u64,
    pub min_new_records: usize,
    pub min_distinct_sources: usize,
    pub max_records: usize,
    pub max_input_chars: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ConsolidationRunStats {
    pub candidate_target_count: usize,
    pub processed_target_count: usize,
    pub input_record_count: usize,
    pub output_record_count: usize,
    pub archived_duplicate_count: usize,
    pub conflict_count: usize,
    pub truncated_target_count: usize,
}

#[derive(Debug)]
struct DueTarget {
    target: MemoryTarget,
}

#[derive(Debug)]
struct RecordWithRowId {
    row_id: i64,
    record: MemoryRecord,
}

impl MemoryStore {
    pub(crate) fn consolidate_due(
        &self,
        limits: ConsolidationLimits,
        now_epoch: i64,
    ) -> Result<ConsolidationRunStats, MemoryError> {
        let targets = self.list_due_consolidation_targets(limits, now_epoch)?;
        let mut stats = ConsolidationRunStats {
            candidate_target_count: targets.len(),
            ..ConsolidationRunStats::default()
        };
        for due in targets {
            let target_stats = self.consolidate_target(&due.target, limits, now_epoch)?;
            stats.processed_target_count += target_stats.processed_target_count;
            stats.input_record_count += target_stats.input_record_count;
            stats.output_record_count += target_stats.output_record_count;
            stats.archived_duplicate_count += target_stats.archived_duplicate_count;
            stats.conflict_count += target_stats.conflict_count;
            stats.truncated_target_count += target_stats.truncated_target_count;
        }
        Ok(stats)
    }

    fn list_due_consolidation_targets(
        &self,
        limits: ConsolidationLimits,
        now_epoch: i64,
    ) -> Result<Vec<DueTarget>, MemoryError> {
        let conn = self.connection()?;
        let cutoff = now_epoch.saturating_sub(limits.min_interval_seconds as i64);
        let mut stmt = conn
            .prepare(
                "SELECT m.scope_type, m.scope_id, m.memory_kind, m.subject_id
                   FROM memories m
              LEFT JOIN memory_consolidation_state s
                     ON s.scope_type = m.scope_type
                    AND s.scope_id = m.scope_id
                    AND s.memory_kind = m.memory_kind
                    AND s.subject_key = ifnull(m.subject_id, '')
                  WHERE m.status = 'active'
                    AND m.memory_kind IN ('personal', 'group_profile', 'group')
                    AND (s.last_run_at_epoch IS NULL OR s.last_run_at_epoch <= ?1)
               GROUP BY m.scope_type, m.scope_id, m.memory_kind, m.subject_id
                 HAVING sum(CASE WHEN m.row_id > ifnull(s.last_processed_row_id, 0)
                                      THEN 1 ELSE 0 END) > 0
                    AND (
                        max(ifnull(s.truncated, 0)) = 1
                        OR (
                            sum(CASE WHEN m.row_id > ifnull(s.last_processed_row_id, 0)
                                     THEN 1 ELSE 0 END) >= ?2
                            AND count(DISTINCT CASE
                                    WHEN m.row_id > ifnull(s.last_processed_row_id, 0)
                                     AND m.source_ref IS NOT NULL
                                     AND trim(m.source_ref) <> ''
                                    THEN m.source_ref END) >= ?3
                        )
                    )
               ORDER BY max(m.row_id) ASC
                  LIMIT ?4",
            )
            .map_err(MemoryError::from_sql)?;
        let rows = stmt
            .query_map(
                params![
                    cutoff,
                    limits.min_new_records as i64,
                    limits.min_distinct_sources as i64,
                    MAX_TARGETS_PER_RUN as i64,
                ],
                |row| {
                    let scope_type_text: String = row.get(0)?;
                    let memory_kind_text: String = row.get(2)?;
                    Ok((
                        scope_type_text,
                        row.get::<_, String>(1)?,
                        memory_kind_text,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .map_err(MemoryError::from_sql)?;
        let mut targets = Vec::new();
        for row in rows {
            let (scope_type, scope_id, memory_kind, subject_id) =
                row.map_err(MemoryError::from_sql)?;
            let scope_type = scope_type
                .parse::<MemoryScopeType>()
                .map_err(|_| MemoryError::io("invalid consolidation scope_type"))?;
            let memory_kind = memory_kind
                .parse::<MemoryKind>()
                .map_err(|_| MemoryError::io("invalid consolidation memory_kind"))?;
            targets.push(DueTarget {
                target: MemoryTarget {
                    scope_type,
                    scope_id,
                    memory_kind,
                    subject_id,
                }
                .clean()?,
            });
        }
        Ok(targets)
    }

    fn consolidate_target(
        &self,
        target: &MemoryTarget,
        limits: ConsolidationLimits,
        now_epoch: i64,
    ) -> Result<ConsolidationRunStats, MemoryError> {
        let target = target.clean()?;
        let mut conn = self.connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MemoryError::from_sql)?;
        let subject_key = target.subject_id().unwrap_or("");
        let checkpoint = tx
            .query_row(
                "SELECT last_processed_row_id, last_run_at_epoch, truncated
                   FROM memory_consolidation_state
                  WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
                    AND subject_key = ?4",
                params![
                    target.scope_type().as_str(),
                    target.scope_id(),
                    target.memory_kind().as_str(),
                    subject_key,
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(MemoryError::from_sql)?
            .unwrap_or((0, 0, false));
        let cutoff = now_epoch.saturating_sub(limits.min_interval_seconds as i64);
        if checkpoint.1 > cutoff {
            tx.commit().map_err(MemoryError::from_sql)?;
            return Ok(ConsolidationRunStats::default());
        }

        let (new_count, source_count) = new_record_stats(&tx, &target, checkpoint.0)?;
        // 一旦某个 target 因数量或字符上限截断，后续轮次必须继续排空该批积压；
        // 否则最后不足 min_new_records 的尾批会永久停在检查点之后。
        if new_count == 0
            || (!checkpoint.2
                && (new_count < limits.min_new_records
                    || source_count < limits.min_distinct_sources))
        {
            tx.commit().map_err(MemoryError::from_sql)?;
            return Ok(ConsolidationRunStats::default());
        }

        let records = load_consolidation_records(&tx, &target, checkpoint.0, limits)?;
        let Some(processed_row_id) = records.last().map(|record| record.row_id) else {
            tx.commit().map_err(MemoryError::from_sql)?;
            return Ok(ConsolidationRunStats::default());
        };
        let truncated = records.len() < new_count;
        let duplicate_ids = exact_duplicate_ids(&records);
        let now = qq_maid_common::time_context::now_iso_cn();
        for id in &duplicate_ids {
            tx.execute(
                "UPDATE memories
                    SET status = 'archived', updated_at = ?1
                  WHERE id = ?2 AND status = 'active'
                    AND scope_type = ?3 AND scope_id = ?4 AND memory_kind = ?5
                    AND subject_id IS ?6",
                params![
                    now,
                    id,
                    target.scope_type().as_str(),
                    target.scope_id(),
                    target.memory_kind().as_str(),
                    target.subject_id(),
                ],
            )
            .map_err(MemoryError::from_sql)?;
        }
        let output_count = records.len().saturating_sub(duplicate_ids.len());
        tx.execute(
            "INSERT INTO memory_consolidation_state (
                 scope_type, scope_id, memory_kind, subject_key,
                 last_processed_row_id, last_run_at_epoch, last_status,
                 input_count, output_count, duplicate_count, conflict_count, truncated
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'success', ?7, ?8, ?9, 0, ?10)
             ON CONFLICT(scope_type, scope_id, memory_kind, subject_key) DO UPDATE SET
                 last_processed_row_id = excluded.last_processed_row_id,
                 last_run_at_epoch = excluded.last_run_at_epoch,
                 last_status = excluded.last_status,
                 input_count = excluded.input_count,
                 output_count = excluded.output_count,
                 duplicate_count = excluded.duplicate_count,
                 conflict_count = excluded.conflict_count,
                 truncated = excluded.truncated",
            params![
                target.scope_type().as_str(),
                target.scope_id(),
                target.memory_kind().as_str(),
                subject_key,
                processed_row_id,
                now_epoch,
                records.len() as i64,
                output_count as i64,
                duplicate_ids.len() as i64,
                i64::from(truncated),
            ],
        )
        .map_err(MemoryError::from_sql)?;
        tx.commit().map_err(MemoryError::from_sql)?;

        Ok(ConsolidationRunStats {
            candidate_target_count: 1,
            processed_target_count: 1,
            input_record_count: records.len(),
            output_record_count: output_count,
            archived_duplicate_count: duplicate_ids.len(),
            conflict_count: 0,
            truncated_target_count: usize::from(truncated),
        })
    }
}

fn new_record_stats(
    tx: &Transaction<'_>,
    target: &MemoryTarget,
    last_processed_row_id: i64,
) -> Result<(usize, usize), MemoryError> {
    tx.query_row(
        "SELECT count(*), count(DISTINCT CASE
                    WHEN source_ref IS NOT NULL AND trim(source_ref) <> ''
                    THEN source_ref END)
           FROM memories
          WHERE status = 'active' AND scope_type = ?1 AND scope_id = ?2
            AND memory_kind = ?3 AND subject_id IS ?4 AND row_id > ?5",
        params![
            target.scope_type().as_str(),
            target.scope_id(),
            target.memory_kind().as_str(),
            target.subject_id(),
            last_processed_row_id,
        ],
        |row| {
            Ok((
                row.get::<_, i64>(0)? as usize,
                row.get::<_, i64>(1)? as usize,
            ))
        },
    )
    .map_err(MemoryError::from_sql)
}

fn load_consolidation_records(
    tx: &Transaction<'_>,
    target: &MemoryTarget,
    last_processed_row_id: i64,
    limits: ConsolidationLimits,
) -> Result<Vec<RecordWithRowId>, MemoryError> {
    let mut stmt = tx
        .prepare(
            "SELECT id, created_at, updated_at, memory_type, scope,
                    scope_type, scope_id, created_by_user_id,
                    user_id, group_id, content, source_text,
                    memory_kind, subject_id, relation_subject_id, relation_object_id,
                    visibility, source_type, source_ref, last_confirmed_at,
                    status, pinned, attribute_key, row_id
               FROM memories
              WHERE status = 'active' AND scope_type = ?1 AND scope_id = ?2
                AND memory_kind = ?3 AND subject_id IS ?4 AND row_id > ?5
           ORDER BY row_id ASC
              LIMIT ?6",
        )
        .map_err(MemoryError::from_sql)?;
    let rows = stmt
        .query_map(
            params![
                target.scope_type().as_str(),
                target.scope_id(),
                target.memory_kind().as_str(),
                target.subject_id(),
                last_processed_row_id,
                limits.max_records.max(1) as i64,
            ],
            |row| {
                Ok(RecordWithRowId {
                    row_id: row.get(23)?,
                    record: memory_from_row(row)?,
                })
            },
        )
        .map_err(MemoryError::from_sql)?;
    let loaded = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from_sql)?;
    let mut records = Vec::new();
    let mut used_chars = 0usize;
    for record in loaded {
        let next_chars = record.record.content.chars().count();
        if !records.is_empty() && used_chars.saturating_add(next_chars) > limits.max_input_chars {
            break;
        }
        used_chars = used_chars.saturating_add(next_chars);
        records.push(record);
    }
    Ok(records)
}

fn exact_duplicate_ids(records: &[RecordWithRowId]) -> Vec<String> {
    // 这里只比较本轮有界批次；检查点表示“已扫描”，不宣称整个 target 已跨批去重。
    let mut groups = HashMap::<String, Vec<&RecordWithRowId>>::new();
    for record in records {
        groups
            .entry(semantic_duplicate_key(&record.record))
            .or_default()
            .push(record);
    }
    let mut duplicates = Vec::new();
    for group in groups.values().filter(|group| group.len() > 1) {
        let canonical = group
            .iter()
            .max_by_key(|record| canonical_rank(record))
            .expect("duplicate group is non-empty");
        duplicates.extend(
            group
                .iter()
                .filter(|record| record.record.id != canonical.record.id)
                .map(|record| record.record.id.clone()),
        );
    }
    duplicates
}

fn semantic_duplicate_key(record: &MemoryRecord) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        normalize_text(&record.content),
        record.memory_type,
        record.visibility.as_str(),
        record.attribute_key.as_deref().unwrap_or(""),
        record.relation_subject_id.as_deref().unwrap_or(""),
        record.relation_object_id.as_deref().unwrap_or("")
    )
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn canonical_rank(record: &&RecordWithRowId) -> (bool, u8, bool, i64) {
    let source_priority = match record.record.source_type {
        MemorySourceType::UserConfirmed => 4,
        MemorySourceType::ManualImport => 3,
        MemorySourceType::Legacy => 2,
        MemorySourceType::SystemDerived => 1,
    };
    (
        record.record.pinned,
        source_priority,
        record.record.last_confirmed_at.is_some(),
        record.row_id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::tools::memory::{
            MemoryActor, MemoryOperations, SaveMemoryRequest,
            storage::{
                MEMORY_MIGRATIONS, MemoryCategory, MemoryQuery, MemoryStatus, MemoryVisibility,
            },
        },
        storage::database::SqliteDatabase,
    };

    fn database() -> SqliteDatabase {
        SqliteDatabase::open_temp("memory-consolidation", MEMORY_MIGRATIONS).unwrap()
    }

    fn save(
        store: &MemoryStore,
        target: MemoryTarget,
        actor: MemoryActor,
        content: &str,
        source_ref: &str,
    ) {
        save_with_source_ref(store, target, actor, content, Some(source_ref));
    }

    fn save_with_source_ref(
        store: &MemoryStore,
        target: MemoryTarget,
        actor: MemoryActor,
        content: &str,
        source_ref: Option<&str>,
    ) {
        let visibility = match target.memory_kind() {
            MemoryKind::Personal => MemoryVisibility::Private,
            MemoryKind::GroupProfile => MemoryVisibility::ContextOnly,
            MemoryKind::Group => MemoryVisibility::GroupMembers,
            MemoryKind::LegacyUnassigned => MemoryVisibility::Private,
        };
        MemoryOperations::new(store.clone())
            .save(SaveMemoryRequest {
                actor,
                target,
                content: content.to_owned(),
                source_text: content.to_owned(),
                category: MemoryCategory::Note,
                legacy_scope: "general".to_owned(),
                visibility,
                source_type: MemorySourceType::UserConfirmed,
                source_ref: source_ref.map(str::to_owned),
                confirmed_at: None,
                pinned: false,
                attribute_key: None,
                relation_subject_id: None,
                relation_object_id: None,
            })
            .unwrap();
    }

    fn target_row_ids(database: &SqliteDatabase, target: &MemoryTarget) -> Vec<i64> {
        let conn = database.connection().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT row_id FROM memories
                  WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
                    AND subject_id IS ?4
               ORDER BY row_id ASC",
            )
            .unwrap();
        stmt.query_map(
            params![
                target.scope_type().as_str(),
                target.scope_id(),
                target.memory_kind().as_str(),
                target.subject_id(),
            ],
            |row| row.get(0),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    }

    fn checkpoint(database: &SqliteDatabase, target: &MemoryTarget) -> (i64, bool) {
        database
            .connection()
            .unwrap()
            .query_row(
                "SELECT last_processed_row_id, truncated
                   FROM memory_consolidation_state
                  WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
                    AND subject_key = ?4",
                params![
                    target.scope_type().as_str(),
                    target.scope_id(),
                    target.memory_kind().as_str(),
                    target.subject_id().unwrap_or(""),
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    fn actor(scope_id: &str) -> MemoryActor {
        MemoryActor {
            user_id: scope_id.to_owned(),
            personal_scope_id: scope_id.to_owned(),
            group_scope_id: None,
            can_manage_group_memory: false,
        }
    }

    fn group_actor(personal_scope_id: &str, group_scope_id: &str, admin: bool) -> MemoryActor {
        MemoryActor {
            user_id: personal_scope_id.to_owned(),
            personal_scope_id: personal_scope_id.to_owned(),
            group_scope_id: Some(group_scope_id.to_owned()),
            can_manage_group_memory: admin,
        }
    }

    fn limits() -> ConsolidationLimits {
        ConsolidationLimits {
            min_interval_seconds: 3600,
            min_new_records: 2,
            min_distinct_sources: 2,
            max_records: 100,
            max_input_chars: 32_000,
        }
    }

    #[test]
    fn exact_duplicates_are_archived_with_history_preserved() {
        let store = MemoryStore::new(database());
        save(
            &store,
            MemoryTarget::personal("u1"),
            actor("u1"),
            "喜欢 Rust",
            "tool:a",
        );
        save(
            &store,
            MemoryTarget::personal("u1"),
            actor("u1"),
            " 喜欢   Rust ",
            "tool:b",
        );

        let stats = store.consolidate_due(limits(), 10_000).unwrap();
        assert_eq!(stats.archived_duplicate_count, 1);
        let operations = MemoryOperations::new(store);
        let active = operations
            .list(
                &actor("u1"),
                MemoryQuery::active(MemoryTarget::personal("u1")),
            )
            .unwrap();
        let mut archived_query = MemoryQuery::active(MemoryTarget::personal("u1"));
        archived_query.status = Some(MemoryStatus::Archived);
        let archived = operations.list(&actor("u1"), archived_query).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(archived.len(), 1);
    }

    #[test]
    fn max_records_checkpoint_advances_only_through_scanned_batches() {
        let database = database();
        let store = MemoryStore::new(database.clone());
        let target = MemoryTarget::personal("u1");
        for index in 0..5 {
            save(
                &store,
                target.clone(),
                actor("u1"),
                &format!("批记录 {index}"),
                &format!("tool:{index}"),
            );
        }
        let row_ids = target_row_ids(&database, &target);
        let mut batch_limits = limits();
        batch_limits.max_records = 2;

        let first = store.consolidate_due(batch_limits, 10_000).unwrap();
        assert_eq!(first.input_record_count, 2);
        assert_eq!(first.truncated_target_count, 1);
        assert_eq!(checkpoint(&database, &target), (row_ids[1], true));

        let second = store.consolidate_due(batch_limits, 13_600).unwrap();
        assert_eq!(second.input_record_count, 2);
        assert_eq!(second.truncated_target_count, 1);
        assert_eq!(checkpoint(&database, &target), (row_ids[3], true));

        let third = store.consolidate_due(batch_limits, 17_200).unwrap();
        assert_eq!(third.input_record_count, 1);
        assert_eq!(third.truncated_target_count, 0);
        assert_eq!(checkpoint(&database, &target), (row_ids[4], false));
    }

    #[test]
    fn max_input_chars_checkpoint_continues_with_unscanned_records() {
        let database = database();
        let store = MemoryStore::new(database.clone());
        let target = MemoryTarget::personal("u1");
        for (content, source_ref) in [("aaaa", "tool:a"), ("bbbb", "tool:b"), ("cccc", "tool:c")] {
            save(&store, target.clone(), actor("u1"), content, source_ref);
        }
        let row_ids = target_row_ids(&database, &target);
        let mut batch_limits = limits();
        batch_limits.max_input_chars = 5;

        for (now_epoch, expected_row_id, expected_truncated) in [
            (10_000, row_ids[0], true),
            (13_600, row_ids[1], true),
            (17_200, row_ids[2], false),
        ] {
            let stats = store.consolidate_due(batch_limits, now_epoch).unwrap();
            assert_eq!(stats.input_record_count, 1);
            assert_eq!(
                checkpoint(&database, &target),
                (expected_row_id, expected_truncated)
            );
        }
    }

    #[test]
    fn null_source_refs_do_not_satisfy_distinct_safe_source_gate() {
        let database = database();
        let store = MemoryStore::new(database.clone());
        let target = MemoryTarget::personal("u1");
        for index in 0..3 {
            save_with_source_ref(
                &store,
                target.clone(),
                actor("u1"),
                &format!("无来源记录 {index}"),
                None,
            );
        }

        let stats = store.consolidate_due(limits(), 10_000).unwrap();
        assert_eq!(stats.candidate_target_count, 0);
        let checkpoint_count: i64 = database
            .connection()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM memory_consolidation_state",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checkpoint_count, 0);
    }

    #[test]
    fn identical_content_in_different_scopes_is_never_merged() {
        let store = MemoryStore::new(database());
        for scope in ["u1", "u2"] {
            save(
                &store,
                MemoryTarget::personal(scope),
                actor(scope),
                "共同文本",
                &format!("tool:{scope}:a"),
            );
            save(
                &store,
                MemoryTarget::personal(scope),
                actor(scope),
                "另一条记录",
                &format!("tool:{scope}:b"),
            );
        }

        let stats = store.consolidate_due(limits(), 10_000).unwrap();
        assert_eq!(stats.archived_duplicate_count, 0);
        for scope in ["u1", "u2"] {
            let records = MemoryOperations::new(store.clone())
                .list(
                    &actor(scope),
                    MemoryQuery::active(MemoryTarget::personal(scope)),
                )
                .unwrap();
            assert_eq!(records.len(), 2);
        }
    }

    #[test]
    fn identical_content_in_different_groups_is_never_merged() {
        let store = MemoryStore::new(database());
        for group in ["group-a", "group-b"] {
            let actor = group_actor("admin", group, true);
            save(
                &store,
                MemoryTarget::group(group),
                actor.clone(),
                "相同群规则",
                &format!("tool:{group}:a"),
            );
            save(
                &store,
                MemoryTarget::group(group),
                actor,
                "独立群规则",
                &format!("tool:{group}:b"),
            );
        }

        let stats = store.consolidate_due(limits(), 10_000).unwrap();
        assert_eq!(stats.archived_duplicate_count, 0);
        for group in ["group-a", "group-b"] {
            let actor = group_actor("admin", group, true);
            let records = MemoryOperations::new(store.clone())
                .list(&actor, MemoryQuery::active(MemoryTarget::group(group)))
                .unwrap();
            assert_eq!(records.len(), 2);
        }
    }

    #[test]
    fn opted_out_group_profile_is_not_consolidated() {
        let store = MemoryStore::new(database());
        let actor = group_actor("member", "group-a", false);
        let target = MemoryTarget::group_profile("group-a", "member");
        save(&store, target.clone(), actor.clone(), "画像内容", "tool:a");
        save(&store, target.clone(), actor.clone(), "画像内容", "tool:b");
        let result = MemoryOperations::new(store.clone())
            .set_group_profile_enabled(&actor, &target, false)
            .unwrap();
        assert_eq!(result.archived_ids.len(), 2);

        let stats = store.consolidate_due(limits(), 10_000).unwrap();
        assert_eq!(stats.candidate_target_count, 0);
        assert_eq!(stats.archived_duplicate_count, 0);
    }

    #[test]
    fn concurrent_cycles_archive_each_duplicate_only_once() {
        let store = MemoryStore::new(database());
        save(
            &store,
            MemoryTarget::personal("u1"),
            actor("u1"),
            "并发重复",
            "tool:a",
        );
        save(
            &store,
            MemoryTarget::personal("u1"),
            actor("u1"),
            "并发重复",
            "tool:b",
        );
        let first = store.clone();
        let second = store.clone();
        let first = std::thread::spawn(move || first.consolidate_due(limits(), 10_000).unwrap());
        let second = std::thread::spawn(move || second.consolidate_due(limits(), 10_000).unwrap());
        let archived = first.join().unwrap().archived_duplicate_count
            + second.join().unwrap().archived_duplicate_count;

        assert_eq!(archived, 1);
        let records = MemoryOperations::new(store)
            .list(
                &actor("u1"),
                MemoryQuery::active(MemoryTarget::personal("u1")),
            )
            .unwrap();
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn failed_archive_rolls_back_records_and_checkpoint() {
        let database = database();
        let store = MemoryStore::new(database.clone());
        save(
            &store,
            MemoryTarget::personal("u1"),
            actor("u1"),
            "重复",
            "tool:a",
        );
        save(
            &store,
            MemoryTarget::personal("u1"),
            actor("u1"),
            "重复",
            "tool:b",
        );
        database
            .connection()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_memory_archive
                 BEFORE UPDATE OF status ON memories
                 WHEN NEW.status = 'archived'
                 BEGIN SELECT RAISE(ABORT, 'archive failed'); END;",
            )
            .unwrap();

        assert!(store.consolidate_due(limits(), 10_000).is_err());
        let records = MemoryOperations::new(store)
            .list(
                &actor("u1"),
                MemoryQuery::active(MemoryTarget::personal("u1")),
            )
            .unwrap();
        assert_eq!(records.len(), 2);
        let checkpoint_count: i64 = database
            .connection()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM memory_consolidation_state",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checkpoint_count, 0);
    }
}

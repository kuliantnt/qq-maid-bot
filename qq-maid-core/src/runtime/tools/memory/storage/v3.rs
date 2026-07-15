//! Memory v3 storage 底层持久化与事务边界。
//!
//! 本模块不判断操作者权限；领域层先完成授权，再调用这里的原子写入、归档、清空和
//! opt-out。storage 只保证精确 target、冲突归档和偏好变更的一致性。

use qq_maid_common::{redaction::redact_sensitive_text, time_context::now_iso_cn};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use uuid::Uuid;

use super::{
    MemoryCategory, MemoryError, MemoryKind, MemoryRecord, MemorySourceType, MemoryStatus,
    MemoryStore, MemoryTarget, PersistMemoryRequest, PersistMemoryResult,
    clean::{
        clean_attribute_key, clean_optional, clean_optional_option, clean_required,
        clean_source_ref, clean_stable_identity, default_scope,
    },
    insert_record_unlocked,
};

impl MemoryStore {
    pub(crate) fn list_active_ids_v3(
        &self,
        target: &MemoryTarget,
    ) -> Result<Vec<String>, MemoryError> {
        let target = target.clean()?;
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(MemoryError::from_sql)?;
        let ids = list_ids_for_target_unlocked(&tx, &target, MemoryStatus::Active)?;
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok(ids)
    }

    pub(crate) fn group_profile_snapshot_v3(
        &self,
        target: &MemoryTarget,
    ) -> Result<(bool, Vec<String>), MemoryError> {
        let target = target.clean()?;
        if target.memory_kind != MemoryKind::GroupProfile {
            return Err(MemoryError::bad_request(
                "profile preference requires a group profile target",
            ));
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(MemoryError::from_sql)?;
        let enabled = profile_enabled_for_target_unlocked(&tx, &target)?;
        let ids = list_ids_for_target_unlocked(&tx, &target, MemoryStatus::Active)?;
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok((enabled, ids))
    }
    /// 原子写入 v3 记忆；有属性键时先归档同一冲突键的 active 记录。
    pub(crate) fn persist_v3(
        &self,
        req: PersistMemoryRequest,
    ) -> Result<PersistMemoryResult, MemoryError> {
        let record = build_v3_record(req)?;
        let mut conn = self.connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MemoryError::from_sql)?;
        ensure_profile_enabled_unlocked(&tx, &record)?;
        let archived_ids = archive_conflicts_unlocked(&tx, &record, None)?;
        insert_record_unlocked(&tx, &record)?;
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok(PersistMemoryResult {
            record,
            archived_ids,
        })
    }

    /// 原子替换 v3 记录。旧记录及同冲突键记录保留为 archived，新记录使用新 ID。
    pub(crate) fn replace_v3(
        &self,
        target: &MemoryTarget,
        id: &str,
        req: PersistMemoryRequest,
    ) -> Result<PersistMemoryResult, MemoryError> {
        let target = target.clean()?;
        let record = build_v3_record(req)?;
        if record_target(&record) != target {
            return Err(MemoryError::bad_request("replacement target mismatch"));
        }
        let mut conn = self.connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MemoryError::from_sql)?;
        ensure_profile_enabled_unlocked(&tx, &record)?;
        if !memory_exists_for_target_unlocked(&tx, &target, id, MemoryStatus::Active)? {
            return Err(MemoryError::not_found("memory not found"));
        }
        let mut archived_ids = archive_conflicts_unlocked(&tx, &record, Some(id))?;
        if archive_id_for_target_unlocked(&tx, &target, id)? == 0 {
            return Err(MemoryError::not_found("memory not found"));
        }
        if !archived_ids.iter().any(|archived| archived == id) {
            archived_ids.push(id.to_owned());
        }
        insert_record_unlocked(&tx, &record)?;
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok(PersistMemoryResult {
            record,
            archived_ids,
        })
    }

    pub(crate) fn archive_v3(
        &self,
        target: &MemoryTarget,
        id: &str,
    ) -> Result<String, MemoryError> {
        let target = target.clean()?;
        let mut conn = self.connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MemoryError::from_sql)?;
        if archive_id_for_target_unlocked(&tx, &target, id)? == 0 {
            return Err(MemoryError::not_found("memory not found"));
        }
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok(id.to_owned())
    }

    pub(crate) fn clear_v3(&self, target: &MemoryTarget) -> Result<Vec<String>, MemoryError> {
        let target = target.clean()?;
        let mut conn = self.connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MemoryError::from_sql)?;
        let ids = list_ids_for_target_unlocked(&tx, &target, MemoryStatus::Active)?;
        archive_target_unlocked(&tx, &target)?;
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok(ids)
    }

    /// 对准备阶段的 active ID 集合做 compare-and-archive，避免确认期间新旧对象漂移。
    pub(crate) fn clear_v3_if_unchanged(
        &self,
        target: &MemoryTarget,
        expected_ids: &[String],
    ) -> Result<Vec<String>, MemoryError> {
        let target = target.clean()?;
        let mut conn = self.connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MemoryError::from_sql)?;
        let ids = list_ids_for_target_unlocked(&tx, &target, MemoryStatus::Active)?;
        if ids != expected_ids {
            return Err(MemoryError::bad_request(
                "memory target changed after confirmation was prepared",
            ));
        }
        archive_target_unlocked(&tx, &target)?;
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok(ids)
    }

    /// 持久化群画像 opt-in/opt-out。关闭时与现有画像归档在同一事务内完成。
    pub(crate) fn set_group_profile_enabled(
        &self,
        target: &MemoryTarget,
        enabled: bool,
    ) -> Result<Vec<String>, MemoryError> {
        let target = target.clean()?;
        if target.memory_kind != MemoryKind::GroupProfile {
            return Err(MemoryError::bad_request(
                "profile preference requires a group profile target",
            ));
        }
        let subject_id = target
            .subject_id
            .as_deref()
            .ok_or_else(|| MemoryError::bad_request("subject_id is required"))?;
        let now = now_iso_cn();
        let mut conn = self.connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MemoryError::from_sql)?;
        tx.execute(
            "INSERT INTO memory_profile_preferences (
                group_scope_id, subject_id, profile_enabled, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(group_scope_id, subject_id) DO UPDATE SET
                profile_enabled = excluded.profile_enabled,
                updated_at = excluded.updated_at",
            params![target.scope_id, subject_id, enabled, now],
        )
        .map_err(MemoryError::from_sql)?;
        let archived_ids = if enabled {
            Vec::new()
        } else {
            let ids = list_ids_for_target_unlocked(&tx, &target, MemoryStatus::Active)?;
            archive_target_unlocked(&tx, &target)?;
            ids
        };
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok(archived_ids)
    }

    pub(crate) fn set_group_profile_enabled_if_unchanged(
        &self,
        target: &MemoryTarget,
        enabled: bool,
        expected_enabled: bool,
        expected_ids: &[String],
    ) -> Result<Vec<String>, MemoryError> {
        let target = target.clean()?;
        if target.memory_kind != MemoryKind::GroupProfile {
            return Err(MemoryError::bad_request(
                "profile preference requires a group profile target",
            ));
        }
        let subject_id = target
            .subject_id
            .as_deref()
            .ok_or_else(|| MemoryError::bad_request("subject_id is required"))?;
        let now = now_iso_cn();
        let mut conn = self.connection()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(MemoryError::from_sql)?;
        if profile_enabled_for_target_unlocked(&tx, &target)? != expected_enabled {
            return Err(MemoryError::bad_request(
                "profile preference changed after confirmation was prepared",
            ));
        }
        let ids = list_ids_for_target_unlocked(&tx, &target, MemoryStatus::Active)?;
        if ids != expected_ids {
            return Err(MemoryError::bad_request(
                "memory target changed after confirmation was prepared",
            ));
        }
        tx.execute(
            "INSERT INTO memory_profile_preferences (
                group_scope_id, subject_id, profile_enabled, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(group_scope_id, subject_id) DO UPDATE SET
                profile_enabled = excluded.profile_enabled,
                updated_at = excluded.updated_at",
            params![target.scope_id, subject_id, enabled, now],
        )
        .map_err(MemoryError::from_sql)?;
        let archived_ids = if enabled {
            Vec::new()
        } else {
            archive_target_unlocked(&tx, &target)?;
            ids
        };
        tx.commit().map_err(MemoryError::from_sql)?;
        Ok(archived_ids)
    }

    /// 物理删除仅供领域层在明确 delete 语义下调用。
    pub(crate) fn delete_v3(&self, target: &MemoryTarget, id: &str) -> Result<String, MemoryError> {
        let target = target.clean()?;
        let conn = self.connection()?;
        let changed = conn
            .execute(
                "DELETE FROM memories
                 WHERE id = ?1 AND scope_type = ?2 AND scope_id = ?3
                   AND status = 'active'
                   AND memory_kind = ?4 AND subject_id IS ?5",
                params![
                    id,
                    target.scope_type.as_str(),
                    target.scope_id,
                    target.memory_kind.as_str(),
                    target.subject_id
                ],
            )
            .map_err(MemoryError::from_sql)?;
        if changed == 0 {
            return Err(MemoryError::not_found("memory not found"));
        }
        Ok(id.to_owned())
    }
}

fn record_target(record: &MemoryRecord) -> MemoryTarget {
    MemoryTarget {
        scope_type: record
            .scope_type
            .parse()
            .unwrap_or(super::MemoryScopeType::LegacyUnassigned),
        scope_id: record.scope_id.clone().unwrap_or_default(),
        memory_kind: record.memory_kind,
        subject_id: record.subject_id.clone(),
    }
}

fn build_v3_record(req: PersistMemoryRequest) -> Result<MemoryRecord, MemoryError> {
    let target = req.target.clean()?;
    let now = now_iso_cn();
    let created_by_user_id = clean_required(req.created_by_user_id, "created_by_user_id")?;
    let content = clean_required(req.content, "content")?;
    let attribute_key = clean_attribute_key(req.attribute_key)?;
    let source_ref = clean_source_ref(req.source_ref)?;
    let relation_subject_id =
        clean_stable_identity(req.relation_subject_id, "relation_subject_id")?;
    let relation_object_id = clean_stable_identity(req.relation_object_id, "relation_object_id")?;
    if req.category != MemoryCategory::Relation
        && (relation_subject_id.is_some() || relation_object_id.is_some())
    {
        return Err(MemoryError::bad_request(
            "relation identities require relation category",
        ));
    }
    let confirmed_at = clean_optional_option(req.confirmed_at)
        .or_else(|| (req.source_type == MemorySourceType::UserConfirmed).then(|| now.clone()));
    Ok(MemoryRecord {
        id: Uuid::new_v4().to_string(),
        ts: now.clone(),
        created_at: now,
        updated_at: None,
        memory_type: req.category.as_str().to_owned(),
        scope: clean_optional(req.legacy_scope).unwrap_or_else(default_scope),
        scope_type: target.scope_type.as_str().to_owned(),
        scope_id: Some(target.scope_id),
        created_by_user_id: Some(created_by_user_id),
        memory_kind: target.memory_kind,
        subject_id: target.subject_id,
        relation_subject_id,
        relation_object_id,
        visibility: req.visibility,
        source_type: req.source_type,
        source_ref,
        last_confirmed_at: confirmed_at,
        status: MemoryStatus::Active,
        pinned: req.pinned,
        attribute_key,
        user_id: None,
        group_id: None,
        content: redact_sensitive_text(&content),
        source_text: redact_sensitive_text(&req.source_text),
    })
}

fn ensure_profile_enabled_unlocked(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
) -> Result<(), MemoryError> {
    if record.memory_kind != MemoryKind::GroupProfile {
        return Ok(());
    }
    let enabled = profile_enabled_for_target_unlocked(tx, &record_target(record))?;
    if enabled {
        Ok(())
    } else {
        Err(MemoryError::profile_opted_out())
    }
}

fn profile_enabled_for_target_unlocked(
    tx: &Transaction<'_>,
    target: &MemoryTarget,
) -> Result<bool, MemoryError> {
    tx.query_row(
        "SELECT profile_enabled FROM memory_profile_preferences
         WHERE group_scope_id = ?1 AND subject_id = ?2",
        params![target.scope_id, target.subject_id],
        |row| row.get::<_, bool>(0),
    )
    .optional()
    .map(|value| value.unwrap_or(true))
    .map_err(MemoryError::from_sql)
}

fn archive_conflicts_unlocked(
    tx: &Transaction<'_>,
    record: &MemoryRecord,
    exclude_id: Option<&str>,
) -> Result<Vec<String>, MemoryError> {
    let Some(attribute_key) = record.attribute_key.as_deref() else {
        return Ok(Vec::new());
    };
    let exclude_id = exclude_id.unwrap_or("");
    let mut stmt = tx
        .prepare(
            "SELECT id FROM memories
             WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
               AND subject_id IS ?4
               AND relation_subject_id IS ?5 AND relation_object_id IS ?6
               AND attribute_key = ?7 AND status = 'active' AND id <> ?8
             ORDER BY row_id DESC",
        )
        .map_err(MemoryError::from_sql)?;
    let ids = stmt
        .query_map(
            params![
                record.scope_type,
                record.scope_id,
                record.memory_kind.as_str(),
                record.subject_id,
                record.relation_subject_id,
                record.relation_object_id,
                attribute_key,
                exclude_id,
            ],
            |row| row.get::<_, String>(0),
        )
        .map_err(MemoryError::from_sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from_sql)?;
    drop(stmt);
    tx.execute(
        "UPDATE memories SET status = 'archived', updated_at = ?1
         WHERE scope_type = ?2 AND scope_id = ?3 AND memory_kind = ?4
           AND subject_id IS ?5
           AND relation_subject_id IS ?6 AND relation_object_id IS ?7
           AND attribute_key = ?8 AND status = 'active' AND id <> ?9",
        params![
            now_iso_cn(),
            record.scope_type,
            record.scope_id,
            record.memory_kind.as_str(),
            record.subject_id,
            record.relation_subject_id,
            record.relation_object_id,
            attribute_key,
            exclude_id,
        ],
    )
    .map_err(MemoryError::from_sql)?;
    Ok(ids)
}

fn memory_exists_for_target_unlocked(
    tx: &Transaction<'_>,
    target: &MemoryTarget,
    id: &str,
    status: MemoryStatus,
) -> Result<bool, MemoryError> {
    tx.query_row(
        "SELECT 1 FROM memories
         WHERE id = ?1 AND scope_type = ?2 AND scope_id = ?3
           AND memory_kind = ?4 AND subject_id IS ?5 AND status = ?6",
        params![
            id,
            target.scope_type.as_str(),
            target.scope_id,
            target.memory_kind.as_str(),
            target.subject_id,
            status.as_str(),
        ],
        |_| Ok(()),
    )
    .optional()
    .map(|value| value.is_some())
    .map_err(MemoryError::from_sql)
}

fn list_ids_for_target_unlocked(
    tx: &Transaction<'_>,
    target: &MemoryTarget,
    status: MemoryStatus,
) -> Result<Vec<String>, MemoryError> {
    let mut stmt = tx
        .prepare(
            "SELECT id FROM memories
             WHERE scope_type = ?1 AND scope_id = ?2 AND memory_kind = ?3
               AND subject_id IS ?4 AND status = ?5 ORDER BY row_id DESC",
        )
        .map_err(MemoryError::from_sql)?;
    let rows = stmt
        .query_map(
            params![
                target.scope_type.as_str(),
                target.scope_id,
                target.memory_kind.as_str(),
                target.subject_id,
                status.as_str(),
            ],
            |row| row.get::<_, String>(0),
        )
        .map_err(MemoryError::from_sql)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(MemoryError::from_sql)
}

fn archive_target_unlocked(
    tx: &Transaction<'_>,
    target: &MemoryTarget,
) -> Result<usize, MemoryError> {
    tx.execute(
        "UPDATE memories SET status = 'archived', updated_at = ?1
         WHERE scope_type = ?2 AND scope_id = ?3 AND memory_kind = ?4
           AND subject_id IS ?5 AND status = 'active'",
        params![
            now_iso_cn(),
            target.scope_type.as_str(),
            target.scope_id,
            target.memory_kind.as_str(),
            target.subject_id,
        ],
    )
    .map_err(MemoryError::from_sql)
}

fn archive_id_for_target_unlocked(
    tx: &Transaction<'_>,
    target: &MemoryTarget,
    id: &str,
) -> Result<usize, MemoryError> {
    tx.execute(
        "UPDATE memories SET status = 'archived', updated_at = ?1
         WHERE id = ?2 AND scope_type = ?3 AND scope_id = ?4 AND memory_kind = ?5
           AND subject_id IS ?6 AND status = 'active'",
        params![
            now_iso_cn(),
            id,
            target.scope_type.as_str(),
            target.scope_id,
            target.memory_kind.as_str(),
            target.subject_id,
        ],
    )
    .map_err(MemoryError::from_sql)
}
